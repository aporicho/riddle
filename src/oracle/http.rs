//! OpenAI-compatible chat-completions and Responses backend.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::ink::PageCapture;

use super::{
    base64, emit_local_route, external_ocr_turn_text, local_route, log_llm_terminal, nonempty_env,
    turn_text, Event, HttpApi, OcrResult, PaddleOcr, RequestCancel, TurnContext,
};

mod chat;
mod responses;
mod worker_pool;

use worker_pool::{WorkerLane, WorkerPermit, WorkerPools};

struct HandwritingRequest {
    request_id: u64,
    domain: &'static str,
    source: PageSource,
    ctx: TurnContext,
    tx: Sender<Result<Event, String>>,
    cancelled: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    ocr_result: Arc<Mutex<Option<OcrResult>>>,
    _permit: WorkerPermit,
}

enum PageSource {
    File(String),
    Capture(PageCapture),
}

struct SendRequest {
    request_id: u64,
    domain: &'static str,
    user_text: String,
    image: Option<String>,
    ctx: TurnContext,
    catalog_ids: Vec<u64>,
    tx: Sender<Result<Event, String>>,
    cancelled: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    _permit: WorkerPermit,
    external_ocr: bool,
}

fn concise_error(error: &str) -> String {
    error
        .lines()
        .next()
        .unwrap_or("unknown error")
        .chars()
        .take(240)
        .collect()
}

fn send_failure(
    tx: &Sender<Result<Event, String>>,
    kind: &str,
    request_id: u64,
    domain: &str,
    terminal: &AtomicBool,
    stage: &str,
    error: String,
) {
    if !log_llm_terminal(terminal, request_id, domain, "error", stage, None) {
        return;
    }
    if kind != "llm-error" {
        eprintln!(
            "magic-paper: event={kind} request={}:{} domain={domain} stage={stage} error={:?}",
            std::process::id(),
            request_id,
            concise_error(&error)
        );
    }
    let _ = tx.send(Err(error));
}

/// OpenAI-compatible HTTP backend. Responses mode adds hosted web search and
/// a final paper-editing pass; chat-completions remains available for older
/// providers.
#[derive(Clone)]
pub struct HttpOracle {
    base: String, // e.g. https://api.openai.com/v1  (no trailing slash)
    key: String,
    model: String,
    max_tokens: u32,
    reasoning: Option<String>, // "reasoning_effort" value, e.g. "low"
    pub(super) api: HttpApi,
    web_search: bool,
    rewrite_model: Option<String>,
    remember: bool,
    pub(super) ocr: Option<PaddleOcr>,
    /// Reused between turns so rapid follow-ups can reuse pooled TLS sockets.
    agent: ureq::Agent,
    workers: WorkerPools,
}

impl HttpOracle {
    pub fn new(remember: bool) -> std::io::Result<Self> {
        crate::runtime_env::require_external_integrations("HTTP oracle")?;
        let key = nonempty_env("MAGICPAPER_OPENAI_KEY")
            .ok_or_else(|| std::io::Error::other("MAGICPAPER_OPENAI_KEY is missing or blank"))?;
        let base = nonempty_env("MAGICPAPER_OPENAI_BASE")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let base = base.trim_end_matches('/').to_string();
        // A vision-capable default; override with MAGICPAPER_OPENAI_MODEL.
        let model =
            std::env::var("MAGICPAPER_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        // Thinking models (Gemini 3.x, o-series…) count hidden reasoning
        // tokens against max_tokens: a tight cap starves the visible reply to
        // one sentence (finish_reason=length). The persona already keeps
        // replies short, so the cap is only a runaway guard — leave headroom.
        let max_tokens = std::env::var("MAGICPAPER_OPENAI_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000);
        // Sent as "reasoning_effort" only when set: reasoning models accept it
        // ("low" ≈ faster first ink), but some providers reject the field on
        // non-reasoning models, so it must stay out of the default request.
        let reasoning = std::env::var("MAGICPAPER_OPENAI_REASONING").ok();
        let api = match std::env::var("MAGICPAPER_OPENAI_API")
            .unwrap_or_else(|_| "chat_completions".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "responses" | "response" => HttpApi::Responses,
            _ => HttpApi::ChatCompletions,
        };
        let web_search = matches!(
            std::env::var("MAGICPAPER_WEB_SEARCH")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "auto" | "on" | "true" | "1"
        );
        let rewrite_model = std::env::var("MAGICPAPER_PAPER_REWRITE_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let ocr = PaddleOcr::from_env()?;
        let agent = ureq::AgentBuilder::new()
            // Optional standard proxy support makes the HTTP backend usable
            // on managed/captive networks without changing its API contract.
            .try_proxy_from_env(true)
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(90))
            .timeout_write(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(115))
            .build();
        eprintln!(
            "magicpaper: http oracle base={base} model={model} api={api:?} max_tokens={max_tokens} reasoning={} web_search={} rewrite={} input={}",
            reasoning.as_deref().unwrap_or("-"),
            if web_search { "auto" } else { "off" },
            rewrite_model.as_deref().unwrap_or("-"),
            ocr.as_ref()
                .map(|ocr| ocr.model.as_str())
                .unwrap_or("OpenAI vision"),
        );
        Ok(Self {
            base,
            key,
            model,
            max_tokens,
            reasoning,
            api,
            web_search,
            rewrite_model,
            remember,
            ocr,
            agent,
            workers: WorkerPools::default(),
        })
    }

    fn try_worker_permit(&self, lane: WorkerLane) -> Option<WorkerPermit> {
        self.workers.acquire(lane)
    }

    pub fn ask(
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
            WorkerLane::Interactive,
        )
        .expect("interactive lane always returns a terminal request handle")
    }

    pub fn ask_capture(
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
            WorkerLane::Interactive,
        )
        .expect("interactive lane always returns a terminal request handle")
    }

    pub fn ask_speculative_capture(
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
            WorkerLane::Speculative,
        )
    }

    fn start_handwriting(
        &self,
        request_id: u64,
        domain: &'static str,
        source: PageSource,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
        lane: WorkerLane,
    ) -> Option<RequestCancel> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let terminal = Arc::new(AtomicBool::new(false));
        let has_external_ocr = self.ocr.is_some();
        let ocr_result = Arc::new(Mutex::new(None));
        let Some(permit) = self.try_worker_permit(lane) else {
            if lane == WorkerLane::Speculative {
                eprintln!(
                    "magic-paper: event=ocr-preask-skipped request={}:{} reason=speculative-worker-busy",
                    std::process::id(),
                    request_id
                );
                return None;
            }
            send_failure(
                &tx,
                "llm-error",
                request_id,
                domain,
                &terminal,
                "worker-busy",
                "previous foreground request is still shutting down; retry shortly".into(),
            );
            return Some(RequestCancel::http(
                request_id,
                domain,
                cancelled,
                terminal,
                has_external_ocr.then_some(ocr_result),
            ));
        };
        let worker = HandwritingRequest {
            request_id,
            domain,
            source,
            ctx: ctx.clone(),
            tx,
            cancelled: Arc::clone(&cancelled),
            terminal: Arc::clone(&terminal),
            ocr_result: Arc::clone(&ocr_result),
            _permit: permit,
        };
        let oracle = self.clone();
        thread::spawn(move || oracle.prepare_handwriting(worker));
        Some(RequestCancel::http(
            request_id,
            domain,
            cancelled,
            terminal,
            has_external_ocr.then_some(ocr_result),
        ))
    }

    pub fn ask_text(
        &self,
        request_id: u64,
        domain: &'static str,
        prompt: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        let cancelled = Arc::new(AtomicBool::new(false));
        let terminal = Arc::new(AtomicBool::new(false));
        let Some(permit) = self.try_worker_permit(WorkerLane::Scheduled) else {
            send_failure(
                &tx,
                "llm-error",
                request_id,
                domain,
                &terminal,
                "worker-busy",
                "previous request is still shutting down; retry shortly".into(),
            );
            return RequestCancel::http(request_id, domain, cancelled, terminal, None);
        };
        let request = SendRequest {
            request_id,
            domain,
            user_text: prompt.to_owned(),
            image: None,
            ctx: ctx.clone(),
            catalog_ids: Vec::new(),
            tx,
            cancelled: Arc::clone(&cancelled),
            terminal: Arc::clone(&terminal),
            _permit: permit,
            external_ocr: false,
        };
        let oracle = self.clone();
        thread::spawn(move || oracle.send(request));
        RequestCancel::http(request_id, domain, cancelled, terminal, None)
    }

    fn prepare_handwriting(&self, request: HandwritingRequest) {
        if request.cancelled.load(Ordering::Acquire) {
            return;
        }
        let page_started = std::time::Instant::now();
        let page_source = match &request.source {
            PageSource::File(_) => "file",
            PageSource::Capture(_) => "capture-png",
        };
        let png = match load_page(&request.source, &request.cancelled) {
            Ok(bytes) => bytes,
            Err(error) if !request.cancelled.load(Ordering::Acquire) => {
                send_failure(
                    &request.tx,
                    "ocr-error",
                    request.request_id,
                    request.domain,
                    &request.terminal,
                    "prepare-page",
                    format!("prepare image: {error}"),
                );
                return;
            }
            Err(_) => return,
        };
        eprintln!(
            "magic-paper: event=page-prepared request={}:{} domain={} source={} latency_ms={} bytes={}",
            std::process::id(),
            request.request_id,
            request.domain,
            page_source,
            page_started.elapsed().as_millis(),
            png.len(),
        );
        if request.cancelled.load(Ordering::Acquire) {
            return;
        }
        let (user_text, image, external_ocr) = match self.recognize_or_encode(&request, &png) {
            Some(prepared) => prepared,
            None => return,
        };
        let catalog_ids = request.ctx.catalog_ids.clone();
        self.send(SendRequest {
            request_id: request.request_id,
            domain: request.domain,
            user_text,
            image,
            ctx: request.ctx,
            catalog_ids,
            tx: request.tx,
            cancelled: request.cancelled,
            terminal: request.terminal,
            _permit: request._permit,
            external_ocr,
        });
    }

    fn recognize_or_encode(
        &self,
        request: &HandwritingRequest,
        png: &[u8],
    ) -> Option<(String, Option<String>, bool)> {
        let Some(ocr) = &self.ocr else {
            return Some((turn_text(&request.ctx), Some(base64(png)), false));
        };
        let recognized =
            match ocr.recognize(request.request_id, request.domain, png, &request.cancelled) {
                Ok(result) => result,
                Err(error) => {
                    if !request.cancelled.load(Ordering::Acquire) {
                        send_failure(
                            &request.tx,
                            "ocr-error",
                            request.request_id,
                            request.domain,
                            &request.terminal,
                            "recognize",
                            error,
                        );
                    }
                    return None;
                }
            };
        if request.cancelled.load(Ordering::Acquire) {
            return None;
        }
        if let Ok(mut slot) = request.ocr_result.lock() {
            *slot = Some(recognized.clone());
        }
        if recognized.high_confidence() {
            if let Some(route) = local_route(&recognized.text) {
                log_llm_terminal(
                    &request.terminal,
                    request.request_id,
                    request.domain,
                    "done",
                    "local-route",
                    None,
                );
                emit_local_route(route, &recognized.text, &request.tx);
                return None;
            }
        }
        Some((
            external_ocr_turn_text(&request.ctx, &recognized.text),
            None,
            true,
        ))
    }

    fn send(&self, request: SendRequest) {
        eprintln!(
            "magic-paper: event=llm-start request={}:{} domain={} backend=http api={:?} input={}",
            std::process::id(),
            request.request_id,
            request.domain,
            self.api,
            if request.external_ocr {
                "ocr-text"
            } else if request.image.is_some() {
                "page-image"
            } else {
                "text"
            }
        );
        match self.api {
            HttpApi::Responses => responses::send(self, request),
            HttpApi::ChatCompletions => chat::send(self, request),
        }
    }
}

fn load_page(source: &PageSource, cancelled: &AtomicBool) -> std::io::Result<Vec<u8>> {
    match source {
        PageSource::File(path) => std::fs::read(path),
        PageSource::Capture(capture) => capture.encode_png(cancelled),
    }
}

#[cfg(test)]
mod tests;
