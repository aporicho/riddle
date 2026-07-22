//! Zero-network routing for paper commands and simple arithmetic.

use std::sync::mpsc::Sender;

use super::Event;
use crate::{tasks, todos};

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
        "设置" | "設定" | "settings" => {
            return Some(LocalRoute::Event(Event::Settings));
        }
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
    if tasks::is_local_command(trimmed) || todos::is_local_add_command(trimmed) {
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
    if parser.remaining().is_empty() {
        let result = value.paper_string()?;
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

    fn expression(&mut self) -> Option<ExactNumber> {
        let mut value = self.term()?;
        loop {
            match self.source.get(self.position).copied() {
                Some(b'+') => {
                    self.position += 1;
                    value = value.checked_add(self.term()?)?;
                }
                Some(b'-') => {
                    self.position += 1;
                    value = value.checked_sub(self.term()?)?;
                }
                _ => return Some(value),
            }
        }
    }

    fn term(&mut self) -> Option<ExactNumber> {
        let mut value = self.factor()?;
        loop {
            match self.source.get(self.position).copied() {
                Some(b'*') => {
                    self.position += 1;
                    value = value.checked_mul(self.factor()?)?;
                }
                Some(b'/') => {
                    self.position += 1;
                    let divisor = self.factor()?;
                    value = value.checked_div(divisor)?;
                }
                _ => return Some(value),
            }
        }
    }

    fn factor(&mut self) -> Option<ExactNumber> {
        if self.source.get(self.position) == Some(&b'-') {
            self.position += 1;
            return self.factor()?.checked_neg();
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
        ExactNumber::parse(std::str::from_utf8(&self.source[start..self.position]).ok()?)
    }
}

/// A reduced rational backed by checked i128 arithmetic. Handwritten
/// calculations therefore stay exact well beyond f64's 53-bit integer limit;
/// expressions that exceed the bound simply fall through to the normal oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactNumber {
    numerator: i128,
    denominator: i128,
}

impl ExactNumber {
    fn new(mut numerator: i128, mut denominator: i128) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        if denominator < 0 {
            numerator = numerator.checked_neg()?;
            denominator = denominator.checked_neg()?;
        }
        let divisor = gcd(numerator.unsigned_abs(), denominator as u128) as i128;
        Some(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    fn parse(text: &str) -> Option<Self> {
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        if text.matches('.').count() > 1 || (whole.is_empty() && fraction.is_empty()) {
            return None;
        }
        let digits = format!("{}{}", if whole.is_empty() { "0" } else { whole }, fraction);
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let numerator = digits.parse::<i128>().ok()?;
        let denominator = 10_i128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
        Self::new(numerator, denominator)
    }

    fn checked_neg(self) -> Option<Self> {
        Self::new(self.numerator.checked_neg()?, self.denominator)
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        let common = gcd(self.denominator as u128, other.denominator as u128) as i128;
        let left_scale = other.denominator / common;
        let right_scale = self.denominator / common;
        let numerator = self
            .numerator
            .checked_mul(left_scale)?
            .checked_add(other.numerator.checked_mul(right_scale)?)?;
        Self::new(numerator, self.denominator.checked_mul(left_scale)?)
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        self.checked_add(other.checked_neg()?)
    }

    fn checked_mul(self, other: Self) -> Option<Self> {
        let left_cancel = gcd(self.numerator.unsigned_abs(), other.denominator as u128) as i128;
        let right_cancel = gcd(other.numerator.unsigned_abs(), self.denominator as u128) as i128;
        Self::new(
            (self.numerator / left_cancel).checked_mul(other.numerator / right_cancel)?,
            (self.denominator / right_cancel).checked_mul(other.denominator / left_cancel)?,
        )
    }

    fn checked_div(self, other: Self) -> Option<Self> {
        if other.numerator == 0 {
            return None;
        }
        self.checked_mul(Self::new(other.denominator, other.numerator)?)
    }

    fn paper_string(self) -> Option<String> {
        if self.denominator == 1 {
            return Some(self.numerator.to_string());
        }
        let mut finite_denominator = self.denominator;
        while finite_denominator % 2 == 0 {
            finite_denominator /= 2;
        }
        while finite_denominator % 5 == 0 {
            finite_denominator /= 5;
        }
        if finite_denominator != 1 {
            return Some(format!("{}/{}", self.numerator, self.denominator));
        }

        let negative = self.numerator < 0;
        let magnitude = self.numerator.unsigned_abs();
        let denominator = self.denominator as u128;
        let integer = magnitude / denominator;
        let mut remainder = magnitude % denominator;
        let mut result = if negative {
            format!("-{integer}")
        } else {
            integer.to_string()
        };
        if remainder == 0 {
            return Some(result);
        }
        result.push('.');
        while remainder != 0 {
            remainder = remainder.checked_mul(10)?;
            result.push(char::from(
                b'0' + u8::try_from(remainder / denominator).ok()?,
            ));
            remainder %= denominator;
        }
        Some(result)
    }
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}
