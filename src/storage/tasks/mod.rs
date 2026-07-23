//! Persistent recurring commands for MagicPaper's due-time scheduler.
//!
//! A handwritten transcription such as
//! `任务 每五分钟讲一个黑暗冷笑话` becomes a local task.  The task list is
//! deliberately separate from page memories: it is small, explicit, survives
//! restarts, and can be checked without spending an oracle request.

use crate::storage::persistence::{
    atomic_write, invalid_line, lock_exclusive, read_optional_utf8, unescape_field, StoreLock,
};
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

mod formatting;
mod lease;
mod parser;

pub use formatting::heartbeat_prompt;
use formatting::{describe_interval, describe_interval_zh, escape};
pub use lease::{acquire_scheduler_lease, external_scheduler_active, SchedulerLease};
#[cfg(test)]
use lease::{acquire_scheduler_lease_in, external_scheduler_active_in};

use parser::parse_command;

const MIN_INTERVAL_SECS: u64 = 5 * 60;
const MAX_TASKS: usize = 9;
const MAX_TASK_INSTRUCTION_BYTES: usize = 4 * 1024;
const MAX_TASK_INDEX_BYTES: usize = 128 * 1024;

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

fn task_dir() -> PathBuf {
    crate::runtime_env::persistent_path(
        "MAGICPAPER_TASKS_DIR",
        "tasks",
        "/home/root/.local/share/magicpaper/tasks",
    )
}

impl TaskStore {
    pub fn open() -> Option<Self> {
        match std::env::var("MAGICPAPER_TASKS").as_deref() {
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
        match store.load() {
            Ok(()) => Some(store),
            Err(error) => {
                eprintln!(
                    "magic-paper: tasks disabled because {} could not be loaded: {error}",
                    store.index_path().display()
                );
                None
            }
        }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.tsv")
    }

    fn lock_exclusive(&self) -> io::Result<StoreLock> {
        lock_exclusive(&self.dir)
    }

    fn load(&mut self) -> io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.load_unlocked()
    }

    fn load_unlocked(&mut self) -> io::Result<()> {
        let path = self.index_path();
        let Some(text) = read_optional_utf8(&path, MAX_TASK_INDEX_BYTES)? else {
            self.entries.clear();
            return Ok(());
        };
        let mut entries = Vec::new();
        let mut ids = HashSet::new();
        for (line_index, line) in text.lines().enumerate() {
            let line_number = line_index + 1;
            if line.is_empty() {
                return Err(invalid_line(&path, line_number, "empty task record"));
            }
            let columns = line.split('\t').collect::<Vec<_>>();
            if !matches!(columns.len(), 4 | 5) {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "task record must contain four legacy columns or five current columns",
                ));
            }
            let id = columns[0]
                .parse::<u64>()
                .map_err(|_| invalid_line(&path, line_number, "invalid task id"))?;
            let interval_secs = columns[1]
                .parse::<u64>()
                .map_err(|_| invalid_line(&path, line_number, "invalid task interval"))?;
            let next_due = columns[2]
                .parse::<u64>()
                .map_err(|_| invalid_line(&path, line_number, "invalid task due time"))?;
            if interval_secs < MIN_INTERVAL_SECS {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "task interval is below the supported minimum",
                ));
            }
            if !ids.insert(id) {
                return Err(invalid_line(&path, line_number, "duplicate task id"));
            }
            // MagicPaper 0.4.2 stored four columns. The fifth-column layout
            // adds state while treating every old task as active.
            let (paused, encoded_instruction) = if columns.len() == 4 {
                (false, columns[3])
            } else {
                let paused = match columns[3] {
                    "active" => false,
                    "paused" => true,
                    _ => {
                        return Err(invalid_line(&path, line_number, "invalid task state"));
                    }
                };
                (paused, columns[4])
            };
            let instruction = unescape_field(encoded_instruction)
                .map_err(|error| invalid_line(&path, line_number, &error.to_string()))?;
            if instruction.trim().is_empty() {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "task instruction is empty",
                ));
            }
            if instruction.len() > MAX_TASK_INSTRUCTION_BYTES {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "task instruction exceeds its size limit",
                ));
            }
            entries.push(Task {
                id,
                interval_secs,
                next_due,
                instruction,
                paused,
            });
            if entries.len() > MAX_TASKS {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "task store exceeds its maximum size",
                ));
            }
        }
        self.entries = entries;
        Ok(())
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
        atomic_write(&self.index_path(), out.as_bytes())
    }

    fn recover_after_failed_persist(&mut self, fallback: Vec<Task>) {
        if self.load_unlocked().is_err() {
            self.entries = fallback;
        }
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
        self.load_unlocked()
            .map_err(|error| format!("load task store: {error}"))?;
        let old_entries = self.entries.clone();
        let change = match command {
            TaskCommand::Add {
                interval_secs,
                instruction,
            } => {
                if self.entries.len() >= MAX_TASKS {
                    return Err(format!("at most {MAX_TASKS} tasks are allowed"));
                }
                let id = match self.entries.iter().map(|task| task.id).max() {
                    Some(previous) => previous
                        .checked_add(1)
                        .ok_or_else(|| "task id space is exhausted".to_string())?
                        .max(now),
                    None => now,
                };
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
            self.recover_after_failed_persist(old_entries);
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
        self.load_unlocked()
            .map_err(|error| format!("load task store: {error}"))?;
        let old_entries = self.entries.clone();
        let index = self.index(number)?;
        let task = self.entries.remove(index);
        if let Err(e) = self.persist() {
            self.recover_after_failed_persist(old_entries);
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
        self.load_unlocked()
            .map_err(|error| format!("load task store: {error}"))?;
        let old_entries = self.entries.clone();
        let index = self.index(number)?;
        self.entries[index].paused = !self.entries[index].paused;
        if !self.entries[index].paused {
            self.entries[index].next_due = now.saturating_add(self.entries[index].interval_secs);
        }
        if let Err(error) = self.persist() {
            self.recover_after_failed_persist(old_entries);
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
        self.load_unlocked()?;
        if expected.iter().any(|snapshot| {
            self.entries
                .iter()
                .find(|task| task.id == snapshot.id)
                .is_none_or(|current| current != snapshot || current.paused)
        }) {
            return Ok(false);
        }
        let old_entries = self.entries.clone();
        for snapshot in expected {
            if let Some(task) = self.entries.iter_mut().find(|task| task.id == snapshot.id) {
                task.next_due = now.saturating_add(task.interval_secs);
            }
        }
        if let Err(error) = self.persist() {
            self.recover_after_failed_persist(old_entries);
            return Err(error);
        }
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
