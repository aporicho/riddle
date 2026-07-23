//! ReMagic-hosted Pi Agent backend.
//!
//! MagicPaper owns handwriting capture, OCR, local command routing and paper
//! rendering. ReMagic owns the Pi/Node process, provider credentials and
//! model/tool loop. The boundary is a private, length-prefixed JSON stream.

use serde_json::Value;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::ink::PageCapture;
use crate::pi_preferences::{PiPreferenceValues, PiPreferences};

use super::{
    emit_local_route, external_ocr_turn_text, local_route, log_llm_terminal, turn_text, Event,
    OcrResult, PaddleOcr, RequestCancel, TurnContext,
};

const DEFAULT_SOCKET: &str = "/run/remagic/agent.sock";
mod control;
mod turn;
mod wire;
pub(crate) use control::{AgentControlCommand, AgentControlStatus};

#[cfg(test)]
#[path = "agent/tests.rs"]
mod tests;

#[derive(Clone)]
pub(crate) struct AgentOracle {
    socket: PathBuf,
    client_token: String,
    app_id: String,
    profile: AgentProfile,
    remember: bool,
    ocr: Option<PaddleOcr>,
    workers: AgentWorkers,
}

#[derive(Clone)]
struct AgentProfile {
    provider: String,
    model: String,
    thinking: String,
    tools: bool,
}

#[derive(Clone, Default)]
struct AgentWorkers {
    interactive: Arc<AtomicBool>,
    speculative: Arc<AtomicBool>,
    scheduled: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Interactive,
    Speculative,
    Scheduled,
}

struct Permit {
    gate: Arc<AtomicBool>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.gate.store(false, Ordering::Release);
    }
}

impl AgentWorkers {
    fn acquire(&self, lane: Lane) -> Option<Permit> {
        let gate = match lane {
            Lane::Interactive => Arc::clone(&self.interactive),
            Lane::Speculative => Arc::clone(&self.speculative),
            Lane::Scheduled => Arc::clone(&self.scheduled),
        };
        gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Permit { gate })
    }
}

enum PageSource {
    File(String),
    Capture(PageCapture),
}

struct TurnRequest {
    request_id: u64,
    domain: &'static str,
    lane: Lane,
    input: String,
    ctx: TurnContext,
    tx: Sender<Result<Event, String>>,
    cancelled: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    _permit: Permit,
}

struct HandwritingRequest {
    request_id: u64,
    domain: &'static str,
    lane: Lane,
    source: PageSource,
    ctx: TurnContext,
    tx: Sender<Result<Event, String>>,
    cancelled: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    ocr_result: Arc<Mutex<Option<OcrResult>>>,
    _permit: Permit,
}

impl AgentOracle {
    pub(super) fn new(remember: bool) -> io::Result<Self> {
        crate::runtime_env::require_external_integrations("ReMagic Pi Agent")?;
        let socket = std::env::var_os("REMAGIC_AGENT_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));
        let client_token = required_env("REMAGIC_AGENT_TOKEN")?;
        let app_id = std::env::var("REMAGIC_APP_ID").unwrap_or_else(|_| "magicpaper".into());
        if app_id != "magicpaper" {
            return Err(io::Error::other(format!(
                "ReMagic Agent identity mismatch: expected magicpaper, got {app_id}"
            )));
        }
        let profile = AgentProfile::from_preferences(PiPreferences::open().values());
        let ocr = PaddleOcr::from_env()?;
        eprintln!(
            "magicpaper: oracle = ReMagic Pi Agent provider={} model={} thinking={} tools={} input={}",
            profile.provider,
            profile.model,
            profile.thinking,
            if profile.tools { "safe" } else { "off" },
            ocr.as_ref()
                .map(|ocr| ocr.model.as_str())
                .unwrap_or("missing PaddleOCR"),
        );
        Ok(Self {
            socket,
            client_token,
            app_id,
            profile,
            remember,
            ocr,
            workers: AgentWorkers::default(),
        })
    }

    pub(super) fn supports_speculative(&self) -> bool {
        self.ocr.as_ref().is_some_and(|ocr| ocr.speculative)
    }

    pub(super) fn apply_preferences(&mut self, values: PiPreferenceValues) {
        self.profile = AgentProfile::from_preferences(values);
    }

    pub(super) fn start_control(
        &self,
        command: AgentControlCommand,
        tx: Sender<AgentControlStatus>,
    ) {
        let oracle = self.clone();
        thread::spawn(move || {
            let _ = tx.send(oracle.run_control(command));
        });
    }

    pub(super) fn ask(
        &self,
        request_id: u64,
        domain: &'static str,
        png_path: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        self.start_handwriting(
            request_id,
            domain,
            PageSource::File(png_path.to_owned()),
            ctx,
            tx,
            Lane::Interactive,
        )
        .expect("interactive Agent lane always returns a request handle")
    }

    pub(super) fn ask_capture(
        &self,
        request_id: u64,
        domain: &'static str,
        capture: PageCapture,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        self.start_handwriting(
            request_id,
            domain,
            PageSource::Capture(capture),
            ctx,
            tx,
            Lane::Interactive,
        )
        .expect("interactive Agent lane always returns a request handle")
    }

    pub(super) fn ask_speculative_capture(
        &self,
        request_id: u64,
        domain: &'static str,
        capture: PageCapture,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> Option<RequestCancel> {
        self.start_handwriting(
            request_id,
            domain,
            PageSource::Capture(capture),
            ctx,
            tx,
            Lane::Speculative,
        )
    }

    pub(super) fn ask_text(
        &self,
        request_id: u64,
        domain: &'static str,
        prompt: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        let cancelled = Arc::new(AtomicBool::new(false));
        let terminal = Arc::new(AtomicBool::new(false));
        let Some(permit) = self.workers.acquire(Lane::Scheduled) else {
            let _ = tx.send(Err(
                "Pi Agent is still finishing the previous scheduled turn".into(),
            ));
            return RequestCancel::agent(request_id, domain, cancelled, terminal, None);
        };
        let input = format!("{}\n\n{prompt}", turn_text(ctx));
        self.spawn_turn(TurnRequest {
            request_id,
            domain,
            lane: Lane::Scheduled,
            input,
            ctx: ctx.clone(),
            tx,
            cancelled: Arc::clone(&cancelled),
            terminal: Arc::clone(&terminal),
            _permit: permit,
        });
        RequestCancel::agent(request_id, domain, cancelled, terminal, None)
    }

    fn start_handwriting(
        &self,
        request_id: u64,
        domain: &'static str,
        source: PageSource,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
        lane: Lane,
    ) -> Option<RequestCancel> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let terminal = Arc::new(AtomicBool::new(false));
        let ocr_result = Arc::new(Mutex::new(None));
        let Some(permit) = self.workers.acquire(lane) else {
            if lane == Lane::Speculative {
                return None;
            }
            let _ = tx.send(Err("Pi Agent is still finishing the previous turn".into()));
            return Some(RequestCancel::agent(
                request_id,
                domain,
                cancelled,
                terminal,
                Some(ocr_result),
            ));
        };
        let request = HandwritingRequest {
            request_id,
            domain,
            lane,
            source,
            ctx: ctx.clone(),
            tx,
            cancelled: Arc::clone(&cancelled),
            terminal: Arc::clone(&terminal),
            ocr_result: Arc::clone(&ocr_result),
            _permit: permit,
        };
        let oracle = self.clone();
        thread::spawn(move || oracle.prepare_handwriting(request));
        Some(RequestCancel::agent(
            request_id,
            domain,
            cancelled,
            terminal,
            Some(ocr_result),
        ))
    }

    fn prepare_handwriting(&self, request: HandwritingRequest) {
        let Some(ocr) = &self.ocr else {
            send_error(
                &request,
                "PaddleOCR is required before Pi Agent can read handwriting",
            );
            return;
        };
        let png = match load_page(&request.source, &request.cancelled) {
            Ok(png) => png,
            Err(error) => {
                send_error(&request, &format!("prepare handwriting image: {error}"));
                return;
            }
        };
        let result =
            match ocr.recognize(request.request_id, request.domain, &png, &request.cancelled) {
                Ok(result) => result,
                Err(_error) if request.cancelled.load(Ordering::Acquire) => return,
                Err(error) => {
                    send_error(&request, &error);
                    return;
                }
            };
        if let Ok(mut slot) = request.ocr_result.lock() {
            *slot = Some(result.clone());
        }
        if result.high_confidence() {
            if let Some(route) = local_route(&result.text) {
                log_llm_terminal(
                    &request.terminal,
                    request.request_id,
                    request.domain,
                    "done",
                    "local-route",
                    None,
                );
                emit_local_route(route, &result.text, &request.tx);
                return;
            }
        }
        self.spawn_turn(TurnRequest {
            request_id: request.request_id,
            domain: request.domain,
            lane: request.lane,
            input: external_ocr_turn_text(&request.ctx, &result.text),
            ctx: request.ctx,
            tx: request.tx,
            cancelled: request.cancelled,
            terminal: request.terminal,
            _permit: request._permit,
        });
    }

    fn spawn_turn(&self, request: TurnRequest) {
        let oracle = self.clone();
        thread::spawn(move || oracle.run_turn(request));
    }
}

impl AgentProfile {
    fn from_preferences(values: PiPreferenceValues) -> Self {
        Self {
            provider: values.provider.provider_id().into(),
            model: values.model.model_id(values.provider).into(),
            thinking: values
                .thinking
                .normalized(values.provider)
                .thinking_id(values.provider)
                .into(),
            tools: values.tools_enabled,
        }
    }
}

fn required_env(name: &str) -> io::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| io::Error::other(format!("{name} is required by agent:pi-v1")))
}

fn safe_tool_specs(enabled: bool) -> Value {
    if !enabled {
        return Value::Array(Vec::new());
    }
    // The schemas are deliberately empty until the application-side
    // acknowledgement path is available. ReMagic must never substitute Pi's
    // default shell/write tools for this empty allow-list.
    Value::Array(Vec::new())
}

fn load_page(source: &PageSource, cancelled: &AtomicBool) -> io::Result<Vec<u8>> {
    match source {
        PageSource::File(path) => std::fs::read(path),
        PageSource::Capture(capture) => capture.encode_png(cancelled),
    }
}

fn send_error(request: &HandwritingRequest, error: &str) {
    log_llm_terminal(
        &request.terminal,
        request.request_id,
        request.domain,
        "error",
        "ocr",
        None,
    );
    let _ = request.tx.send(Err(error.into()));
}
