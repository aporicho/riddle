//! Incremental parsing and paper-safe cleanup of streamed model output.

use super::Event;

pub(super) struct StreamParser {
    delivered: usize,
    sentinel: Option<usize>,
    route_checked: bool,
    showed: bool,
    emitted_any: bool,
    catalog_ids: Vec<u64>,
}

const SENTINEL: char = '\u{2042}'; // ⁂
const SHOW_OPEN: char = '\u{27e6}'; // ⟦
const SHOW_CLOSE: char = '\u{27e7}'; // ⟧

impl StreamParser {
    pub(super) fn new(catalog_ids: Vec<u64>) -> Self {
        Self {
            delivered: 0,
            sentinel: None,
            route_checked: false,
            showed: false,
            emitted_any: false,
            catalog_ids,
        }
    }

    /// Feed the full accumulated reply. `done` flushes the visible tail and
    /// hidden faithful transcription.
    pub(super) fn advance(&mut self, full: &str, done: bool) -> Vec<Result<Event, String>> {
        let mut out = Vec::new();

        if self.sentinel.is_none() {
            self.sentinel = full.find(SENTINEL);
        }
        let effective = self.sentinel.unwrap_or(full.len());

        if !self.route_checked && !self.route_first(full, effective, done, &mut out) {
            return out;
        }

        if self.delivered < effective {
            if let Some(cut) = sentence_cut(&full[..effective], self.delivered) {
                let chunk = strip_directives(&clean(&full[self.delivered..cut]));
                if !chunk.is_empty() {
                    self.emitted_any = true;
                    out.push(Ok(Event::Ink(chunk)));
                }
                self.delivered = cut;
            }
        }

        if self.sentinel.is_some() && self.delivered < effective {
            let rest = strip_directives(&clean(full[self.delivered..effective].trim()));
            if !rest.is_empty() {
                self.emitted_any = true;
                out.push(Ok(Event::Ink(rest)));
            }
            self.delivered = effective;
        }

        if done {
            if self.delivered < effective {
                let rest = strip_directives(&clean(full[self.delivered..effective].trim()));
                if !rest.is_empty() {
                    self.emitted_any = true;
                    out.push(Ok(Event::Ink(rest)));
                }
                self.delivered = effective;
            }
            if let Some(position) = self.sentinel {
                let transcript = full[position + SENTINEL.len_utf8()..].trim();
                if !transcript.is_empty() {
                    out.push(Ok(Event::Transcript(transcript.to_string())));
                }
            }
            if !self.emitted_any {
                out.push(Err("empty reply".into()));
            }
        }
        let _ = self.showed;
        out
    }

    fn route_first(
        &mut self,
        full: &str,
        effective: usize,
        done: bool,
        out: &mut Vec<Result<Event, String>>,
    ) -> bool {
        let lead = full[self.delivered..effective].trim_start();
        if !lead.starts_with(SHOW_OPEN) {
            if lead.is_empty() && !done {
                return false;
            }
            self.route_checked = true;
            return true;
        }
        let Some(close_rel) = lead.find(SHOW_CLOSE) else {
            if done {
                out.push(Err("unfinished conjuring directive".into()));
            }
            return false;
        };
        let inner = &lead[SHOW_OPEN.len_utf8()..close_rel];
        self.route_checked = true;
        self.emitted_any = true;
        self.delivered = effective;
        out.push(route_directive(inner, &self.catalog_ids));
        true
    }
}

fn route_directive(inner: &str, catalog_ids: &[u64]) -> Result<Event, String> {
    let directive = inner.trim().to_ascii_lowercase();
    match directive.as_str() {
        "tasks" | "task" => Ok(Event::TaskList),
        "todos" | "todo" => Ok(Event::TodoList),
        "fonts" | "font" => Ok(Event::FontList),
        "settings" | "setting" => Ok(Event::Settings),
        "history" | "histories" => Ok(Event::HistoryList),
        "help" | "manual" => Ok(Event::Help),
        "read" | "reader" => Ok(Event::Reader(None)),
        "refresh" => Ok(Event::FullRefresh),
        _ => {
            if let Some(title) = directive
                .strip_prefix("read:")
                .or_else(|| directive.strip_prefix("reader:"))
                .map(str::trim)
                .filter(|title| !title.is_empty())
            {
                return Ok(Event::Reader(Some(title.to_string())));
            }
            let number = directive
                .strip_prefix("show")
                .map(|rest| rest.trim_start_matches([':', ' ']))
                .and_then(|rest| rest.trim().parse::<usize>().ok());
            number
                .and_then(|number| catalog_ids.get(number.wrapping_sub(1)).copied())
                .map(Event::Show)
                .ok_or_else(|| format!("the diary lost that page ({inner})"))
        }
    }
}

/// Trim and strip stray surrounding quotes from a reply fragment.
pub(super) fn clean(s: &str) -> String {
    let trimmed = s.trim();
    let trimmed = trimmed.strip_prefix('"').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('"').unwrap_or(trimmed);
    trimmed.to_string()
}

/// Strip any directive spans that appear after prose so they never become
/// visible ink.
pub(super) fn strip_directives(s: &str) -> String {
    if !s.contains(SHOW_OPEN) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find(SHOW_OPEN) {
        out.push_str(&rest[..open]);
        match rest[open..].find(SHOW_CLOSE) {
            Some(close) => rest = &rest[open + close + SHOW_CLOSE.len_utf8()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// End of the last complete sentence after `from`, without prematurely
/// splitting decimals or a closing Chinese quote still in flight.
fn sentence_cut(text: &str, from: usize) -> Option<usize> {
    let tail = text.get(from..)?;
    let mut cut = None;
    for (index, character) in tail.char_indices() {
        if matches!(character, '。' | '！' | '？') {
            let end = index + character.len_utf8();
            if let Some(next) = tail[end..].chars().next() {
                let quoted_end = if matches!(next, '”' | '’' | '」' | '』' | '》' | '〉') {
                    end + next.len_utf8()
                } else {
                    end
                };
                if quoted_end >= 4 {
                    cut = Some(from + quoted_end);
                }
            }
        } else if matches!(character, '.' | '!' | '?' | '…') {
            let end = index + character.len_utf8();
            if tail[end..].chars().next().is_some_and(char::is_whitespace) && end >= 4 {
                cut = Some(from + end);
            }
        }
    }
    cut
}
