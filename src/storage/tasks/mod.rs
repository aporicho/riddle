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

mod parser;

use parser::parse_command;

const MIN_INTERVAL_SECS: u64 = 5 * 60;
const MAX_TASKS: usize = 9;

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
    std::env::var("RIDDLE_TASKS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/root/riddle-data/tasks"))
}

fn scheduler_lock_file(dir: &Path) -> io::Result<File> {
    std::fs::create_dir_all(&dir)?;
    OpenOptions::new()
        .create(true)
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
                        "已暫停"
                    } else {
                        "執行中"
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

pub fn heartbeat_prompt(due: &[Task]) -> String {
    let mut lines = String::new();
    for task in due {
        lines.push_str(&format!(
            "- Every {}: {}\n",
            describe_interval(task.interval_secs),
            task.instruction
        ));
    }
    format!(
        "[INTERNAL MAGIC PAPER HEARTBEAT]\nThe following recurring commands from Master are due now:\n{lines}\nPerform each due command exactly once. Write only the requested output for Master, using the language of that command. Do not mention the heartbeat, scheduling, or task list. If several commands are due, give each one a separate short paragraph. Do not add a greeting or confirmation."
    )
}

fn describe_interval(seconds: u64) -> String {
    if seconds % 86400 == 0 {
        format!("{} days", seconds / 86400)
    } else if seconds % 3600 == 0 {
        format!("{} hours", seconds / 3600)
    } else {
        format!("{} minutes", seconds / 60)
    }
}

fn describe_interval_zh(seconds: u64) -> String {
    if seconds % 86400 == 0 {
        format!("{}天", seconds / 86400)
    } else if seconds % 3600 == 0 {
        format!("{}小時", seconds / 3600)
    } else {
        format!("{}分鐘", seconds / 60)
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(name: &str) -> TaskStore {
        let dir = std::env::temp_dir().join(format!(
            "magic-paper-task-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TaskStore {
            dir,
            entries: Vec::new(),
        }
    }

    fn add(store: &mut TaskStore, text: &str, now: u64) -> Task {
        match store.apply_from_transcript(text, now).unwrap().unwrap() {
            TaskChange::Added(task) => task,
            other => panic!("expected addition, got {other:?}"),
        }
    }

    fn reopen(dir: &std::path::Path) -> TaskStore {
        let mut store = TaskStore {
            dir: dir.to_path_buf(),
            entries: Vec::new(),
        };
        store.load();
        store
    }

    #[test]
    fn parses_simplified_and_traditional_task_commands() {
        assert_eq!(
            parse_command("任务 每五分钟讲一个黑暗冷笑话").unwrap(),
            Some(TaskCommand::Add {
                interval_secs: 300,
                instruction: "讲一个黑暗冷笑话".into(),
            })
        );
        assert_eq!(
            parse_command("任務：每 10 分鐘 說一句哲學語錄").unwrap(),
            Some(TaskCommand::Add {
                interval_secs: 600,
                instruction: "說一句哲學語錄".into(),
            })
        );
    }

    #[test]
    fn parses_larger_chinese_intervals() {
        assert_eq!(
            parse_command("任务 每二十五分钟提醒我喝水").unwrap(),
            Some(TaskCommand::Add {
                interval_secs: 1500,
                instruction: "提醒我喝水".into(),
            })
        );
        assert_eq!(
            parse_command("task 每两小时回顾目标").unwrap(),
            Some(TaskCommand::Add {
                interval_secs: 7200,
                instruction: "回顾目标".into(),
            })
        );
    }

    #[test]
    fn parses_management_commands_and_task_numbers() {
        assert_eq!(
            parse_command("删除任务 2").unwrap(),
            Some(TaskCommand::Delete(2))
        );
        assert_eq!(
            parse_command("任務：暫停第二項").unwrap(),
            Some(TaskCommand::Pause(2))
        );
        assert_eq!(
            parse_command("恢复任务二号。 ").unwrap(),
            Some(TaskCommand::Resume(2))
        );
        assert_eq!(
            parse_command("修改任務 2 為 每十分鐘提醒我喝水").unwrap(),
            Some(TaskCommand::Modify {
                number: 2,
                interval_secs: 600,
                instruction: "提醒我喝水".into(),
            })
        );
    }

    #[test]
    fn rejects_short_or_incomplete_tasks() {
        assert!(parse_command("任务 每一分钟响一次").is_err());
        assert!(parse_command("任务 每五分钟").is_err());
        assert!(parse_command("删除任务").is_err());
        assert!(parse_command("修改任务 1 提醒我").is_err());
        assert_eq!(parse_command("今天写点什么").unwrap(), None);
    }

    #[test]
    fn persists_due_and_successful_run_state() {
        let mut s = tmp_store("round-trip");
        let task = add(&mut s, "任务 每五分钟讲一个黑暗冷笑话", 1000);
        assert!(s.due(1299).is_empty());
        assert_eq!(s.due(1300), vec![task.clone()]);
        assert!(s
            .complete_due_if_unchanged(std::slice::from_ref(&task), 1301)
            .unwrap());
        assert!(s.due(1600).is_empty());
        assert_eq!(s.due(1601).len(), 1);

        let dir = s.dir.clone();
        let mut reopened = TaskStore {
            dir: dir.clone(),
            entries: Vec::new(),
        };
        reopened.load();
        assert_eq!(reopened.entries.len(), 1);
        assert_eq!(reopened.entries[0].instruction, "讲一个黑暗冷笑话");
        assert_eq!(reopened.entries[0].next_due, 1601);
        assert!(!reopened.entries[0].paused);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pauses_resumes_modifies_and_deletes_persistently() {
        let mut s = tmp_store("manage");
        add(&mut s, "任务 每五分钟讲笑话", 1000);
        add(&mut s, "任务 每十分钟提醒喝水", 1001);

        let change = s
            .apply_from_transcript("暂停任务 1", 1100)
            .unwrap()
            .unwrap();
        assert!(matches!(change, TaskChange::Paused { number: 1, .. }));
        assert!(s.due(2000).iter().all(|task| task.instruction != "讲笑话"));

        s.apply_from_transcript("修改任务 1 每十五分钟讲冷笑话", 1200)
            .unwrap();
        assert!(s.entries[0].paused);
        assert_eq!(s.entries[0].interval_secs, 900);
        assert_eq!(s.entries[0].instruction, "讲冷笑话");

        s.apply_from_transcript("恢复任务 1", 1300).unwrap();
        assert!(!s.entries[0].paused);
        assert_eq!(s.entries[0].next_due, 2200);
        assert!(s
            .due(2199)
            .iter()
            .all(|task| task.instruction != "讲冷笑话"));

        let deleted = s
            .apply_from_transcript("删除任务 1", 1400)
            .unwrap()
            .unwrap();
        assert!(matches!(deleted, TaskChange::Deleted { number: 1, .. }));
        assert_eq!(s.entries.len(), 1);
        assert_eq!(
            s.catalog_lines()[0],
            "1. [active] every 10 minutes — 提醒喝水"
        );

        let dir = s.dir.clone();
        let mut reopened = TaskStore {
            dir: dir.clone(),
            entries: Vec::new(),
        };
        reopened.load();
        assert_eq!(reopened.entries, s.entries);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paper_checkbox_toggles_and_restarts_the_interval() {
        let mut s = tmp_store("paper-toggle");
        add(&mut s, "任务 每五分钟提醒喝水", 1000);
        let disabled = s.toggle_number(1, 1100).unwrap();
        assert!(disabled.paused);
        assert_eq!(s.panel_enabled(), vec![false]);
        let enabled = s.toggle_number(1, 5000).unwrap();
        assert!(!enabled.paused);
        assert_eq!(enabled.next_due, 5300);
        assert_eq!(s.panel_enabled(), vec![true]);
        let _ = std::fs::remove_dir_all(s.dir);
    }

    #[test]
    fn migrates_old_active_tasks_and_limits_the_list_to_nine() {
        let mut s = tmp_store("migration-and-limit");
        std::fs::write(s.index_path(), "7\t300\t900\t旧任务\\n一行\n").unwrap();
        s.load();
        assert_eq!(s.entries.len(), 1);
        assert!(!s.entries[0].paused);
        assert_eq!(s.entries[0].instruction, "旧任务\n一行");

        for n in 2..=MAX_TASKS {
            add(&mut s, &format!("任务 每五分钟任务{n}"), 1000 + n as u64);
        }
        assert_eq!(s.entries.len(), MAX_TASKS);
        assert!(s
            .apply_from_transcript("任务 每五分钟第十个任务", 2000)
            .unwrap_err()
            .contains("at most 9"));
        let _ = std::fs::remove_dir_all(s.dir);
    }

    #[test]
    fn stale_agent_completion_cannot_resurrect_ui_changes() {
        let mut ui = tmp_store("agent-ui-race");
        let task = add(&mut ui, "任务 每五分钟提醒喝水", 1000);
        let dir = ui.dir.clone();
        let mut agent = reopen(&dir);
        let due_snapshot = vec![task.clone()];

        ui.apply_from_transcript("修改任务 1 每十分钟提醒休息", 1300)
            .unwrap();
        assert!(!agent
            .complete_due_if_unchanged(&due_snapshot, 1400)
            .unwrap());
        let current = reopen(&dir);
        assert_eq!(current.entries[0].instruction, "提醒休息");
        assert_eq!(current.entries[0].interval_secs, 600);
        assert_eq!(current.entries[0].next_due, 1900);

        let modified = current.entries[0].clone();
        ui.delete_number(1).unwrap();
        assert!(!agent.complete_due_if_unchanged(&[modified], 2000).unwrap());
        assert!(reopen(&dir).entries.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paused_task_is_not_advanced_by_stale_agent_store() {
        let mut ui = tmp_store("pause-race");
        let task = add(&mut ui, "任务 每五分钟提醒喝水", 1000);
        let dir = ui.dir.clone();
        let mut stale_agent = reopen(&dir);
        ui.toggle_number(1, 1200).unwrap();
        assert!(!stale_agent
            .complete_due_if_unchanged(std::slice::from_ref(&task), 1300)
            .unwrap());
        let current = reopen(&dir);
        assert!(current.entries[0].paused);
        assert_eq!(current.entries[0].next_due, task.next_due);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_one_concurrent_scheduler_can_complete_a_due_snapshot() {
        let mut original = tmp_store("double-complete");
        let task = add(&mut original, "任务 每五分钟提醒喝水", 1000);
        let dir = original.dir.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for now in [1301, 1302] {
            let dir = dir.clone();
            let task = task.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut store = reopen(&dir);
                barrier.wait();
                store.complete_due_if_unchanged(&[task], now).unwrap()
            }));
        }
        barrier.wait();
        let results: Vec<bool> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| **result).count(), 1);
        assert_eq!(reopen(&dir).entries.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_additions_reload_under_lock_and_keep_unique_ids() {
        let original = tmp_store("concurrent-add");
        let dir = original.dir.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for (text, now) in [
            ("任务 每五分钟提醒喝水", 1000),
            ("任务 每十分钟提醒休息", 1001),
        ] {
            let dir = dir.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut store = reopen(&dir);
                barrier.wait();
                store.apply_from_transcript(text, now).unwrap();
            }));
        }
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        let current = reopen(&dir);
        assert_eq!(current.entries.len(), 2);
        assert_ne!(current.entries[0].id, current.entries[1].id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scheduler_lease_excludes_every_other_ui_or_agent_owner() {
        let store = tmp_store("scheduler-lease");
        let dir = store.dir.clone();
        drop(store);
        let owner = acquire_scheduler_lease_in(&dir).expect("first scheduler owns the lease");
        assert!(external_scheduler_active_in(&dir));
        let error = match acquire_scheduler_lease_in(&dir) {
            Ok(_) => panic!("a second scheduler acquired the same lease"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        drop(owner);
        assert!(!external_scheduler_active_in(&dir));
        assert!(acquire_scheduler_lease_in(&dir).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
