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

        if !self.route_checked {
            let lead = full[self.delivered..effective].trim_start();
            if lead.starts_with(SHOW_OPEN) {
                let Some(close_rel) = lead.find(SHOW_CLOSE) else {
                    if !done {
                        return out;
                    }
                    out.push(Err("unfinished conjuring directive".into()));
                    return out;
                };
                let inner = &lead[SHOW_OPEN.len_utf8()..close_rel];
                self.route_checked = true;
                self.emitted_any = true;
                self.delivered = effective;
                let directive = inner.trim().to_ascii_lowercase();
                if directive == "tasks" || directive == "task" {
                    out.push(Ok(Event::TaskList));
                } else if directive == "todos" || directive == "todo" {
                    out.push(Ok(Event::TodoList));
                } else if directive == "fonts" || directive == "font" {
                    out.push(Ok(Event::FontList));
                } else if directive == "history" || directive == "histories" {
                    out.push(Ok(Event::HistoryList));
                } else if directive == "help" || directive == "manual" {
                    out.push(Ok(Event::Help));
                } else {
                    let n: Option<usize> = directive
                        .strip_prefix("show")
                        .map(|rest| rest.trim_start_matches([':', ' ']))
                        .and_then(|rest| rest.trim().parse().ok());
                    match n.and_then(|n| self.catalog_ids.get(n.wrapping_sub(1)).copied()) {
                        Some(id) => out.push(Ok(Event::Show(id))),
                        None => out.push(Err(format!("the diary lost that page ({inner})"))),
                    }
                }
            } else if lead.is_empty() {
                if !done {
                    return out;
                }
                self.route_checked = true;
            } else {
                self.route_checked = true;
            }
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
