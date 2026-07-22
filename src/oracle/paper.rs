//! Semantic cleanup for prose that will be handwritten onto physical paper.

use super::{extract_assistant_text, json_quote};

/// Does the visible draft contain screen-oriented formatting that should be
/// rewritten semantically before it reaches physical paper?
pub(super) fn paper_answer_needs_rewrite(full: &str) -> bool {
    let visible = full.split_once('\u{2042}').map(|p| p.0).unwrap_or(full);
    let lower = visible.to_ascii_lowercase();
    lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("www.")
        || visible.contains("](")
        || visible.contains("**")
        || visible.contains("```")
        || visible.contains("cite")
}

/// Last-resort paper rendering when the semantic editor is unavailable.
/// The model remains the primary formatter; this fallback preserves link
/// labels and prose while removing only transport-oriented markup so a valid
/// answer is never replaced by a generic network error.
pub(super) fn paper_safe_fallback(draft: &str) -> String {
    let mut text = draft.replace("**", "").replace("```", "");
    while let Some(marker) = text.find("cite") {
        let end = text[marker..]
            .find('')
            .map_or(text.len(), |offset| marker + offset + ''.len_utf8());
        text.replace_range(marker..end, "");
    }
    while let Some(close_label) = text.find("](") {
        let Some(open_label) = text[..close_label].rfind('[') else {
            break;
        };
        let Some(close_url) = text[close_label + 2..].find(')') else {
            break;
        };
        let close_url = close_label + 2 + close_url;
        let label = text[open_label + 1..close_label].to_owned();
        text.replace_range(open_label..=close_url, &label);
    }
    for prefix in ["https://", "http://", "www."] {
        while let Some(start) = text.to_ascii_lowercase().find(prefix) {
            let end = text[start..]
                .char_indices()
                .skip(1)
                .find(|(_, character)| {
                    character.is_whitespace()
                        || matches!(
                            character,
                            '，' | '。' | '、' | '；' | '！' | '？' | ')' | '）' | ']' | '】'
                        )
                })
                .map_or(text.len(), |(offset, _)| start + offset);
            text.replace_range(start..end, "相關資料");
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A rare second pass: rewrite only the not-yet-inked tail as coherent paper
/// prose. A clean prefix may already be on paper, so it is context only and
/// must never be repeated.
pub(super) fn rewrite_paper_tail(
    agent: &ureq::Agent,
    base: &str,
    key: &str,
    model: &str,
    written_prefix: &str,
    draft_tail: &str,
) -> Result<String, String> {
    let instructions = "Rewrite only the remaining draft into the continuation that will be handwritten on physical paper. Preserve every fact, calculation, source name, and intended answer, but make it natural and concise. Text already written is context only: do not repeat or contradict it. Output only the rewritten continuation: no URL, Markdown, citation marker, reference list, search discussion, heading, or commentary. Use Traditional Chinese when the draft is Chinese.";
    let input = format!(
        "Text already written on paper:\n{}\n\nRemaining draft to rewrite:\n{}",
        written_prefix.trim(),
        draft_tail.trim(),
    );
    let body = format!(
        concat!(
            "{{\"model\":{},\"stream\":false,\"store\":false,",
            "\"max_output_tokens\":600,\"reasoning\":{{\"effort\":\"none\"}},",
            "\"instructions\":{},\"input\":{}}}"
        ),
        json_quote(model),
        json_quote(instructions),
        json_quote(&input),
    );
    let response = agent
        .post(&format!("{base}/responses"))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, response) => format!(
                "http {code}: {}",
                response.into_string().unwrap_or_default().trim()
            ),
            other => other.to_string(),
        })?
        .into_string()
        .map_err(|e| e.to_string())?;
    let rewritten = extract_assistant_text(&response)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "paper editor returned no answer".to_string())?;
    Ok(rewritten.trim().to_string())
}
