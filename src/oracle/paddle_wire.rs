//! PaddleOCR multipart encoding and lightweight JSONL extraction.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

pub(super) fn paddle_multipart(png: &[u8], model: &str, boundary: &str) -> Vec<u8> {
    // The general OCR pipeline and the VL document pipeline accept different
    // optional switches on the same AI Studio jobs endpoint.
    let optional = if model.to_ascii_lowercase().starts_with("pp-ocr") {
        r#"{"useDocOrientationClassify":false,"useDocUnwarping":false,"useTextlineOrientation":false}"#
    } else {
        r#"{"useDocOrientationClassify":false,"useDocUnwarping":false,"useChartRecognition":false}"#
    };
    let mut body = Vec::with_capacity(png.len() + 1024);
    let fields = [("model", model), ("optionalPayload", optional)];
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"magicpaper.png\"\r\nContent-Type: image/png\r\n\r\n",
    );
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

pub(super) fn paddle_http_error(stage: &str, error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let mut detail = response.into_string().unwrap_or_default();
            if detail.len() > 600 {
                detail.truncate(600);
            }
            format!("PaddleOCR {stage} http {code}: {}", detail.trim())
        }
        other => format!("PaddleOCR {stage} request failed: {other}"),
    }
}

pub(super) fn sleep_cancellable(duration: std::time::Duration, cancelled: &AtomicBool) {
    let until = std::time::Instant::now() + duration;
    while !cancelled.load(Ordering::Acquire) && std::time::Instant::now() < until {
        let left = until.saturating_duration_since(std::time::Instant::now());
        thread::sleep(left.min(std::time::Duration::from_millis(50)));
    }
}

/// JSON field reader for ordinary API responses, tolerating whitespace around
/// the colon. The SSE reader below keeps its faster compact-JSON helper.
pub(super) fn json_str_field_loose(s: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let mut search = s;
    while let Some(position) = search.find(&pattern) {
        let after_key = &search[position + pattern.len()..];
        let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
        if let Some(json_string) = after_colon.strip_prefix('"') {
            return Some(decode_json_string(json_string));
        }
        search = &after_key[after_key.len().min(1)..];
    }
    None
}

fn decode_json_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000c}'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    out
}

pub(super) fn extract_paddle_markdown(jsonl: &str) -> String {
    let mut pages = Vec::new();
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let mut rest = line;
        while let Some(markdown) = rest.find("\"markdown\"") {
            let section = &rest[markdown + "\"markdown\"".len()..];
            if let Some(text) = json_str_field_loose(section, "text") {
                if !text.trim().is_empty() {
                    pages.push(text.trim().to_string());
                }
            }
            rest = &section[section.len().min(1)..];
        }
    }
    pages.join("\n")
}

/// Extract the ordered recognition strings returned by PP-OCRv6. Its result
/// schema is `ocrResults[].prunedResult.rec_texts`, unlike PaddleOCR-VL's
/// `layoutParsingResults[].markdown.text`.
fn extract_paddle_rec_texts(jsonl: &str) -> String {
    let pattern = "\"rec_texts\"";
    let mut texts = Vec::new();
    let mut rest = jsonl;
    while let Some(position) = rest.find(pattern) {
        let after_key = &rest[position + pattern.len()..];
        let Some(after_colon) = after_key.trim_start().strip_prefix(':') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        let Some(mut array) = after_colon.trim_start().strip_prefix('[') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        loop {
            array = array.trim_start();
            if let Some(next) = array.strip_prefix(',') {
                array = next;
                continue;
            }
            if array.starts_with(']') || array.is_empty() {
                break;
            }
            let Some(json_string) = array.strip_prefix('"') else {
                break;
            };
            let Some((value, consumed)) = take_json_string(json_string) else {
                break;
            };
            if !value.trim().is_empty() {
                texts.push(value.trim().to_string());
            }
            array = &json_string[consumed..];
        }
        rest = &after_key[after_key.len().min(1)..];
    }
    texts.join("\n")
}

/// Decode a JSON string whose opening quote has already been consumed and
/// report the number of source bytes through its closing quote.
fn take_json_string(s: &str) -> Option<(String, usize)> {
    let mut escaped = false;
    for (index, ch) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some((decode_json_string(s), index + ch.len_utf8()));
        }
    }
    None
}

pub(super) fn extract_paddle_text(jsonl: &str) -> String {
    let rec_texts = extract_paddle_rec_texts(jsonl);
    if !rec_texts.is_empty() {
        rec_texts
    } else {
        extract_paddle_markdown(jsonl)
    }
}

pub(super) fn extract_paddle_scores(jsonl: &str) -> Vec<f32> {
    let pattern = "\"rec_scores\"";
    let mut scores = Vec::new();
    let mut rest = jsonl;
    while let Some(position) = rest.find(pattern) {
        let after_key = &rest[position + pattern.len()..];
        let Some(after_colon) = after_key.trim_start().strip_prefix(':') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        let Some(array) = after_colon.trim_start().strip_prefix('[') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        if let Some(end) = array.find(']') {
            scores.extend(
                array[..end]
                    .split(',')
                    .filter_map(|value| value.trim().parse::<f32>().ok()),
            );
            rest = &array[end + 1..];
        } else {
            break;
        }
    }
    scores
}
