use super::Task;

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

pub(super) fn describe_interval(seconds: u64) -> String {
    if seconds.is_multiple_of(86400) {
        format!("{} days", seconds / 86400)
    } else if seconds.is_multiple_of(3600) {
        format!("{} hours", seconds / 3600)
    } else {
        format!("{} minutes", seconds / 60)
    }
}

pub(super) fn describe_interval_zh(seconds: u64) -> String {
    if seconds.is_multiple_of(86400) {
        format!("{}天", seconds / 86400)
    } else if seconds.is_multiple_of(3600) {
        format!("{}小时", seconds / 3600)
    } else {
        format!("{}分钟", seconds / 60)
    }
}

pub(super) fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

pub(super) fn unescape(s: &str) -> String {
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
