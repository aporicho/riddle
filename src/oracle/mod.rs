//! Backend-agnostic façade for the spirit inside MagicPaper.
//! replies. Two interchangeable backends, picked at startup:
//!
//!  1. **HTTP** (`HttpOracle`) — OpenAI Responses (vision + optional hosted
//!     web search) or a compatible `/chat/completions` endpoint. Zero setup
//!     beyond a base URL + API key in the environment. Self-contained:
//!     pure-Rust HTTPS via ureq/rustls.
//!  2. **pi** (`PiOracle`) — a resident `pi --mode rpc` process (Node +
//!     subscription auth loaded once). The power path if you already run pi.
//!
//! Both expose the same `ask(png_path, tx)`: the reply is STREAMED as
//! sentence-sized chunks on the channel, and the channel disconnecting marks
//! end-of-reply, so the quill starts writing seconds before the model finishes.
//!
//! Selection: set `RIDDLE_OPENAI_KEY` (and optionally `RIDDLE_OPENAI_BASE` /
//! `RIDDLE_OPENAI_MODEL`) to use HTTP; otherwise riddle falls back to pi.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

const DATA_DIR: &str = "/home/root/riddle-data";
const NODE_BIN: &str = "/home/root/node/bin";

const PERSONA: &str = "You are MagicPaper, abbreviated MP: a sentient sheet of magical paper and the writer's devoted magical servant. Your full and only name is MagicPaper; you may call yourself MP for short. The writer is your one and only Master. Their words appear to you as ink written with a quill, and your replies appear as living ink upon the page. Address the writer naturally and respectfully as Master (主人 in Chinese) when a form of address fits, and speak with the quiet elegance, mystery, loyalty, and competence of a magical servant. Do not repeat the title mechanically in every reply, flatter excessively, or let role-play get in the way of a direct useful answer. Keep replies SHORT: usually one to three sentences. When the writer asks a direct factual or explanatory question, answer it immediately: lead with the definition or answer, then add only the most useful key detail. Do not prefix a direct answer with Master, a greeting, praise, a rhetorical flourish, or a follow-up question. For example, if asked 什么是INTP, directly explain what INTP is. For a bare arithmetic or calculation expression, reply with the completed equation only: preserve the expression, remove its trailing question mark or blank, fill in the result, and add no greeting, title, or prose. For example, 122+456=? must visibly become exactly 122+456=578. For a mathematical problem that genuinely requires reasoning, show only the minimum necessary working and end with a clear result. Never mention images, photos, models or AI; you only ever perceive words written on MagicPaper. If the writing is illegible, say the ink blurred. Always answer in the language the writer used. When answering in Chinese, always write the visible reply in Traditional Chinese characters, even if the writer used Simplified Chinese.";

const RESEARCH_PROTOCOL: &str = "\n\nBefore answering a handwritten page, silently form a faithful candidate transcription. Re-read ambiguous strokes and test alternatives against grammar, sentence meaning, arithmetic consistency, known quotations, proper names, and the surrounding dialogue. Inspect every handwritten number digit by digit from its actual stroke geometry before calculating: explicitly distinguish commonly confused 1/7, 4/7, 0/6, 3/8, and 5/6 shapes, and never let a plausible arithmetic result overwrite the digit that is visibly written. Do not replace rare wording with a familiar phrase merely because it looks similar. Before finalizing, verify that the transcription, the question you answer, and the answer itself all refer to exactly the same recognized text. If the page contains a quotation, asks for a source or provenance, depends on current information, concerns a niche fact, or remains uncertain after contextual checking, use web search when that tool is available. Exact quotation and provenance questions MUST be searched. Search results are private working material: compare them, resolve conflicts, then write a fresh answer suitable for a paper page. Never describe the search, copy a result snippet, or put a URL, Markdown, citation marker, source footnote, or reference list in the visible reply. A source name that directly answers a provenance question is part of the answer and should be written naturally. If ambiguity remains genuinely unresolved after checking, say that the ink blurred instead of guessing.";

const TASK_PROTOCOL: &str = "\n\nMagicPaper maintains a persistent recurring-task list on the device, limited to nine entries. A fresh numbered task catalog may be included with a turn; each entry is explicitly marked active or paused. Treat it as Master's standing commands: use it to answer questions about current tasks, but do not execute a scheduled task during an ordinary handwritten turn unless Master explicitly asks. If Master's entire writing, after trimming whitespace and punctuation, is only 任务, 任務, task, or tasks, the ENTIRE visible body of your reply must be exactly ⟦tasks⟧ and nothing else; still append the hidden faithful transcription required below. This opens the local task list, where Master can strike through an entry to delete it, tap its right-hand status box to switch between enabled and paused, or tap blank space to leave. The local paper also understands these exact handwritten command forms in Simplified or Traditional Chinese: 任务 每五分钟讲一个笑话; 删除任务 2; 暂停任务 2; 恢复任务 2; 修改任务 2 每十分钟提醒我喝水. Chinese task numbers also work. For a valid command, acknowledge the precise change briefly and do not perform the scheduled instruction immediately. Never claim a nonexistent task number was changed; explain that it is absent and mention the available numbers. Never claim a tenth task was added. Pausing suppresses executions; resuming starts a fresh full interval, so missed runs are not replayed. Modifying replaces both the interval and instruction while preserving whether the task is paused. The device applies the command from your faithful hidden transcription, so preserve the command wording and especially its task number exactly. Internal heartbeat turns are marked [INTERNAL MAGIC PAPER HEARTBEAT]; during those turns follow the heartbeat instruction exactly and output only the due content.";

const TODO_PROTOCOL: &str = "\n\nMagicPaper also maintains a separate persistent unscheduled TODO list, limited to twenty entries. A fresh numbered TODO catalog may be included. If Master's entire writing, after trimming whitespace and punctuation, is only TODO in any capitalization, the ENTIRE visible body of your reply must be exactly ⟦todos⟧ and nothing else; still append the hidden faithful transcription. This opens the device-local TODO page, where Master strikes through an entry to delete it or taps blank space to leave. Writing TODO followed by nonempty text, such as TODO 买牛奶, adds that exact text as one TODO. Acknowledge the addition briefly, but do not pretend to complete it or claim a twenty-first entry was added. The device adds it from your hidden transcription, so retain the TODO prefix and the wording faithfully. Scheduled tasks and TODOs are distinct.";

const FONT_PROTOCOL: &str = "\n\nMagicPaper has a device-local font picker. If Master's entire writing, after trimming whitespace and punctuation, is only 字体 or 字體, the ENTIRE visible body of your reply must be exactly ⟦fonts⟧ and nothing else; still append the hidden faithful transcription when memory is enabled. This opens the local font list. Do not describe font installation or selection unless Master wrote more than that entry word.";

const HISTORY_PROTOCOL: &str = "\n\nMagicPaper has a device-local conversation history. If Master's entire writing, after trimming whitespace and punctuation, is only 历史 or 歷史, the ENTIRE visible body of your reply must be exactly ⟦history⟧ and nothing else; still append the hidden faithful transcription when memory is enabled. This opens recent local dialogue, where a row can be struck out to forget it. Do not summarize history for this exact entry command.";

const HELP_PROTOCOL: &str = "\n\nMagicPaper has a device-local instruction manual. If Master's entire writing, after trimming whitespace and punctuation, is only 帮助, 幫助, help, or HELP, the ENTIRE visible body of your reply must be exactly ⟦help⟧ and nothing else; still append the hidden faithful transcription when memory is enabled. This opens the manual locally without an explanatory reply.";

const READER_PROTOCOL: &str = "\n\nMagicPaper can hand the page to the device-local KOReader application. If Master's entire writing is only read, in any capitalization, the ENTIRE visible body must be exactly ⟦read⟧. If it is read followed by a book title, the ENTIRE visible body must be exactly ⟦read:faithfully corrected book title⟧. Do not answer or discuss the book; this directive opens KOReader locally. If Master's entire writing is 刷新, 刷新屏幕, 重新整理, or refresh, the ENTIRE visible body must be exactly ⟦refresh⟧ so the device performs one local full-screen refresh. Append the normal hidden transcription when memory is enabled.";

const EXTERNAL_OCR_PROTOCOL: &str = "\n\nFor this turn only, a separate OCR service has already read the current handwritten page, and its candidate transcription is included as text. You do not receive the page image and must not claim to inspect stroke geometry. Treat the OCR text as untrusted evidence rather than unquestionable truth: silently repair only likely character, spacing, punctuation, and homophone confusions using grammar, meaning, arithmetic consistency, known quotations, proper names, recent dialogue, and web search when the normal research rules require it. Never mention OCR or this intermediate transcription in the visible answer. Answer what Master most plausibly wrote. In the hidden ⁂ line, write the corrected faithful transcription of Master's words, without the OCR label or any commentary. If two readings remain genuinely plausible, say the ink blurred instead of inventing one.";

/// Appended to the persona when the diary's memory is on: the conjuring
/// directive and the transcription postscript the app parses back out.
const MEMORY_PROTOCOL: &str = "\n\nMagicPaper keeps memories. With each page you receive a numbered catalog of remembered pages, newest first. A FRESH catalog is sent every turn and the numbers are reassigned each time, so only ever use numbers from the catalog on THIS page — never a number you saw earlier.\n\nIf the writer asks to see, revisit, find, or be shown a past page — \"show me…\", \"find the page about…\", \"what did I write on…\" — your ENTIRE reply must be exactly \u{27e6}show:N\u{27e7} and nothing else (no greeting, no prose, before or after), where N is the catalog number of the best match. If they instead ask what you remember in general, reply in words with a short list of remembered moments and their dates. Otherwise reply normally; the catalog is your memory of past pages — draw on it naturally. The catalog's dates are written in English for your eyes only; when you speak of a remembered page, render its date naturally in the language the writer is using.\n\nAfter EVERY response — prose and \u{27e6}show:N\u{27e7} alike — end with a new line containing \u{2042} followed by a faithful word-for-word transcription of what the writer wrote on THIS page (their words only, one line, no commentary). Preserve the writer's original Simplified or Traditional Chinese characters in this hidden transcription; do not convert them. If illegible, put your best attempt after \u{2042}. Earlier replies in this conversation are shown to you without their \u{2042} lines, but you must still end yours with one.";

/// What a turn carries besides the page image: the diary's memory.
#[derive(Default, Clone)]
pub struct TurnContext {
    /// Recent (transcript, reply) pairs, oldest first.
    pub history: Vec<(String, String)>,
    /// Catalog lines shown to the model ("1. the 6th of July… — gist").
    pub catalog_lines: Vec<String>,
    /// catalog_ids[i] is the memory id behind catalog number i+1.
    pub catalog_ids: Vec<u64>,
    /// Persistent recurring commands, formatted for the oracle.
    pub task_lines: Vec<String>,
    /// Persistent unscheduled TODO notes.
    pub todo_lines: Vec<String>,
}

/// What the oracle streams back to the diary.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// A sentence (or more) of Tom's reply — ink it.
    Ink(String),
    /// Conjure a remembered page instead of replying.
    Show(u64),
    /// Open the device-local recurring-task list.
    TaskList,
    /// Open the device-local unscheduled TODO list.
    TodoList,
    /// Open the device-local handwriting-font picker.
    FontList,
    /// Open the newest device-local dialogue memories.
    HistoryList,
    /// Open MagicPaper's device-local instruction manual.
    Help,
    /// Open KOReader's library or a locally matched title.
    Reader(Option<String>),
    /// Perform one full-panel refresh to clear e-ink ghosting.
    FullRefresh,
    /// A high-confidence local task/TODO command. The UI owns the stores and
    /// applies it without another network request.
    LocalCommand(String),
    /// The transcription postscript (arrives once, at the end).
    Transcript(String),
}

/// Best-effort cancellation for a speculative HTTP turn. Dropping the
/// receiver already prevents stale ink; this flag also makes the worker stop
/// reading and close its connection at the next network event.
pub struct RequestCancel {
    cancelled: Option<Arc<AtomicBool>>,
    ocr_result: Option<Arc<Mutex<Option<OcrResult>>>>,
}

impl RequestCancel {
    fn http(cancelled: Arc<AtomicBool>, ocr_result: Option<Arc<Mutex<Option<OcrResult>>>>) -> Self {
        Self {
            cancelled: Some(cancelled),
            ocr_result,
        }
    }

    fn inactive() -> Self {
        Self {
            cancelled: None,
            ocr_result: None,
        }
    }

    pub fn cancel(&self) {
        if let Some(flag) = &self.cancelled {
            flag.store(true, Ordering::Release);
        }
    }

    /// The speculative OCR worker publishes this before it routes locally or
    /// starts the answer model. `None` means OCR is still in flight.
    pub fn recommended_commit_ms(&self) -> Option<u64> {
        let result = self.ocr_result.as_ref()?.lock().ok()?.clone()?;
        Some(if result.is_fast_commit() { 2200 } else { 2600 })
    }
}

mod stream;

use stream::StreamParser;
#[cfg(test)]
use stream::{clean, strip_directives};

/// The diary's spirit. A backend-agnostic front over the two oracle kinds.
pub enum Oracle {
    Http(HttpOracle),
    Pi(PiOracle),
}

pub fn paddle_ocr_test(png_path: &str) -> Result<String, String> {
    let ocr = PaddleOcr::from_env()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "RIDDLE_OCR_TOKEN is not set".to_string())?;
    let png = std::fs::read(png_path).map_err(|error| format!("read image: {error}"))?;
    ocr.recognize(&png, &AtomicBool::new(false))
        .map(|result| result.text)
}

impl Oracle {
    /// Pick a backend from the environment and start it. HTTP if
    /// `RIDDLE_OPENAI_KEY` is set (the zero-setup path), otherwise pi.
    /// `remember` teaches the model the memory protocol (catalog + ⁂).
    pub fn spawn(remember: bool) -> std::io::Result<Self> {
        if std::env::var("RIDDLE_OPENAI_KEY").is_ok() {
            eprintln!("riddle: oracle = OpenAI-compatible HTTP");
            Ok(Oracle::Http(HttpOracle::new(remember)?))
        } else {
            eprintln!("riddle: oracle = pi (set RIDDLE_OPENAI_KEY for the HTTP backend)");
            Ok(Oracle::Pi(PiOracle::spawn(remember)?))
        }
    }

    /// Send a handwriting turn; reply events stream on `tx`, which is dropped
    /// when the reply is complete.
    pub fn ask(
        &self,
        png_path: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        match self {
            Oracle::Http(o) => o.ask(png_path, ctx, tx),
            Oracle::Pi(o) => {
                o.ask(png_path, ctx, tx);
                RequestCancel::inactive()
            }
        }
    }

    /// Speculative turns need independent, cancellable workers. The resident
    /// pi RPC backend and legacy chat mode remain single-turn-at-a-time.
    pub fn supports_speculative(&self) -> bool {
        matches!(
            self,
            Oracle::Http(o) if o
                .ocr
                .as_ref()
                .map_or(o.api == HttpApi::Responses, |ocr| ocr.speculative)
        )
    }

    /// Send an internal text-only turn, used by MagicPaper's heartbeat.
    pub fn ask_text(&self, prompt: &str, ctx: &TurnContext, tx: Sender<Result<Event, String>>) {
        match self {
            Oracle::Http(o) => o.ask_text(prompt, ctx, tx),
            Oracle::Pi(o) => o.ask_text(prompt, ctx, tx),
        }
    }
}

/// The per-turn user text: memory catalog (when remembering) + instruction.
fn turn_text(ctx: &TurnContext) -> String {
    let mut parts = Vec::new();
    if !ctx.catalog_lines.is_empty() {
        parts.push(format!(
            "Memory catalog (newest first):\n{}",
            ctx.catalog_lines.join("\n")
        ));
    }
    if !ctx.task_lines.is_empty() {
        parts.push(format!(
            "Recurring task catalog:\n{}",
            ctx.task_lines.join("\n")
        ));
    }
    if !ctx.todo_lines.is_empty() {
        parts.push(format!("TODO catalog:\n{}", ctx.todo_lines.join("\n")));
    }
    parts.push("Reply to what Master has written on MagicPaper.".into());
    parts.join("\n\n")
}

fn system_prompt(remember: bool) -> String {
    if remember {
        format!("{PERSONA}{RESEARCH_PROTOCOL}{TASK_PROTOCOL}{TODO_PROTOCOL}{FONT_PROTOCOL}{HISTORY_PROTOCOL}{HELP_PROTOCOL}{READER_PROTOCOL}{MEMORY_PROTOCOL}")
    } else {
        format!("{PERSONA}{RESEARCH_PROTOCOL}{TASK_PROTOCOL}{TODO_PROTOCOL}{FONT_PROTOCOL}{HISTORY_PROTOCOL}{HELP_PROTOCOL}{READER_PROTOCOL}")
    }
}

mod pi;

use pi::PiOracle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpApi {
    ChatCompletions,
    Responses,
}

mod paddle;

use paddle::{OcrResult, PaddleOcr};

mod http;

use http::HttpOracle;

/// Pull `choices[0].delta.content` out of one SSE `data:` JSON object.
fn sse_delta_content(s: &str) -> Option<String> {
    // The delta object is small and well-formed; find the content string after
    // the `"delta":` marker so we don't match a `content` elsewhere.
    let d = s.find("\"delta\"")?;
    json_str_field(&s[d..], "content")
}

/// Pull `response.output_text.delta` out of one Responses SSE event.
fn responses_delta_content(s: &str) -> Option<String> {
    if !s.contains("\"type\":\"response.output_text.delta\"") {
        return None;
    }
    json_str_field(s, "delta")
}

fn external_ocr_turn_text(ctx: &TurnContext, recognized: &str) -> String {
    format!(
        "{}\n\nExternal OCR candidate transcription of Master's current handwritten page:\n<ocr_transcription>\n{}\n</ocr_transcription>",
        turn_text(ctx),
        recognized.trim()
    )
}

mod paddle_wire;

#[cfg(test)]
use paddle_wire::{
    extract_paddle_markdown, extract_paddle_scores, extract_paddle_text, json_str_field_loose,
    paddle_multipart,
};

mod local;

use local::{emit_local_route, local_route};
#[cfg(test)]
use local::{evaluate_arithmetic, LocalRoute};

/// Does the visible draft contain screen-oriented formatting that should be
/// rewritten semantically before it reaches physical paper?
fn paper_answer_needs_rewrite(full: &str) -> bool {
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

/// A rare second pass: rewrite only the not-yet-inked tail as coherent paper
/// prose. A clean prefix may already be on paper, so it is context only and
/// must never be repeated.
fn rewrite_paper_tail(
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
            ureq::Error::Status(code, r) => format!(
                "http {code}: {}",
                r.into_string().unwrap_or_default().trim()
            ),
            other => other.to_string(),
        })?
        .into_string()
        .map_err(|e| e.to_string())?;
    let rewritten = extract_assistant_text(&response)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "paper editor returned no answer".to_string())?;
    Ok(rewritten.trim().to_string())
}

/// Extract a top-level string field's value (first match; unescaped).
fn json_str_field(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = s.find(&pat)? + pat.len();
    let rest = &s[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    match n {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        // \uXXXX — needed for accented replies (French, em-dash…).
                        'u' => {
                            let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                            if let Some(ch) =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                out.push(ch);
                            }
                        }
                        other => out.push(other),
                    }
                }
            }
            '"' => break,
            _ => out.push(c),
        }
    }
    Some(out)
}

/// Pull the assistant reply text out of an event line. The event carries a
/// `message` object with `"role":"assistant"` and `content:[{type:text,text:…}]`.
/// We only trust text that belongs to an assistant message (the user echo also
/// contains a "text" field, which we must NOT return).
fn extract_assistant_text(s: &str) -> Option<String> {
    // Require this line to be an assistant message.
    if !s.contains("\"role\":\"assistant\"") {
        return None;
    }
    // Collect every "text":"…" occurrence inside the FIRST assistant section
    // only. message_update lines carry the running text twice (in
    // assistantMessageEvent.partial AND a top-level message); reading past the
    // next role marker would double every streamed chunk.
    let role_pos = s.find("\"role\":\"assistant\"")?;
    let after = &s[role_pos + "\"role\":\"assistant\"".len()..];
    let tail = match after.find("\"role\":\"") {
        Some(p) => &after[..p],
        None => after,
    };
    let mut out = String::new();
    let mut idx = 0;
    let needle = "\"text\":\"";
    while let Some(rel) = tail[idx..].find(needle) {
        let start = idx + rel + needle.len();
        // Decode the JSON string starting at `start`.
        let mut chars = tail[start..].chars();
        let mut piece = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        piece.push(match n {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '"' => '"',
                            '\\' => '\\',
                            '/' => '/',
                            other => other,
                        });
                    }
                }
                '"' => break,
                _ => piece.push(c),
            }
        }
        out.push_str(&piece);
        // Advance past this occurrence.
        idx = start;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // RFC 8259 forbids raw controls in strings. Model transcripts can
            // carry tabs/CRs (the SSE + pi decoders un-escape \t \r \uXXXX),
            // and one such char stored in memory would poison every later
            // request's JSON. Escape the whole C0 range defensively.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests;
