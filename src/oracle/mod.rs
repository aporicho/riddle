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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

pub(super) fn log_llm_terminal(
    terminal: &AtomicBool,
    request_id: u64,
    domain: &str,
    outcome: &str,
    stage: &str,
    latency_ms: Option<u128>,
) -> bool {
    if terminal
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    match latency_ms {
        Some(latency_ms) => eprintln!(
            "magic-paper: event=llm-{outcome} request={}:{} domain={domain} stage={stage} latency_ms={latency_ms}",
            std::process::id(),
            request_id,
        ),
        None => eprintln!(
            "magic-paper: event=llm-{outcome} request={}:{} domain={domain} stage={stage}",
            std::process::id(),
            request_id,
        ),
    }
    true
}

const DATA_DIR: &str = "/home/root/riddle-data";
const NODE_BIN: &str = "/home/root/node/bin";

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
    /// A sentence (or more) of MP's reply — ink it.
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
    request_id: u64,
    domain: &'static str,
    cancelled: Option<Arc<AtomicBool>>,
    terminal: Option<Arc<AtomicBool>>,
    ocr_result: Option<Arc<Mutex<Option<OcrResult>>>>,
}

impl RequestCancel {
    fn http(
        request_id: u64,
        domain: &'static str,
        cancelled: Arc<AtomicBool>,
        terminal: Arc<AtomicBool>,
        ocr_result: Option<Arc<Mutex<Option<OcrResult>>>>,
    ) -> Self {
        Self {
            request_id,
            domain,
            cancelled: Some(cancelled),
            terminal: Some(terminal),
            ocr_result,
        }
    }

    fn inactive(request_id: u64, domain: &'static str) -> Self {
        Self {
            request_id,
            domain,
            cancelled: None,
            terminal: None,
            ocr_result: None,
        }
    }

    fn pi(request_id: u64, domain: &'static str, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            request_id,
            domain,
            cancelled: Some(cancelled),
            terminal: Some(Arc::new(AtomicBool::new(false))),
            ocr_result: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn testing(request_id: u64, cancelled: Arc<AtomicBool>) -> Self {
        Self::http(
            request_id,
            "test",
            cancelled,
            Arc::new(AtomicBool::new(false)),
            None,
        )
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Returns true only for the first effective cancellation.  Callers use
    /// this to keep structured cancellation logs free of duplicate noise.
    pub fn cancel(&self) -> bool {
        self.cancel_with_reason("caller")
    }

    pub fn cancel_with_reason(&self, reason: &str) -> bool {
        if let Some(flag) = &self.cancelled {
            let first = !flag.swap(true, Ordering::AcqRel);
            if first {
                if let Some(terminal) = &self.terminal {
                    log_llm_terminal(
                        terminal,
                        self.request_id,
                        self.domain,
                        "cancelled",
                        reason,
                        None,
                    );
                }
            }
            return first;
        }
        false
    }

    /// The speculative OCR worker publishes this before it routes locally or
    /// starts the answer model. `None` means OCR is still in flight.
    pub fn recommended_commit_ms(&self) -> Option<u64> {
        let result = self.ocr_result.as_ref()?.lock().ok()?.clone()?;
        Some(if result.is_fast_commit() { 2200 } else { 2600 })
    }
}

mod codec;
mod deterministic;
mod prompts;
mod stream;

use codec::{
    base64, extract_assistant_text, json_quote, json_str_field, responses_delta_content,
    sse_delta_content,
};

use prompts::{external_ocr_turn_text, system_prompt, turn_text, EXTERNAL_OCR_PROTOCOL};

use deterministic::DeterministicOracle;
use stream::StreamParser;
#[cfg(test)]
use stream::{clean, strip_directives};

/// The diary's spirit. A backend-agnostic front over the two oracle kinds.
pub enum Oracle {
    Http(Box<HttpOracle>),
    Pi(PiOracle),
    Deterministic(DeterministicOracle),
}

pub(super) fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(trim_nonempty)
}

fn trim_nonempty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn paddle_ocr_test(png_path: &str) -> Result<String, String> {
    let ocr = PaddleOcr::from_env()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "RIDDLE_OCR_TOKEN is not set".to_string())?;
    let png = std::fs::read(png_path).map_err(|error| format!("read image: {error}"))?;
    ocr.recognize(0, "cli", &png, &AtomicBool::new(false))
        .map(|result| result.text)
}

impl Oracle {
    /// Pick a backend from the environment and start it. HTTP if
    /// `RIDDLE_OPENAI_KEY` is set (the zero-setup path), otherwise pi.
    /// `remember` teaches the model the memory protocol (catalog + ⁂).
    pub fn spawn(remember: bool) -> std::io::Result<Self> {
        Self::spawn_for_mode(remember, crate::runtime_env::test_mode())
    }

    fn spawn_for_mode(remember: bool, test_mode: bool) -> std::io::Result<Self> {
        if test_mode {
            eprintln!("magic-paper: oracle = deterministic offline test backend");
            return Ok(Oracle::Deterministic(DeterministicOracle));
        }
        if nonempty_env("RIDDLE_OPENAI_KEY").is_some() {
            eprintln!("riddle: oracle = OpenAI-compatible HTTP");
            Ok(Oracle::Http(Box::new(HttpOracle::new(remember)?)))
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
        let request_id = next_request_id();
        match self {
            Oracle::Http(o) => o.ask(request_id, "handwriting", png_path, ctx, tx),
            Oracle::Pi(o) => {
                eprintln!("magic-paper: event=llm-start request={request_id} backend=pi");
                match o.ask(png_path, ctx, tx) {
                    Some(cancelled) => RequestCancel::pi(request_id, "handwriting", cancelled),
                    None => RequestCancel::inactive(request_id, "handwriting"),
                }
            }
            Oracle::Deterministic(_) => {
                DeterministicOracle::handwriting(tx);
                RequestCancel::inactive(request_id, "handwriting-test")
            }
        }
    }

    /// Interactive path: the UI copies only the bounded crop, then each
    /// backend performs luma conversion and PNG compression on its worker.
    pub fn ask_capture(
        &self,
        capture: crate::ink::PageCapture,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        let request_id = next_request_id();
        match self {
            Oracle::Http(oracle) => oracle.ask_capture(request_id, "handwriting", capture, ctx, tx),
            Oracle::Pi(oracle) => {
                eprintln!("magic-paper: event=llm-start request={request_id} backend=pi");
                match oracle.ask_capture(capture, ctx, tx) {
                    Some(cancelled) => RequestCancel::pi(request_id, "handwriting", cancelled),
                    None => RequestCancel::inactive(request_id, "handwriting"),
                }
            }
            Oracle::Deterministic(_) => {
                DeterministicOracle::handwriting(tx);
                RequestCancel::inactive(request_id, "handwriting-test")
            }
        }
    }

    /// Start the one-second pre-request in a lane isolated from committed
    /// handwriting. If an older cancelled pre-request is still blocked inside
    /// the HTTP transport, skip this optimization and let commit use its own
    /// foreground lane instead of manufacturing a `worker-busy` turn.
    pub fn ask_speculative_capture(
        &self,
        capture: crate::ink::PageCapture,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> Option<RequestCancel> {
        let request_id = next_request_id();
        match self {
            Oracle::Http(oracle) => {
                oracle.ask_speculative_capture(request_id, "handwriting", capture, ctx, tx)
            }
            Oracle::Pi(_) | Oracle::Deterministic(_) => None,
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
    pub fn ask_text(
        &self,
        prompt: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        let request_id = next_request_id();
        match self {
            Oracle::Http(o) => o.ask_text(request_id, "scheduled", prompt, ctx, tx),
            Oracle::Pi(o) => {
                eprintln!("magic-paper: event=llm-start request={request_id} backend=pi");
                match o.ask_text(prompt, ctx, tx) {
                    Some(cancelled) => RequestCancel::pi(request_id, "scheduled", cancelled),
                    None => RequestCancel::inactive(request_id, "scheduled"),
                }
            }
            Oracle::Deterministic(_) => {
                DeterministicOracle::scheduled(tx);
                RequestCancel::inactive(request_id, "scheduled-test")
            }
        }
    }

    #[cfg(test)]
    fn is_deterministic(&self) -> bool {
        matches!(self, Oracle::Deterministic(_))
    }
}

/// The per-turn user text: memory catalog (when remembering) + instruction.
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
mod paper;

use paper::{paper_answer_needs_rewrite, paper_safe_fallback, rewrite_paper_tail};

#[cfg(test)]
mod tests;
