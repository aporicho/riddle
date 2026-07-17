//! Persistent recurring commands for Magic Paper.
//!
//! A handwritten transcription such as
//! `任务 每五分钟讲一个黑暗冷笑话` becomes a local task.  The task list is
//! deliberately separate from page memories: it is small, explicit, survives
//! restarts, and can be checked without spending an oracle request.

use std::io;
use std::path::PathBuf;

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

impl TaskStore {
    pub fn open() -> Option<Self> {
        match std::env::var("RIDDLE_TASKS").as_deref() {
            Ok("off") | Ok("0") | Ok("no") | Ok("false") => return None,
            _ => {}
        }
        let dir = std::env::var("RIDDLE_TASKS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/home/root/riddle-data/tasks"));
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

    fn load(&mut self) {
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
        std::fs::write(&tmp, out)?;
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

    /// Advance only tasks whose output completed successfully. Missed periods
    /// are intentionally skipped instead of flooding the page after downtime.
    pub fn mark_ran(&mut self, ids: &[u64], now: u64) -> io::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for task in &mut self.entries {
            if ids.contains(&task.id) {
                task.next_due = now.saturating_add(task.interval_secs);
            }
        }
        self.persist()
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

fn parse_command(text: &str) -> Result<Option<TaskCommand>, String> {
    let text = text.trim();

    if let Some(rest) = strip_any_prefix(
        text,
        &[
            "删除任务",
            "刪除任務",
            "删除任務",
            "刪除任务",
            "delete task",
        ],
    ) {
        return parse_number_only(rest, "delete").map(|n| Some(TaskCommand::Delete(n)));
    }
    if let Some(rest) = strip_any_prefix(
        text,
        &["暂停任务", "暫停任務", "暂停任務", "暫停任务", "pause task"],
    ) {
        return parse_number_only(rest, "pause").map(|n| Some(TaskCommand::Pause(n)));
    }
    if let Some(rest) = strip_any_prefix(
        text,
        &[
            "恢复任务",
            "恢復任務",
            "恢复任務",
            "恢復任务",
            "继续任务",
            "繼續任務",
            "resume task",
        ],
    ) {
        return parse_number_only(rest, "resume").map(|n| Some(TaskCommand::Resume(n)));
    }
    if let Some(rest) = strip_any_prefix(
        text,
        &["修改任务", "修改任務", "modify task", "change task"],
    ) {
        return parse_modify_command(rest).map(Some);
    }

    // Also accept verb-first forms under the common task introducer, such as
    // `任务 暂停 2`. Addition remains `任务 每五分钟……`.
    let mut rest = text;
    let prefixes = ["任务", "任務", "task", "Task", "TASK"];
    let Some(prefix) = prefixes.iter().find(|p| rest.starts_with(**p)) else {
        return Ok(None);
    };
    rest = trim_separators(&rest[prefix.len()..]);
    if let Some(after) = strip_any_prefix(rest, &["删除", "刪除", "delete"]) {
        return parse_number_only(after, "delete").map(|n| Some(TaskCommand::Delete(n)));
    }
    if let Some(after) = strip_any_prefix(rest, &["暂停", "暫停", "pause"]) {
        return parse_number_only(after, "pause").map(|n| Some(TaskCommand::Pause(n)));
    }
    if let Some(after) = strip_any_prefix(rest, &["恢复", "恢復", "继续", "繼續", "resume"])
    {
        return parse_number_only(after, "resume").map(|n| Some(TaskCommand::Resume(n)));
    }
    if let Some(after) = strip_any_prefix(rest, &["修改", "modify", "change"]) {
        return parse_modify_command(after).map(Some);
    }
    let (interval_secs, instruction) = parse_schedule(rest)?;
    Ok(Some(TaskCommand::Add {
        interval_secs,
        instruction,
    }))
}

fn parse_modify_command(rest: &str) -> Result<TaskCommand, String> {
    let (number, rest) = parse_number_prefix(rest, "modify")?;
    let rest = trim_separators(rest);
    let rest = strip_any_prefix(rest, &["改为", "改為", "改成", "为", "為", "to"])
        .map(trim_separators)
        .unwrap_or(rest);
    let (interval_secs, instruction) = parse_schedule(rest)?;
    Ok(TaskCommand::Modify {
        number,
        interval_secs,
        instruction,
    })
}

fn parse_schedule(rest: &str) -> Result<(u64, String), String> {
    let Some(after_every) = rest.strip_prefix('每') else {
        return Err("task needs a recurring interval beginning with 每".into());
    };
    let after_every = after_every.trim_start();
    let Some((n, used)) = take_number(after_every) else {
        return Err("task interval has no number".into());
    };
    if n == 0 {
        return Err("task interval must be positive".into());
    }
    let after_number = after_every[used..].trim_start();
    let (unit_secs, after_unit) = if let Some(r) = after_number.strip_prefix("分钟") {
        (60, r)
    } else if let Some(r) = after_number.strip_prefix("分鐘") {
        (60, r)
    } else if let Some(r) = after_number.strip_prefix("小时") {
        (3600, r)
    } else if let Some(r) = after_number.strip_prefix("小時") {
        (3600, r)
    } else if let Some(r) = after_number.strip_prefix('时') {
        (3600, r)
    } else if let Some(r) = after_number.strip_prefix('時') {
        (3600, r)
    } else if let Some(r) = after_number.strip_prefix('天') {
        (86400, r)
    } else {
        return Err("task interval needs 分钟、小时 or 天".into());
    };
    let interval_secs = n.saturating_mul(unit_secs);
    if interval_secs < MIN_INTERVAL_SECS {
        return Err("the shortest task interval is five minutes".into());
    }
    let instruction = trim_separators(after_unit).trim().to_string();
    if instruction.is_empty() {
        return Err("task has no instruction".into());
    }
    Ok((interval_secs, instruction))
}

fn strip_any_prefix<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|prefix| text.strip_prefix(prefix))
}

fn trim_separators(text: &str) -> &str {
    text.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '：' | ',' | '，' | '-' | '—')
    })
}

fn parse_number_only(rest: &str, operation: &str) -> Result<usize, String> {
    let (number, trailing) = parse_number_prefix(rest, operation)?;
    let trailing = trailing.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '.' | '。' | '!' | '！' | '?' | '？')
    });
    if !trailing.is_empty() {
        return Err(format!("{operation} task command has unexpected text"));
    }
    Ok(number)
}

fn parse_number_prefix<'a>(rest: &'a str, operation: &str) -> Result<(usize, &'a str), String> {
    let rest = trim_separators(rest);
    let rest = rest.strip_prefix('第').unwrap_or(rest);
    let Some((number, used)) = take_number(rest) else {
        return Err(format!("{operation} task command needs a task number"));
    };
    let number = usize::try_from(number).map_err(|_| "task number is too large")?;
    if number == 0 {
        return Err("task numbers begin at one".into());
    }
    let trailing = &rest[used..];
    let trailing = trailing
        .strip_prefix('号')
        .or_else(|| trailing.strip_prefix('號'))
        .or_else(|| trailing.strip_prefix('个'))
        .or_else(|| trailing.strip_prefix('個'))
        .or_else(|| trailing.strip_prefix('项'))
        .or_else(|| trailing.strip_prefix('項'))
        .unwrap_or(trailing);
    Ok((number, trailing))
}

fn take_number(s: &str) -> Option<(u64, usize)> {
    let ascii_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if ascii_len > 0 {
        return s[..ascii_len].parse().ok().map(|n| (n, ascii_len));
    }
    let mut used = 0;
    let mut chars = Vec::new();
    for (i, c) in s.char_indices() {
        if chinese_digit(c).is_some() || matches!(c, '十' | '百' | '千') {
            used = i + c.len_utf8();
            chars.push(c);
        } else {
            break;
        }
    }
    if chars.is_empty() {
        return None;
    }
    chinese_number(&chars).map(|n| (n, used))
}

fn chinese_digit(c: char) -> Option<u64> {
    match c {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' | '兩' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

fn chinese_number(chars: &[char]) -> Option<u64> {
    let mut total = 0u64;
    let mut digit = 0u64;
    for &c in chars {
        if let Some(n) = chinese_digit(c) {
            digit = n;
            continue;
        }
        let unit = match c {
            '十' => 10,
            '百' => 100,
            '千' => 1000,
            _ => return None,
        };
        total = total.saturating_add(if digit == 0 { unit } else { digit * unit });
        digit = 0;
    }
    Some(total.saturating_add(digit))
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
        s.mark_ran(&[task.id], 1301).unwrap();
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
}
