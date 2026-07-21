use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::Ordering;

use super::super::{
    json_quote, log_llm_terminal, sse_delta_content, system_prompt, Event, StreamParser,
    EXTERNAL_OCR_PROTOCOL,
};
use super::{send_failure, HttpOracle, SendRequest};

struct ChatConfig {
    base: String,
    key: String,
    model: String,
    max_tokens: u32,
    reasoning_field: String,
    system: String,
    history: String,
    user_content: String,
}

struct StreamFailure {
    stage: &'static str,
    message: String,
}

pub(super) fn send(oracle: &HttpOracle, request: SendRequest) {
    let config = ChatConfig::new(oracle, &request);
    let asked = std::time::Instant::now();
    let response = match open_response(&oracle.agent, &config) {
        Ok(response) => response,
        Err(message) => {
            send_failure(
                &request.tx,
                "llm-error",
                request.request_id,
                request.domain,
                &request.terminal,
                "request",
                message,
            );
            return;
        }
    };
    if request.cancelled.load(Ordering::Acquire) {
        return;
    }
    if let Err(error) = stream_response(response.into_reader(), &request, asked) {
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
    if !request.cancelled.load(Ordering::Acquire) {
        log_llm_terminal(
            &request.terminal,
            request.request_id,
            request.domain,
            "done",
            "chat-completions",
            Some(asked.elapsed().as_millis()),
        );
    }
}

impl ChatConfig {
    fn new(oracle: &HttpOracle, request: &SendRequest) -> Self {
        let mut system = system_prompt(oracle.remember);
        if request.external_ocr {
            system.push_str(EXTERNAL_OCR_PROTOCOL);
        }
        let history = request
            .ctx
            .history
            .iter()
            .map(|(transcript, reply)| {
                format!(
                    "{{\"role\":\"user\",\"content\":{}}},{{\"role\":\"assistant\",\"content\":{}}},",
                    json_quote(&format!("(an earlier page) {transcript}")),
                    json_quote(reply),
                )
            })
            .collect();
        let user_content = match &request.image {
            Some(image) => format!(
                concat!(
                    "[{{\"type\":\"text\",\"text\":{}}},",
                    "{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/png;base64,{}\"}}}}]"
                ),
                json_quote(&request.user_text),
                image,
            ),
            None => json_quote(&request.user_text),
        };
        Self {
            base: oracle.base.clone(),
            key: oracle.key.clone(),
            model: oracle.model.clone(),
            max_tokens: oracle.max_tokens,
            reasoning_field: oracle
                .reasoning
                .as_deref()
                .map(|value| format!("\"reasoning_effort\":{},", json_quote(value)))
                .unwrap_or_default(),
            system,
            history,
            user_content,
        }
    }
}

fn open_response(agent: &ureq::Agent, config: &ChatConfig) -> Result<ureq::Response, String> {
    match post(agent, config, "max_tokens") {
        Err(error) if matches!(*error, ureq::Error::Status(400, _)) => {
            let ureq::Error::Status(_, response) = *error else {
                unreachable!()
            };
            let detail = response.into_string().unwrap_or_default();
            if detail.contains("max_completion_tokens") {
                eprintln!("riddle: endpoint wants max_completion_tokens; retrying");
                post(agent, config, "max_completion_tokens").map_err(|error| http_error(*error))
            } else {
                Err(format!("http 400: {}", detail.trim()))
            }
        }
        other => other.map_err(|error| http_error(*error)),
    }
}

fn post(
    agent: &ureq::Agent,
    config: &ChatConfig,
    token_field: &str,
) -> Result<ureq::Response, Box<ureq::Error>> {
    let body = format!(
        concat!(
            "{{\"model\":{},\"stream\":true,\"{}\":{},{}",
            "\"messages\":[",
            "{{\"role\":\"system\",\"content\":{}}},",
            "{}",
            "{{\"role\":\"user\",\"content\":{}}}]}}"
        ),
        json_quote(&config.model),
        token_field,
        config.max_tokens,
        config.reasoning_field,
        json_quote(&config.system),
        config.history,
        config.user_content,
    );
    agent
        .post(&format!("{}/chat/completions", config.base))
        .set("Authorization", &format!("Bearer {}", config.key))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(Box::new)
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => format!(
            "http {code}: {}",
            response.into_string().unwrap_or_default().trim()
        ),
        other => format!("request failed: {other}"),
    }
}

fn stream_response(
    reader: impl Read,
    request: &SendRequest,
    asked: std::time::Instant,
) -> Result<(), StreamFailure> {
    let mut stream = ChatStream::new(request, asked);
    for line in BufReader::new(reader).lines() {
        if request.cancelled.load(Ordering::Acquire) {
            eprintln!("riddle: speculative chat request cancelled");
            return Ok(());
        }
        let line = line.map_err(|error| StreamFailure {
            stage: "stream-read",
            message: format!("response stream failed: {error}"),
        })?;
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        stream.accept(data);
    }
    if request.cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    stream.finish()
}

struct ChatStream<'a> {
    request: &'a SendRequest,
    parser: StreamParser,
    accumulated: String,
    first: bool,
    asked: std::time::Instant,
}

impl<'a> ChatStream<'a> {
    fn new(request: &'a SendRequest, asked: std::time::Instant) -> Self {
        Self {
            request,
            parser: StreamParser::new(request.catalog_ids.clone()),
            accumulated: String::new(),
            first: true,
            asked,
        }
    }

    fn accept(&mut self, data: &str) {
        let Some(fragment) = sse_delta_content(data).filter(|fragment| !fragment.is_empty()) else {
            return;
        };
        self.accumulated.push_str(&fragment);
        let events = self.parser.advance(&self.accumulated, false);
        self.emit(events);
    }

    fn finish(&mut self) -> Result<(), StreamFailure> {
        if self.accumulated.trim().is_empty() {
            return Err(StreamFailure {
                stage: "empty-response",
                message: "chat completions returned no paper answer".into(),
            });
        }
        let events = self.parser.advance(&self.accumulated, true);
        self.emit(events);
        Ok(())
    }

    fn emit(&mut self, events: Vec<Result<Event, String>>) {
        for event in events {
            if self.first {
                eprintln!(
                    "riddle: oracle first chunk +{}ms",
                    self.asked.elapsed().as_millis()
                );
                self.first = false;
            }
            if event.is_err() {
                log_llm_terminal(
                    &self.request.terminal,
                    self.request.request_id,
                    self.request.domain,
                    "error",
                    "paper-parse",
                    None,
                );
            }
            let _ = self.request.tx.send(event);
        }
    }
}
