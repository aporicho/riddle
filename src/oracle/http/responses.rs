use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::Ordering;

use super::super::{
    json_quote, json_str_field, log_llm_terminal, paper_answer_needs_rewrite, paper_safe_fallback,
    responses_delta_content, rewrite_paper_tail, system_prompt, Event, StreamParser,
    EXTERNAL_OCR_PROTOCOL,
};
use super::{send_failure, HttpOracle, SendRequest};

struct ResponseConfig {
    base: String,
    key: String,
    body: String,
    rewrite_model: Option<String>,
    agent: ureq::Agent,
}

struct StreamFailure {
    stage: &'static str,
    message: String,
}

pub(super) fn send(oracle: &HttpOracle, request: SendRequest) {
    let config = ResponseConfig::new(oracle, &request);
    let asked = std::time::Instant::now();
    let response = config
        .agent
        .post(&format!("{}/responses", config.base))
        .set("Authorization", &format!("Bearer {}", config.key))
        .set("Content-Type", "application/json")
        .send_string(&config.body);
    if request.cancelled.load(Ordering::Acquire) {
        return;
    }
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            send_failure(
                &request.tx,
                "llm-error",
                request.request_id,
                request.domain,
                &request.terminal,
                "request",
                response_error(error),
            );
            return;
        }
    };
    let mut paper = match consume_response(response.into_reader(), &request, asked) {
        Ok(paper) => paper,
        Err(error) => {
            send_failure(
                &request.tx,
                "llm-error",
                request.request_id,
                request.domain,
                &request.terminal,
                error.stage,
                error.message,
            );
            return;
        }
    };
    if request.cancelled.load(Ordering::Acquire) {
        return;
    }
    paper.rewrite_held(&config);
    log_llm_terminal(
        &request.terminal,
        request.request_id,
        request.domain,
        "done",
        if paper.searched {
            "responses-search"
        } else {
            "responses"
        },
        Some(asked.elapsed().as_millis()),
    );
}

impl ResponseConfig {
    fn new(oracle: &HttpOracle, request: &SendRequest) -> Self {
        let mut system = system_prompt(oracle.remember);
        if request.external_ocr {
            system.push_str(EXTERNAL_OCR_PROTOCOL);
        }
        let page_text = page_text(request);
        let content = match &request.image {
            Some(image) => format!(
                concat!(
                    "[{{\"type\":\"input_text\",\"text\":{}}},",
                    "{{\"type\":\"input_image\",\"image_url\":\"data:image/png;base64,{}\",\"detail\":\"high\"}}]"
                ),
                json_quote(&page_text),
                image,
            ),
            None => format!(
                "[{{\"type\":\"input_text\",\"text\":{}}}]",
                json_quote(&page_text)
            ),
        };
        let reasoning = oracle
            .reasoning
            .as_deref()
            .map(|effort| format!("\"reasoning\":{{\"effort\":{}}},", json_quote(effort)))
            .unwrap_or_default();
        let tools = if oracle.web_search {
            "\"tools\":[{\"type\":\"web_search\",\"search_context_size\":\"low\"}],\"tool_choice\":\"auto\",".to_owned()
        } else {
            String::new()
        };
        let body = format!(
            concat!(
                "{{\"model\":{},\"stream\":true,\"store\":false,",
                "\"max_output_tokens\":{},{}{}\"instructions\":{},",
                "\"input\":[{{\"role\":\"user\",\"content\":{}}}]}}"
            ),
            json_quote(&oracle.model),
            oracle.max_tokens,
            reasoning,
            tools,
            json_quote(&system),
            content,
        );
        Self {
            base: oracle.base.clone(),
            key: oracle.key.clone(),
            body,
            rewrite_model: oracle.rewrite_model.clone(),
            agent: oracle.agent.clone(),
        }
    }
}

fn page_text(request: &SendRequest) -> String {
    let mut text = String::new();
    if !request.ctx.history.is_empty() {
        text.push_str("Recent earlier pages, oldest first:\n");
        for (transcript, reply) in &request.ctx.history {
            text.push_str("Master wrote: ");
            text.push_str(transcript);
            text.push_str("\nMagicPaper replied: ");
            text.push_str(reply);
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(&request.user_text);
    text
}

fn response_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => format!(
            "responses http {code}: {}",
            response.into_string().unwrap_or_default().trim()
        ),
        other => format!("responses request failed: {other}"),
    }
}

fn consume_response<'a>(
    reader: impl Read,
    request: &'a SendRequest,
    asked: std::time::Instant,
) -> Result<PaperStream<'a>, StreamFailure> {
    let mut paper = PaperStream::new(request, asked);
    for line in BufReader::new(reader).lines() {
        if request.cancelled.load(Ordering::Acquire) {
            eprintln!("magicpaper: speculative Responses request cancelled");
            return Ok(paper);
        }
        let line = line.map_err(|error| StreamFailure {
            stage: "stream-read",
            message: format!("responses stream failed: {error}"),
        })?;
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        paper.accept(data);
    }
    if request.cancelled.load(Ordering::Acquire) {
        return Ok(paper);
    }
    paper.finish()?;
    Ok(paper)
}

struct PaperStream<'a> {
    request: &'a SendRequest,
    parser: StreamParser,
    accumulated: String,
    first_model_text: bool,
    first_paper_event: bool,
    searched: bool,
    search_started_logged: bool,
    search_completed_logged: bool,
    failed: Option<String>,
    holding_tail: bool,
    held_tail: String,
    sent_prefix: String,
    held_transcript: Option<String>,
    asked: std::time::Instant,
}

impl<'a> PaperStream<'a> {
    fn new(request: &'a SendRequest, asked: std::time::Instant) -> Self {
        Self {
            request,
            parser: StreamParser::new(request.catalog_ids.clone()),
            accumulated: String::new(),
            first_model_text: true,
            first_paper_event: true,
            searched: false,
            search_started_logged: false,
            search_completed_logged: false,
            failed: None,
            holding_tail: false,
            held_tail: String::new(),
            sent_prefix: String::new(),
            held_transcript: None,
            asked,
        }
    }

    fn accept(&mut self, data: &str) {
        if data.contains("web_search_call") {
            self.searched = true;
            let completed = data.contains("web_search_call.completed");
            if !completed && !self.search_started_logged {
                self.search_started_logged = true;
                eprintln!(
                    "magic-paper: event=web-search-start request={}:{} domain={} latency_ms={}",
                    std::process::id(),
                    self.request.request_id,
                    self.request.domain,
                    self.asked.elapsed().as_millis(),
                );
            }
            if completed && !self.search_completed_logged {
                self.search_completed_logged = true;
                eprintln!(
                    "magic-paper: event=web-search-done request={}:{} domain={} latency_ms={}",
                    std::process::id(),
                    self.request.request_id,
                    self.request.domain,
                    self.asked.elapsed().as_millis(),
                );
            }
        }
        if data.contains("\"type\":\"response.failed\"") {
            self.failed = json_str_field(data, "message").or_else(|| Some(data.to_owned()));
        }
        let Some(fragment) = responses_delta_content(data) else {
            return;
        };
        if self.first_model_text {
            eprintln!(
                "magicpaper: oracle first model text +{}ms",
                self.asked.elapsed().as_millis()
            );
            self.first_model_text = false;
        }
        self.accumulated.push_str(&fragment);
        let events = self.parser.advance(&self.accumulated, false);
        self.deliver(events);
    }

    fn finish(&mut self) -> Result<(), StreamFailure> {
        if let Some(detail) = self.failed.take() {
            return Err(StreamFailure {
                stage: "stream",
                message: format!("responses failed: {detail}"),
            });
        }
        if self.accumulated.trim().is_empty() {
            return Err(StreamFailure {
                stage: "empty-response",
                message: "responses returned no paper answer".into(),
            });
        }
        let events = self.parser.advance(&self.accumulated, true);
        self.deliver(events);
        Ok(())
    }

    fn deliver(&mut self, events: Vec<Result<Event, String>>) {
        for event in events {
            match event {
                Ok(Event::Ink(chunk)) => self.deliver_ink(chunk),
                Ok(Event::Transcript(transcript)) if self.holding_tail => {
                    self.held_transcript = Some(transcript);
                }
                Ok(other) => {
                    if self.first_paper_event && matches!(other, Event::Show(_)) {
                        eprintln!(
                            "magicpaper: oracle first paper event +{}ms",
                            self.asked.elapsed().as_millis()
                        );
                        self.first_paper_event = false;
                    }
                    let _ = self.request.tx.send(Ok(other));
                }
                Err(error) => {
                    log_llm_terminal(
                        &self.request.terminal,
                        self.request.request_id,
                        self.request.domain,
                        "error",
                        "paper-parse",
                        None,
                    );
                    let _ = self.request.tx.send(Err(error));
                }
            }
        }
    }

    fn deliver_ink(&mut self, chunk: String) {
        if self.holding_tail || paper_answer_needs_rewrite(&chunk) {
            self.holding_tail = true;
            if !self.held_tail.is_empty() {
                self.held_tail.push(' ');
            }
            self.held_tail.push_str(&chunk);
            return;
        }
        if self.first_paper_event {
            eprintln!(
                "magicpaper: oracle first paper text +{}ms",
                self.asked.elapsed().as_millis()
            );
            self.first_paper_event = false;
        }
        if !self.sent_prefix.is_empty() {
            self.sent_prefix.push(' ');
        }
        self.sent_prefix.push_str(&chunk);
        let _ = self.request.tx.send(Ok(Event::Ink(chunk)));
    }

    fn rewrite_held(&mut self, config: &ResponseConfig) {
        if !self.holding_tail {
            return;
        }
        let started = std::time::Instant::now();
        eprintln!(
            "magic-paper: event=paper-rewrite-start request={}:{} domain={} chars={}",
            std::process::id(),
            self.request.request_id,
            self.request.domain,
            self.held_tail.chars().count(),
        );
        let rewritten = match &config.rewrite_model {
            Some(model) => rewrite_paper_tail(
                &config.agent,
                &config.base,
                &config.key,
                model,
                &self.sent_prefix,
                &self.held_tail,
            ),
            None => Err("no paper editor model is configured".into()),
        };
        if self.request.cancelled.load(Ordering::Acquire) {
            eprintln!(
                "magic-paper: event=paper-rewrite-cancelled request={}:{} domain={} latency_ms={}",
                std::process::id(),
                self.request.request_id,
                self.request.domain,
                started.elapsed().as_millis(),
            );
            return;
        }
        let outcome = self.deliver_rewrite(rewritten);
        eprintln!(
            "magic-paper: event=paper-rewrite-{outcome} request={}:{} domain={} latency_ms={}",
            std::process::id(),
            self.request.request_id,
            self.request.domain,
            started.elapsed().as_millis(),
        );
        if let Some(transcript) = self.held_transcript.take() {
            let _ = self.request.tx.send(Ok(Event::Transcript(transcript)));
        }
    }

    fn deliver_rewrite(&mut self, rewritten: Result<String, String>) -> &'static str {
        let (text, outcome) = match rewritten {
            Ok(text) if !paper_answer_needs_rewrite(&text) => (text, "done"),
            Ok(_) | Err(_) => (paper_safe_fallback(&self.held_tail), "fallback"),
        };
        if text.trim().is_empty() || paper_answer_needs_rewrite(&text) {
            send_failure(
                &self.request.tx,
                "llm-error",
                self.request.request_id,
                self.request.domain,
                &self.request.terminal,
                "paper-editor",
                "paper editor and local fallback produced no safe answer".into(),
            );
            return "error";
        }
        if self.first_paper_event {
            eprintln!(
                "magicpaper: oracle first paper text +{}ms",
                self.asked.elapsed().as_millis()
            );
            self.first_paper_event = false;
        }
        let _ = self.request.tx.send(Ok(Event::Ink(text)));
        outcome
    }
}
