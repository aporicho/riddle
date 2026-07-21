//! Persistent recurring commands for MagicPaper's due-time scheduler.
//!
//! A handwritten transcription such as
//! `任务 每五分钟讲一个黑暗冷笑话` becomes a local task.  The task list is
//! deliberately separate from page memories: it is small, explicit, survives
//! restarts, and can be checked without spending an oracle request.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

mod formatting;
mod parser;

pub use formatting::heartbeat_prompt;
use formatting::{describe_interval, describe_interval_zh, escape, unescape};

use parser::parse_command;

const MIN_INTERVAL_SECS: u64 = 5 * 60;
const MAX_TASKS: usize = 9;

/// True only when the complete transcription is a valid local task command.
/// This deliberately does not treat every sentence beginning with `任务` as a
/// command: questions such as “任务管理有什么意义？” must still reach the oracle.
pub(crate) fn is_local_command(text: &str) -> bool {
    matches!(parse_command(text), Ok(Some(_)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: u64,
    pub interval_secs: u64,
    pub next_due: u64,
    pub instruction: String,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskChange {
    Added(Task),
    Deleted {
        number: usize,
        task: Task,
    },
    Paused {
        number: usize,
        task: Task,
    },
    Resumed {
        number: usize,
        task: Task,
    },
    Modified {
        number: usize,
        before: Task,
        after: Task,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskCommand {
    Add {
        interval_secs: u64,
        instruction: String,
    },
    Delete(usize),
    Pause(usize),
    Resume(usize),
    Modify {
        number: usize,
        interval_secs: u64,
        instruction: String,
    },
}

pub struct TaskStore {
    dir: PathBuf,
    pub entries: Vec<Task>,
}

/// Process-lifetime lease held by the screenless task agent. Its advisory lock
/// lets every UI mode (managed QTFB or legacy takeover) prove that it must not
/// run a second scheduler.
pub struct SchedulerLease(File);

impl Drop for SchedulerLease {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn task_dir() -> PathBuf {
    crate::runtime_env::persistent_path("RIDDLE_TASKS_DIR", "tasks", "/home/root/riddle-data/tasks")
}

fn scheduler_lock_file(dir: &Path) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("scheduler.lock"))
}

fn acquire_scheduler_lease_in(dir: &Path) -> io::Result<SchedulerLease> {
    let file = scheduler_lock_file(dir)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            if error.kind() == io::ErrorKind::WouldBlock {
                io::ErrorKind::AlreadyExists
            } else {
                error.kind()
            },
            format!("task scheduler lease unavailable: {error}"),
        ));
    }
    Ok(SchedulerLease(file))
}

pub fn acquire_scheduler_lease() -> io::Result<SchedulerLease> {
    acquire_scheduler_lease_in(&task_dir())
}

/// Fail closed: if ownership cannot be checked, the interactive UI must not
/// risk issuing duplicate scheduled API requests.
fn external_scheduler_active_in(dir: &Path) -> bool {
    let Ok(file) = scheduler_lock_file(dir) else {
        return true;
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return true;
    }
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
    false
}

pub fn external_scheduler_active() -> bool {
    external_scheduler_active_in(&task_dir())
}

struct TaskLock(File);

impl Drop for TaskLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl TaskStore {
    pub fn open() -> Option<Self> {
        match std::env::var("RIDDLE_TASKS").as_deref() {
            Ok("off") | Ok("0") | Ok("no") | Ok("false") => return None,
            _ => {}
        }
        let dir = task_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("magic-paper: tasks disabled ({}: {e})", dir.display());
            return None;
        }
        let mut store = Self {
            dir,
            entries: Vec::new(),
        };
        store.load();
        Some(store)
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.tsv")
    }

    fn lock_exclusive(&self) -> io::Result<TaskLock> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("index.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(TaskLock(file))
    }

    fn load(&mut self) {
        let Ok(_lock) = self.lock_exclusive() else {
            return;
        };
        self.load_unlocked();
    }

    fn load_unlocked(&mut self) {
        self.entries.clear();
        let Ok(text) = std::fs::read_to_string(self.index_path()) else {
            return;
        };
        for line in text.lines() {
            let mut cols = line.splitn(5, '\t');
            let (Some(id), Some(interval), Some(next_due), Some(state_or_instruction)) =
                (cols.next(), cols.next(), cols.next(), cols.next())
            else {
                continue;
            };
            let (Ok(id), Ok(interval_secs), Ok(next_due)) =
                (id.parse(), interval.parse(), next_due.parse())
            else {
                continue;
            };
            if interval_secs < MIN_INTERVAL_SECS {
                continue;
            }
            // MagicPaper 0.4.2 stored four columns. The fifth-column layout
            // adds state while treating every old task as active.
            let (paused, instruction) = match cols.next() {
                Some(instruction) => match state_or_instruction {
                    "active" => (false, instruction),
                    "paused" => (true, instruction),
                    _ => continue,
                },
                None => (false, state_or_instruction),
            };
            self.entries.push(Task {
                id,
                interval_secs,
                next_due,
                instruction: unescape(instruction),
                paused,
            });
            if self.entries.len() == MAX_TASKS {
                break;
            }
        }
    }

    fn persist(&self) -> io::Result<()> {
        let mut out = String::new();
        for task in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                task.id,
                task.interval_secs,
                task.next_due,
                if task.paused { "paused" } else { "active" },
                escape(&task.instruction)
            ));
        }
        let tmp = self.dir.join("index.tsv.new");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(out.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(tmp, self.index_path())
    }

    /// Apply a handwritten recurring-task command. Task numbers are the
    /// one-based numbers in the fresh catalog shown to the oracle.
    /// Non-task writing returns `Ok(None)`.
    pub fn apply_from_transcript(
        &mut self,
        text: &str,
        now: u64,
    ) -> Result<Option<TaskChange>, String> {
        let Some(command) = parse_command(text)? else {
            return Ok(None);
        };
        let _lock = self
            .lock_exclusive()
            .map_err(|error| format!("lock task store: {error}"))?;
        self.load_unlocked();
        let old_entries = self.entries.clone();
        let change = match command {
            TaskCommand::Add {
                interval_secs,
                instruction,
            } => {
                if self.entries.len() >= MAX_TASKS {
                    return Err(format!("at most {MAX_TASKS} tasks are allowed"));
                }
                let id = self
                    .entries
                    .iter()
                    .map(|t| t.id)
                    .max()
                    .map(|id| id.saturating_add(1))
                    .unwrap_or(now)
                    .max(now);
                let task = Task {
                    id,
                    interval_secs,
                    next_due: now.saturating_add(interval_secs),
                    instruction,
                    paused: false,
                };
                self.entries.push(task.clone());
                TaskChange::Added(task)
            }
            TaskCommand::Delete(number) => {
                let index = self.index(number)?;
                let task = self.entries.remove(index);
                TaskChange::Deleted { number, task }
            }
            TaskCommand::Pause(number) => {
                let index = self.index(number)?;
                if self.entries[index].paused {
                    return Err(format!("task {number} is already paused"));
                }
                self.entries[index].paused = true;
                TaskChange::Paused {
                    number,
                    task: self.entries[index].clone(),
                }
            }
            TaskCommand::Resume(number) => {
                let index = self.index(number)?;
                if !self.entries[index].paused {
                    return Err(format!("task {number} is already active"));
                }
                self.entries[index].paused = false;
                self.entries[index].next_due =
                    now.saturating_add(self.entries[index].interval_secs);
                TaskChange::Resumed {
                    number,
                    task: self.entries[index].clone(),
                }
            }
            TaskCommand::Modify {
                number,
                interval_secs,
                instruction,
            } => {
                let index = self.index(number)?;
                let before = self.entries[index].clone();
                self.entries[index].interval_secs = interval_secs;
                self.entries[index].instruction = instruction;
                self.entries[index].next_due = now.saturating_add(interval_secs);
                TaskChange::Modified {
                    number,
                    before,
                    after: self.entries[index].clone(),
                }
            }
        };
        if let Err(e) = self.persist() {
            self.entries = old_entries;
            return Err(format!("save task change: {e}"));
        }
        Ok(Some(change))
    }

    fn index(&self, number: usize) -> Result<usize, String> {
        if number == 0 || number > self.entries.len() {
            Err(format!(
                "task {number} does not exist (there are {})",
                self.entries.len()
            ))
        } else {
            Ok(number - 1)
        }
    }

    /// Delete directly from the local task panel, without an oracle turn.
    pub fn delete_number(&mut self, number: usize) -> Result<Task, String> {
        let _lock = self
            .lock_exclusive()
            .map_err(|error| format!("lock task store: {error}"))?;
        self.load_unlocked();
        let index = self.index(number)?;
        let task = self.entries.remove(index);
        if let Err(e) = self.persist() {
            self.entries.insert(index, task.clone());
            return Err(format!("save task deletion: {e}"));
        }
        Ok(task)
    }

    /// Toggle from the paper list. Enabling starts a fresh full interval so a
    /// task disabled for a while never fires immediately or replays backlog.
    pub fn toggle_number(&mut self, number: usize, now: u64) -> Result<Task, String> {
        let _lock = self
            .lock_exclusive()
            .map_err(|error| format!("lock task store: {error}"))?;
        self.load_unlocked();
        let index = self.index(number)?;
        let before = self.entries[index].clone();
        self.entries[index].paused = !self.entries[index].paused;
        if !self.entries[index].paused {
            self.entries[index].next_due = now.saturating_add(self.entries[index].interval_secs);
        }
        if let Err(error) = self.persist() {
            self.entries[index] = before;
            return Err(format!("save task toggle: {error}"));
        }
        Ok(self.entries[index].clone())
    }

    pub fn due(&self, now: u64) -> Vec<Task> {
        self.entries
            .iter()
            .filter(|t| !t.paused && t.next_due <= now)
            .cloned()
            .collect()
    }

    pub fn next_due(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter(|task| !task.paused)
            .map(|task| task.next_due)
            .min()
    }

    /// Atomically advance a completed due batch only if every task is still
    /// byte-for-byte the task that produced the request. A concurrent delete,
    /// pause, resume, modification, or another scheduler completion makes the
    /// batch stale, so its output must be discarded rather than resurrecting
    /// old state or advancing a replacement task.
    pub fn complete_due_if_unchanged(&mut self, expected: &[Task], now: u64) -> io::Result<bool> {
        if expected.is_empty() {
            return Ok(false);
        }
        let _lock = self.lock_exclusive()?;
        self.load_unlocked();
        if expected.iter().any(|snapshot| {
            self.entries
                .iter()
                .find(|task| task.id == snapshot.id)
                .is_none_or(|current| current != snapshot || current.paused)
        }) {
            return Ok(false);
        }
        for snapshot in expected {
            if let Some(task) = self.entries.iter_mut().find(|task| task.id == snapshot.id) {
                task.next_due = now.saturating_add(task.interval_secs);
            }
        }
        self.persist()?;
        Ok(true)
    }

    /// A fresh catalog sent with every handwritten turn, so the oracle can
    /// answer questions about the Master's persistent task list.
    pub fn catalog_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, task)| {
                format!(
                    "{}. [{}] every {} — {}",
                    i + 1,
                    if task.paused { "paused" } else { "active" },
                    describe_interval(task.interval_secs),
                    task.instruction
                )
            })
            .collect()
    }

    pub fn panel_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, task)| {
                format!(
                    "{}  {}  每{}  {}",
                    index + 1,
                    if task.paused {
                        "已暂停"
                    } else {
                        "执行中"
                    },
                    describe_interval_zh(task.interval_secs),
                    task.instruction
                )
            })
            .collect()
    }

    pub fn panel_enabled(&self) -> Vec<bool> {
        self.entries.iter().map(|task| !task.paused).collect()
    }
}

#[cfg(test)]
mod tests;
