//! Parsing handwritten recurring-task commands.

use super::{TaskCommand, MIN_INTERVAL_SECS};

pub(super) fn parse_command(text: &str) -> Result<Option<TaskCommand>, String> {
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
