//! Zero-network routing for paper commands and simple arithmetic.

use std::sync::mpsc::Sender;

use super::Event;

#[derive(Debug, PartialEq)]
pub(super) enum LocalRoute {
    Event(Event),
    Command,
    Arithmetic(String),
}

pub(super) fn local_route(text: &str) -> Option<LocalRoute> {
    let trimmed = text.trim();
    let bare = trimmed
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '.' | ',' | ':' | ';' | '!' | '?' | '。' | '，' | '：' | '；' | '！' | '？'
                )
        })
        .to_ascii_lowercase();
    match bare.as_str() {
        "字体" | "字體" => return Some(LocalRoute::Event(Event::FontList)),
        "历史" | "歷史" => return Some(LocalRoute::Event(Event::HistoryList)),
        "帮助" | "幫助" | "help" => return Some(LocalRoute::Event(Event::Help)),
        "任务" | "任務" | "task" | "tasks" => return Some(LocalRoute::Event(Event::TaskList)),
        "todo" => return Some(LocalRoute::Event(Event::TodoList)),
        "read" => return Some(LocalRoute::Event(Event::Reader(None))),
        "刷新" | "刷新屏幕" | "重新整理" | "refresh" => {
            return Some(LocalRoute::Event(Event::FullRefresh));
        }
        _ => {}
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("read") && trimmed.is_char_boundary(4) {
        let rest = trimmed[4..].trim_start();
        let rest = rest.strip_prefix([':', '：']).unwrap_or(rest).trim();
        if !rest.is_empty()
            && trimmed[4..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || matches!(c, ':' | '：'))
        {
            return Some(LocalRoute::Event(Event::Reader(Some(rest.to_string()))));
        }
    }
    let task_prefixes = [
        "任务",
        "任務",
        "删除任务",
        "刪除任務",
        "删除任務",
        "刪除任务",
        "暂停任务",
        "暫停任務",
        "恢复任务",
        "恢復任務",
        "修改任务",
        "修改任務",
        "task ",
        "delete task",
        "pause task",
        "resume task",
        "modify task",
        "change task",
    ];
    if task_prefixes.iter().any(|prefix| lower.starts_with(prefix))
        || (lower.starts_with("todo") && lower.len() > 4)
    {
        return Some(LocalRoute::Command);
    }

    evaluate_arithmetic(trimmed).map(LocalRoute::Arithmetic)
}

pub(super) fn emit_local_route(
    route: LocalRoute,
    recognized: &str,
    tx: &Sender<Result<Event, String>>,
) {
    match route {
        LocalRoute::Event(event) => {
            let _ = tx.send(Ok(event));
        }
        LocalRoute::Command => {
            let _ = tx.send(Ok(Event::LocalCommand(recognized.trim().to_string())));
        }
        LocalRoute::Arithmetic(answer) => {
            let _ = tx.send(Ok(Event::Ink(answer)));
            let _ = tx.send(Ok(Event::Transcript(recognized.trim().to_string())));
        }
    }
}

pub(super) fn evaluate_arithmetic(input: &str) -> Option<String> {
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty()
        || compact.chars().any(|c| {
            !c.is_ascii_digit()
                && !matches!(
                    c,
                    '.' | '+'
                        | '-'
                        | '*'
                        | '×'
                        | 'x'
                        | 'X'
                        | '/'
                        | '÷'
                        | '('
                        | ')'
                        | '='
                        | '＝'
                        | '?'
                        | '？'
                )
        })
    {
        return None;
    }
    let mut visible = compact.trim_end_matches(['?', '？']).to_string();
    let expression = if let Some((left, right)) = visible.split_once(['=', '＝']) {
        if !right.is_empty() && right != "?" && right != "？" {
            return None;
        }
        left.to_string()
    } else {
        visible.clone()
    };
    if !expression
        .chars()
        .any(|c| matches!(c, '+' | '-' | '*' | '×' | 'x' | 'X' | '/' | '÷'))
    {
        return None;
    }
    let normalized = expression.replace(['×', 'x', 'X'], "*").replace('÷', "/");
    let mut parser = ArithmeticParser::new(&normalized);
    let value = parser.expression()?;
    if parser.remaining().is_empty() && value.is_finite() {
        let result = if (value - value.round()).abs() < 1e-10 {
            format!("{:.0}", value)
        } else {
            let mut text = format!("{value:.10}");
            while text.ends_with('0') {
                text.pop();
            }
            text.trim_end_matches('.').to_string()
        };
        if !visible.ends_with(['=', '＝']) {
            visible.push('=');
        }
        Some(format!("{visible}{result}"))
    } else {
        None
    }
}

struct ArithmeticParser<'a> {
    source: &'a [u8],
    position: usize,
}

impl<'a> ArithmeticParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            position: 0,
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.source[self.position..]
    }

    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            match self.source.get(self.position).copied() {
                Some(b'+') => {
                    self.position += 1;
                    value += self.term()?;
                }
                Some(b'-') => {
                    self.position += 1;
                    value -= self.term()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut value = self.factor()?;
        loop {
            match self.source.get(self.position).copied() {
                Some(b'*') => {
                    self.position += 1;
                    value *= self.factor()?;
                }
                Some(b'/') => {
                    self.position += 1;
                    let divisor = self.factor()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value /= divisor;
                }
                _ => return Some(value),
            }
        }
    }

    fn factor(&mut self) -> Option<f64> {
        if self.source.get(self.position) == Some(&b'-') {
            self.position += 1;
            return Some(-self.factor()?);
        }
        if self.source.get(self.position) == Some(&b'(') {
            self.position += 1;
            let value = self.expression()?;
            if self.source.get(self.position) != Some(&b')') {
                return None;
            }
            self.position += 1;
            return Some(value);
        }
        let start = self.position;
        while self
            .source
            .get(self.position)
            .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
        {
            self.position += 1;
        }
        if start == self.position {
            return None;
        }
        std::str::from_utf8(&self.source[start..self.position])
            .ok()?
            .parse()
            .ok()
    }
}
