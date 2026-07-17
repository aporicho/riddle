//! Persistent recurring commands for Magic Paper.
//!
//! A handwritten transcription such as
//! `任务 每五分钟讲一个黑暗冷笑话` becomes a local task.  The task list is
//! deliberately separate from page memories: it is small, explicit, survives
//! restarts, and can be checked without spending an oracle request.

use std::io;
use std::path::PathBuf;

const MIN_INTERVAL_SECS: u64 = 5 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: u64,
    pub interval_secs: u64,
    pub next_due: u64,
    pub instruction: String,
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
            let mut cols = line.splitn(4, '\t');
            let (Some(id), Some(interval), Some(next_due), Some(instruction)) =
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
            self.entries.push(Task {
                id,
                interval_secs,
                next_due,
                instruction: unescape(instruction),
            });
        }
    }

    fn persist(&self) -> io::Result<()> {
        let mut out = String::new();
        for task in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                task.id,
                task.interval_secs,
                task.next_due,
                escape(&task.instruction)
            ));
        }
        let tmp = self.dir.join("index.tsv.new");
        std::fs::write(&tmp, out)?;
        std::fs::rename(tmp, self.index_path())
    }

    /// Parse a handwritten transcription and persist it when it is a valid
    /// recurring-task command. Non-task writing returns `Ok(None)`.
    pub fn add_from_transcript(&mut self, text: &str, now: u64) -> Result<Option<Task>, String> {
        let Some((interval_secs, instruction)) = parse_add_command(text)? else {
            return Ok(None);
        };
        let id = self
            .entries
            .last()
            .map(|t| t.id.saturating_add(1))
            .unwrap_or(now)
            .max(now);
        let task = Task {
            id,
            interval_secs,
            next_due: now.saturating_add(interval_secs),
            instruction,
        };
        self.entries.push(task.clone());
        if let Err(e) = self.persist() {
            self.entries.pop();
            return Err(format!("save task: {e}"));
        }
        Ok(Some(task))
    }

    pub fn due(&self, now: u64) -> Vec<Task> {
        self.entries
            .iter()
            .filter(|t| t.next_due <= now)
            .cloned()
            .collect()
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
                    "{}. every {} — {}",
                    i + 1,
                    describe_interval(task.interval_secs),
                    task.instruction
                )
            })
            .collect()
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

fn parse_add_command(text: &str) -> Result<Option<(u64, String)>, String> {
    let mut rest = text.trim_start();
    let prefixes = ["任务", "任務", "task", "Task", "TASK"];
    let Some(prefix) = prefixes.iter().find(|p| rest.starts_with(**p)) else {
        return Ok(None);
    };
    rest = rest[prefix.len()..]
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | ',' | '，'));
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
    let instruction = after_unit
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | ',' | '，'))
        .trim()
        .to_string();
    if instruction.is_empty() {
        return Err("task has no instruction".into());
    }
    Ok(Some((interval_secs, instruction)))
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

    #[test]
    fn parses_simplified_and_traditional_task_commands() {
        assert_eq!(
            parse_add_command("任务 每五分钟讲一个黑暗冷笑话").unwrap(),
            Some((300, "讲一个黑暗冷笑话".into()))
        );
        assert_eq!(
            parse_add_command("任務：每 10 分鐘 說一句哲學語錄").unwrap(),
            Some((600, "說一句哲學語錄".into()))
        );
    }

    #[test]
    fn parses_larger_chinese_intervals() {
        assert_eq!(
            parse_add_command("任务 每二十五分钟提醒我喝水").unwrap(),
            Some((1500, "提醒我喝水".into()))
        );
        assert_eq!(
            parse_add_command("task 每两小时回顾目标").unwrap(),
            Some((7200, "回顾目标".into()))
        );
    }

    #[test]
    fn rejects_short_or_incomplete_tasks() {
        assert!(parse_add_command("任务 每一分钟响一次").is_err());
        assert!(parse_add_command("任务 每五分钟").is_err());
        assert_eq!(parse_add_command("今天写点什么").unwrap(), None);
    }

    #[test]
    fn persists_due_and_successful_run_state() {
        let mut s = tmp_store("round-trip");
        let task = s
            .add_from_transcript("任务 每五分钟讲一个黑暗冷笑话", 1000)
            .unwrap()
            .unwrap();
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
        let _ = std::fs::remove_dir_all(dir);
    }
}
