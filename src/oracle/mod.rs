//! MagicPaper's model boundary.
//!
//! Production model turns always cross the private ReMagic Pi Agent socket.
//! MagicPaper keeps handwriting capture, PaddleOCR, local commands, memory and
//! paper rendering; ReMagic owns provider credentials and the resident Pi RPC
//! process. Deterministic mode is available only to isolated acceptance tests.

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

#[allow(dead_code)] // Legacy direct-Pi migration backend only.
const DATA_DIR: &str = "/home/root/.local/share/magicpaper";
#[allow(dead_code)] // Legacy direct-Pi migration backend only.
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
    /// Open device-local experience settings.
    Settings,
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
    fn agent(
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

    #[cfg(test)]
    pub(crate) fn testing(request_id: u64, cancelled: Arc<AtomicBool>) -> Self {
        Self::agent(
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

mod deterministic;
mod prompts;
mod stream;

use prompts::{external_ocr_turn_text, system_prompt, turn_text};

use deterministic::DeterministicOracle;
use stream::StreamParser;
#[cfg(test)]
use stream::{clean, strip_directives};

/// The diary's spirit: hosted Agent in production, deterministic in tests.
pub(crate) enum Oracle {
    Agent(Box<AgentOracle>),
    Deterministic(DeterministicOracle),
}

pub fn paddle_ocr_test(png_path: &str) -> Result<String, String> {
    let ocr = PaddleOcr::from_env()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "MAGICPAPER_OCR_TOKEN is not set".to_string())?;
    let png = std::fs::read(png_path).map_err(|error| format!("read image: {error}"))?;
    ocr.recognize(0, "cli", &png, &AtomicBool::new(false))
        .map(|result| result.text)
}

impl Oracle {
    /// Start the ReMagic-hosted Pi Agent. HTTP credentials no longer alter
    /// backend selection; they are migrated into the Agent provider store.
    pub fn spawn(remember: bool) -> std::io::Result<Self> {
        Self::spawn_for_mode(remember, crate::runtime_env::test_mode())
    }

    pub(crate) fn apply_pi_preferences(
        &mut self,
        values: crate::pi_preferences::PiPreferenceValues,
    ) {
        if let Oracle::Agent(oracle) = self {
            oracle.apply_preferences(values);
        }
    }

    pub(crate) fn start_agent_control(
        &self,
        command: AgentControlCommand,
        tx: Sender<AgentControlStatus>,
    ) -> bool {
        if let Oracle::Agent(oracle) = self {
            oracle.start_control(command, tx);
            true
        } else {
            false
        }
    }

    fn spawn_for_mode(remember: bool, test_mode: bool) -> std::io::Result<Self> {
        if test_mode {
            eprintln!("magic-paper: oracle = deterministic offline test backend");
            return Ok(Oracle::Deterministic(DeterministicOracle));
        }
        Ok(Oracle::Agent(Box::new(AgentOracle::new(remember)?)))
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
            Oracle::Agent(oracle) => oracle.ask(request_id, "handwriting", png_path, ctx, tx),
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
            Oracle::Agent(oracle) => {
                oracle.ask_capture(request_id, "handwriting", capture, ctx, tx)
            }
            Oracle::Deterministic(_) => {
                DeterministicOracle::handwriting(tx);
                RequestCancel::inactive(request_id, "handwriting-test")
            }
        }
    }

    /// Start the one-second pre-request in a lane isolated from committed
    /// handwriting. ReMagic may cancel this speculative lane immediately when
    /// a real pen interaction arrives.
    pub fn ask_speculative_capture(
        &self,
        capture: crate::ink::PageCapture,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> Option<RequestCancel> {
        let request_id = next_request_id();
        match self {
            Oracle::Agent(oracle) => {
                oracle.ask_speculative_capture(request_id, "handwriting", capture, ctx, tx)
            }
            Oracle::Deterministic(_) => None,
        }
    }

    /// Speculative turns are supported only when external OCR is enabled.
    pub fn supports_speculative(&self) -> bool {
        matches!(self, Oracle::Agent(oracle) if oracle.supports_speculative())
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
            Oracle::Agent(oracle) => oracle.ask_text(request_id, "scheduled", prompt, ctx, tx),
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

mod agent;
use agent::AgentOracle;
pub(crate) use agent::{AgentControlCommand, AgentControlStatus};

mod paddle;

use paddle::{OcrResult, PaddleOcr};

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
#[cfg(test)]
mod tests;
