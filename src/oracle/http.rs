//! OpenAI-compatible chat-completions and Responses backend.

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use super::{
    base64, emit_local_route, external_ocr_turn_text, json_quote, json_str_field, local_route,
    paper_answer_needs_rewrite, responses_delta_content, rewrite_paper_tail, sse_delta_content,
    system_prompt, turn_text, Event, HttpApi, OcrResult, PaddleOcr, RequestCancel, StreamParser,
    TurnContext, EXTERNAL_OCR_PROTOCOL,
};

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
}

impl HttpOracle {
    pub fn new(remember: bool) -> std::io::Result<Self> {
        let key = std::env::var("RIDDLE_OPENAI_KEY")
            .map_err(|_| std::io::Error::other("RIDDLE_OPENAI_KEY not set"))?;
        let base = std::env::var("RIDDLE_OPENAI_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let base = base.trim_end_matches('/').to_string();
        // A vision-capable default; override with RIDDLE_OPENAI_MODEL.
        let model =
            std::env::var("RIDDLE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        // Thinking models (Gemini 3.x, o-series…) count hidden reasoning
        // tokens against max_tokens: a tight cap starves the visible reply to
        // one sentence (finish_reason=length). The persona already keeps
        // replies short, so the cap is only a runaway guard — leave headroom.
        let max_tokens = std::env::var("RIDDLE_OPENAI_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000);
        // Sent as "reasoning_effort" only when set: reasoning models accept it
        // ("low" ≈ faster first ink), but some providers reject the field on
        // non-reasoning models, so it must stay out of the default request.
        let reasoning = std::env::var("RIDDLE_OPENAI_REASONING").ok();
        let api = match std::env::var("RIDDLE_OPENAI_API")
            .unwrap_or_else(|_| "chat_completions".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "responses" | "response" => HttpApi::Responses,
            _ => HttpApi::ChatCompletions,
        };
        let web_search = matches!(
            std::env::var("RIDDLE_WEB_SEARCH")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "auto" | "on" | "true" | "1"
        );
        let rewrite_model = std::env::var("RIDDLE_PAPER_REWRITE_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let ocr = PaddleOcr::from_env()?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(90))
            .build();
        eprintln!(
            "riddle: http oracle base={base} model={model} api={api:?} max_tokens={max_tokens} reasoning={} web_search={} rewrite={} input={}",
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
        })
    }

    pub fn ask(
        &self,
        png_path: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> RequestCancel {
        let cancelled = Arc::new(AtomicBool::new(false));
        let ocr_result: Arc<Mutex<Option<OcrResult>>> = Arc::new(Mutex::new(None));
        let png = match std::fs::read(png_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = tx.send(Err(format!("read image: {e}")));
                return RequestCancel::http(cancelled, None);
            }
        };
        if let Some(ocr) = self.ocr.clone() {
            let oracle = self.clone();
            let ctx = ctx.clone();
            let cancel = Arc::clone(&cancelled);
            let shared_result = Arc::clone(&ocr_result);
            thread::spawn(move || {
                let recognized = match ocr.recognize(&png, &cancel) {
                    Ok(result) => result,
                    Err(error) => {
                        if !cancel.load(Ordering::Acquire) {
                            let _ = tx.send(Err(error));
                        }
                        return;
                    }
                };
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                if let Ok(mut slot) = shared_result.lock() {
                    *slot = Some(recognized.clone());
                }
                if recognized.high_confidence() {
                    if let Some(route) = local_route(&recognized.text) {
                        emit_local_route(route, &recognized.text, &tx);
                        return;
                    }
                }
                let user_text = external_ocr_turn_text(&ctx, &recognized.text);
                let catalog_ids = ctx.catalog_ids.clone();
                oracle.send(user_text, None, &ctx, catalog_ids, tx, cancel, true);
            });
            return RequestCancel::http(cancelled, Some(ocr_result));
        }
        let img = base64(&png);
        self.send(
            turn_text(ctx),
            Some(img),
            ctx,
            ctx.catalog_ids.clone(),
            tx,
            Arc::clone(&cancelled),
            false,
        );
        RequestCancel::http(cancelled, None)
    }

    pub fn ask_text(&self, prompt: &str, ctx: &TurnContext, tx: Sender<Result<Event, String>>) {
        self.send(
            prompt.to_string(),
            None,
            ctx,
            Vec::new(),
            tx,
            Arc::new(AtomicBool::new(false)),
            false,
        );
    }

    fn send(
        &self,
        user_text: String,
        image: Option<String>,
        ctx: &TurnContext,
        catalog_ids: Vec<u64>,
        tx: Sender<Result<Event, String>>,
        cancelled: Arc<AtomicBool>,
        external_ocr: bool,
    ) {
        if self.api == HttpApi::Responses {
            self.send_responses(
                user_text,
                image,
                ctx,
                catalog_ids,
                tx,
                cancelled,
                external_ocr,
            );
            return;
        }

        let (base, key, model) = (self.base.clone(), self.key.clone(), self.model.clone());
        let max_tokens = self.max_tokens;
        let reasoning_field = self
            .reasoning
            .as_deref()
            .map(|r| format!("\"reasoning_effort\":{},", json_quote(r)))
            .unwrap_or_default();

        let mut system = system_prompt(self.remember);
        if external_ocr {
            system.push_str(EXTERNAL_OCR_PROTOCOL);
        }
        // MagicPaper's conversational memory: recent pages as prior turns.
        let mut history_msgs = String::new();
        for (t, r) in &ctx.history {
            history_msgs.push_str(&format!(
                "{{\"role\":\"user\",\"content\":{}}},{{\"role\":\"assistant\",\"content\":{}}},",
                json_quote(&format!("(an earlier page) {t}")),
                json_quote(r),
            ));
        }
        let user_content = match image {
            Some(img) => format!(
                concat!(
                    "[{{\"type\":\"text\",\"text\":{}}},",
                    "{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/png;base64,{}\"}}}}]"
                ),
                json_quote(&user_text),
                img,
            ),
            None => json_quote(&user_text),
        };

        let agent = self.agent.clone();
        thread::spawn(move || {
            // Guard rails on the socket: without them a dropped connection or
            // a stalled SSE stream leaves the diary "thinking" forever. The
            // read timeout is per-read, so a healthy stream can run long —
            // only silence trips it (thinking models can lead with ~a minute).
            // OpenAI chat-completions, optionally with a data-URI image part.
            // The token-cap field is provider-dependent: OpenAI's newest
            // models reject "max_tokens" and demand "max_completion_tokens",
            // while many OpenAI-compatible servers only know "max_tokens".
            // Send the widely-supported name first; retry once if corrected.
            let request = |cap_field: &str| {
                let body = format!(
                    concat!(
                        "{{\"model\":{},\"stream\":true,\"{}\":{},{}",
                        "\"messages\":[",
                        "{{\"role\":\"system\",\"content\":{}}},",
                        "{}",
                        "{{\"role\":\"user\",\"content\":{}}}]}}"
                    ),
                    json_quote(&model),
                    cap_field,
                    max_tokens,
                    reasoning_field,
                    json_quote(&system),
                    history_msgs,
                    user_content,
                );
                agent
                    .post(&format!("{base}/chat/completions"))
                    .set("Authorization", &format!("Bearer {key}"))
                    .set("Content-Type", "application/json")
                    .send_string(&body)
            };

            let asked = std::time::Instant::now();
            let resp = match request("max_tokens") {
                Err(ureq::Error::Status(400, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    if detail.contains("max_completion_tokens") {
                        eprintln!("riddle: endpoint wants max_completion_tokens; retrying");
                        request("max_completion_tokens")
                    } else {
                        let _ = tx.send(Err(format!("http 400: {}", detail.trim())));
                        return;
                    }
                }
                other => other,
            };

            let reader = match resp {
                Ok(r) => r.into_reader(),
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    let _ = tx.send(Err(format!("http {code}: {}", detail.trim())));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("request failed: {e}")));
                    return;
                }
            };
            if cancelled.load(Ordering::Acquire) {
                return;
            }

            // Parse the SSE stream: lines of `data: {json}` whose delta.content
            // fragments accumulate; the parser turns the running text into
            // events (route directive, sentences, transcription postscript).
            let mut parser = StreamParser::new(catalog_ids);
            let mut acc = String::new();
            let mut first = true;
            let mut emit = |events: Vec<Result<Event, String>>| {
                for ev in events {
                    if first {
                        eprintln!(
                            "riddle: oracle first chunk +{}ms",
                            asked.elapsed().as_millis()
                        );
                        first = false;
                    }
                    let _ = tx.send(ev);
                }
            };
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if cancelled.load(Ordering::Acquire) {
                    eprintln!("riddle: speculative chat request cancelled");
                    return;
                }
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                if let Some(frag) = sse_delta_content(data) {
                    if frag.is_empty() {
                        continue;
                    }
                    acc.push_str(&frag);
                    emit(parser.advance(&acc, false));
                }
            }
            emit(parser.advance(&acc, true));
            // tx drops here → the diary's receiver disconnects = reply complete.
        });
    }

    fn send_responses(
        &self,
        user_text: String,
        image: Option<String>,
        ctx: &TurnContext,
        catalog_ids: Vec<u64>,
        tx: Sender<Result<Event, String>>,
        cancelled: Arc<AtomicBool>,
        external_ocr: bool,
    ) {
        let (base, key, model) = (self.base.clone(), self.key.clone(), self.model.clone());
        let max_tokens = self.max_tokens;
        let reasoning = self.reasoning.clone();
        let web_search = self.web_search;
        let rewrite_model = self.rewrite_model.clone();
        let mut system = system_prompt(self.remember);
        if external_ocr {
            system.push_str(EXTERNAL_OCR_PROTOCOL);
        }
        let agent = self.agent.clone();

        // Responses accepts prior messages, but a compact labeled transcript is
        // more widely compatible with third-party Responses gateways and keeps
        // the current image as the only multimodal input item.
        let mut page_text = String::new();
        if !ctx.history.is_empty() {
            page_text.push_str("Recent earlier pages, oldest first:\n");
            for (transcript, reply) in &ctx.history {
                page_text.push_str("Master wrote: ");
                page_text.push_str(transcript);
                page_text.push_str("\nMagicPaper replied: ");
                page_text.push_str(reply);
                page_text.push('\n');
            }
            page_text.push('\n');
        }
        page_text.push_str(&user_text);

        let content = match image {
            Some(img) => format!(
                concat!(
                    "[{{\"type\":\"input_text\",\"text\":{}}},",
                    "{{\"type\":\"input_image\",\"image_url\":\"data:image/png;base64,{}\",\"detail\":\"high\"}}]"
                ),
                json_quote(&page_text),
                img,
            ),
            None => format!(
                "[{{\"type\":\"input_text\",\"text\":{}}}]",
                json_quote(&page_text)
            ),
        };
        let reasoning_field = reasoning
            .as_deref()
            .map(|effort| format!("\"reasoning\":{{\"effort\":{}}},", json_quote(effort)))
            .unwrap_or_default();
        let tools_field = if web_search {
            "\"tools\":[{\"type\":\"web_search\",\"search_context_size\":\"low\"}],\"tool_choice\":\"auto\",".to_string()
        } else {
            String::new()
        };
        let body = format!(
            concat!(
                "{{\"model\":{},\"stream\":true,\"store\":false,",
                "\"max_output_tokens\":{},{}{}\"instructions\":{},",
                "\"input\":[{{\"role\":\"user\",\"content\":{}}}]}}"
            ),
            json_quote(&model),
            max_tokens,
            reasoning_field,
            tools_field,
            json_quote(&system),
            content,
        );

        thread::spawn(move || {
            let asked = std::time::Instant::now();
            let resp = agent
                .post(&format!("{base}/responses"))
                .set("Authorization", &format!("Bearer {key}"))
                .set("Content-Type", "application/json")
                .send_string(&body);
            let reader = match resp {
                Ok(r) => r.into_reader(),
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    let _ = tx.send(Err(format!("responses http {code}: {}", detail.trim())));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("responses request failed: {e}")));
                    return;
                }
            };
            if cancelled.load(Ordering::Acquire) {
                return;
            }

            // Feed completed sentences to the paper as the model writes them.
            // A malformed chunk and everything after it are held back for the
            // paper editor, so URLs/Markdown never reach physical ink.
            let mut acc = String::new();
            let mut parser = StreamParser::new(catalog_ids);
            let mut first_model_text = true;
            let mut searched = false;
            let mut failed: Option<String> = None;
            let mut holding_tail = false;
            let mut held_tail = String::new();
            let mut sent_prefix = String::new();
            let mut held_transcript: Option<String> = None;
            let mut first_paper_event = true;

            let mut deliver = |events: Vec<Result<Event, String>>| {
                for event in events {
                    match event {
                        Ok(Event::Ink(chunk)) => {
                            if holding_tail || paper_answer_needs_rewrite(&chunk) {
                                holding_tail = true;
                                if !held_tail.is_empty() {
                                    held_tail.push(' ');
                                }
                                held_tail.push_str(&chunk);
                                continue;
                            }
                            if first_paper_event {
                                eprintln!(
                                    "riddle: oracle first paper text +{}ms",
                                    asked.elapsed().as_millis()
                                );
                                first_paper_event = false;
                            }
                            if !sent_prefix.is_empty() {
                                sent_prefix.push(' ');
                            }
                            sent_prefix.push_str(&chunk);
                            let _ = tx.send(Ok(Event::Ink(chunk)));
                        }
                        Ok(Event::Transcript(t)) if holding_tail => {
                            held_transcript = Some(t);
                        }
                        Ok(other) => {
                            if first_paper_event && matches!(other, Event::Show(_)) {
                                eprintln!(
                                    "riddle: oracle first paper event +{}ms",
                                    asked.elapsed().as_millis()
                                );
                                first_paper_event = false;
                            }
                            let _ = tx.send(Ok(other));
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                        }
                    }
                }
            };

            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if cancelled.load(Ordering::Acquire) {
                    eprintln!("riddle: speculative Responses request cancelled");
                    return;
                }
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                if data.contains("web_search_call") {
                    searched = true;
                }
                if data.contains("\"type\":\"response.failed\"") {
                    failed = json_str_field(data, "message").or_else(|| Some(data.to_string()));
                }
                if let Some(frag) = responses_delta_content(data) {
                    if first_model_text {
                        eprintln!(
                            "riddle: oracle first model text +{}ms",
                            asked.elapsed().as_millis()
                        );
                        first_model_text = false;
                    }
                    acc.push_str(&frag);
                    deliver(parser.advance(&acc, false));
                }
            }
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            deliver(parser.advance(&acc, true));
            drop(deliver);

            if let Some(detail) = failed {
                let _ = tx.send(Err(format!("responses failed: {detail}")));
                return;
            }
            if acc.trim().is_empty() {
                let _ = tx.send(Err("responses returned no paper answer".into()));
                return;
            }

            if holding_tail {
                let rewritten = match rewrite_model {
                    Some(ref model) => {
                        rewrite_paper_tail(&agent, &base, &key, model, &sent_prefix, &held_tail)
                    }
                    None => Err("no paper editor model is configured".into()),
                };
                match rewritten {
                    Ok(text) if !paper_answer_needs_rewrite(&text) => {
                        if first_paper_event {
                            eprintln!(
                                "riddle: oracle first paper text +{}ms",
                                asked.elapsed().as_millis()
                            );
                        }
                        eprintln!("riddle: paper editor rewrote held response tail");
                        let _ = tx.send(Ok(Event::Ink(text)));
                    }
                    Ok(_) => {
                        let _ = tx.send(Err("paper editor kept non-paper formatting".into()));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("paper editor failed: {e}")));
                    }
                }
                if let Some(transcript) = held_transcript {
                    let _ = tx.send(Ok(Event::Transcript(transcript)));
                }
            }

            eprintln!(
                "riddle: oracle complete +{}ms search={}",
                asked.elapsed().as_millis(),
                if searched { "yes" } else { "no" }
            );
        });
    }
}
