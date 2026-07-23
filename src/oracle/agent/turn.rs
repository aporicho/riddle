//! One streamed Pi Agent turn and its fail-closed event handling.

use super::wire::{read_frame, valid_event, write_frame};
use super::{safe_tool_specs, AgentOracle, TurnRequest};
use crate::oracle::{log_llm_terminal, system_prompt, Event, StreamParser};
use serde_json::{json, Value};
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const MAX_TURN_TEXT_BYTES: usize = 64 * 1024;

impl AgentOracle {
    pub(super) fn run_turn(&self, request: TurnRequest) {
        let request_key = format!("magicpaper-{}-{}", std::process::id(), request.request_id);
        let started = Instant::now();
        self.log_start(&request);
        let mut stream = match self.connect(&request) {
            Some(stream) => stream,
            None => return,
        };
        if !self.send_start(&mut stream, &request, &request_key) {
            return;
        }
        let mut state = TurnStream::new(&request);
        loop {
            if request.cancelled.load(Ordering::Acquire) {
                self.send_cancel(&mut stream, &request_key, state.turn_id.as_deref());
                return;
            }
            let event = match read_frame(&mut stream, &request.cancelled) {
                Ok(Some(event)) => event,
                Ok(None) => continue,
                Err(error) => {
                    send_turn_error(
                        &request,
                        "agent-read",
                        format!("Pi Agent stream failed: {error}"),
                    );
                    return;
                }
            };
            if !valid_event(&event, &request_key, &self.app_id) {
                send_turn_error(
                    &request,
                    "agent-protocol",
                    "Pi Agent returned a mismatched envelope".into(),
                );
                return;
            }
            if self.handle_event(
                &mut stream,
                &request,
                &request_key,
                &mut state,
                event,
                started,
            ) {
                return;
            }
        }
    }

    fn connect(&self, request: &TurnRequest) -> Option<UnixStream> {
        match UnixStream::connect(&self.socket) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                Some(stream)
            }
            Err(error) => {
                send_turn_error(
                    request,
                    "agent-connect",
                    format!("Pi Agent unavailable: {error}"),
                );
                None
            }
        }
    }

    fn send_start(
        &self,
        stream: &mut UnixStream,
        request: &TurnRequest,
        request_key: &str,
    ) -> bool {
        let history: Vec<Value> = request
            .ctx
            .history
            .iter()
            .map(|(user, assistant)| json!({"user": user, "assistant": assistant}))
            .collect();
        let start = json!({
            "protocol": 1,
            "type": "start_turn",
            "request_id": request_key,
            "app_id": self.app_id,
            "client_token": self.client_token,
            "lane": request.lane.as_protocol_name(),
            "profile": {
                "provider": self.profile.provider,
                "model": self.profile.model,
                "thinking": self.profile.thinking,
                "tools": self.profile.tools,
            },
            "system_prompt": system_prompt(self.remember),
            "input": request.input,
            "history": history,
            "tools": safe_tool_specs(self.profile.tools),
        });
        if let Err(error) = write_frame(stream, &start) {
            send_turn_error(
                request,
                "agent-write",
                format!("Pi Agent request failed: {error}"),
            );
            return false;
        }
        true
    }

    fn handle_event(
        &self,
        stream: &mut UnixStream,
        request: &TurnRequest,
        request_key: &str,
        state: &mut TurnStream,
        event: Value,
        started: Instant,
    ) -> bool {
        let event_type = event.get("type").and_then(Value::as_str);
        if matches!(event_type, Some("text_delta" | "tool_call" | "complete"))
            && !state.matches_active_turn(&event)
        {
            send_turn_error(
                request,
                "agent-protocol",
                "Pi Agent returned an event for another turn".into(),
            );
            return true;
        }
        match event_type {
            Some("accepted") => {
                let Some(turn_id) = event
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .filter(|turn_id| !turn_id.is_empty())
                else {
                    send_turn_error(
                        request,
                        "agent-protocol",
                        "Pi Agent accepted a turn without an id".into(),
                    );
                    return true;
                };
                if state
                    .turn_id
                    .as_deref()
                    .is_some_and(|active| active != turn_id)
                {
                    send_turn_error(
                        request,
                        "agent-protocol",
                        "Pi Agent changed the active turn id".into(),
                    );
                    return true;
                }
                state.turn_id = Some(turn_id.to_owned());
                false
            }
            Some("status") => false,
            Some("text_delta") => {
                let incoming = event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if state.answer.len().saturating_add(incoming.len()) > MAX_TURN_TEXT_BYTES {
                    self.send_cancel(stream, request_key, state.turn_id.as_deref());
                    send_turn_error(
                        request,
                        "agent-limit",
                        "Pi Agent answer exceeded the paper limit".into(),
                    );
                    true
                } else {
                    state.on_text(request, &event, started)
                }
            }
            Some("tool_call") => self.reject_tool_call(
                stream,
                request,
                request_key,
                &event,
                state.turn_id.as_deref(),
            ),
            Some("complete") => {
                state.complete(request, started);
                true
            }
            Some("error") => {
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Pi Agent failed")
                    .to_owned();
                send_turn_error(request, "agent", message);
                true
            }
            _ => {
                send_turn_error(
                    request,
                    "agent-protocol",
                    "Pi Agent returned an unknown event".into(),
                );
                true
            }
        }
    }

    fn reject_tool_call(
        &self,
        stream: &mut UnixStream,
        request: &TurnRequest,
        request_key: &str,
        event: &Value,
        turn_id: Option<&str>,
    ) -> bool {
        let call_id = event
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let result = json!({
            "protocol": 1,
            "type": "tool_result",
            "request_id": request_key,
            "app_id": self.app_id,
            "client_token": self.client_token,
            "turn_id": turn_id.unwrap_or("missing"),
            "tool_call_id": call_id,
            "result": {"error":"MagicPaper tool bridge is unavailable"},
            "is_error": true,
        });
        if let Err(error) = write_frame(stream, &result) {
            send_turn_error(request, "agent-tool", error.to_string());
            return true;
        }
        false
    }

    fn send_cancel(&self, stream: &mut UnixStream, request_key: &str, turn_id: Option<&str>) {
        let Some(turn_id) = turn_id else {
            return;
        };
        let cancel = json!({
            "protocol": 1,
            "type": "cancel_turn",
            "request_id": request_key,
            "app_id": self.app_id,
            "client_token": self.client_token,
            "turn_id": turn_id,
        });
        let _ = write_frame(stream, &cancel);
    }

    fn log_start(&self, request: &TurnRequest) {
        eprintln!(
            "magic-paper: event=llm-start request={}:{} domain={} backend=pi-agent provider={} model={}",
            std::process::id(), request.request_id, request.domain, self.profile.provider, self.profile.model,
        );
    }
}

impl super::Lane {
    pub(super) fn as_protocol_name(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Speculative => "speculative",
            Self::Scheduled => "scheduled",
        }
    }
}

struct TurnStream {
    parser: StreamParser,
    answer: String,
    first: bool,
    turn_id: Option<String>,
}

impl TurnStream {
    fn new(request: &TurnRequest) -> Self {
        Self {
            parser: StreamParser::new(request.ctx.catalog_ids.clone()),
            answer: String::new(),
            first: true,
            turn_id: None,
        }
    }

    fn matches_active_turn(&self, event: &Value) -> bool {
        self.turn_id
            .as_deref()
            .is_some_and(|active| event.get("turn_id").and_then(Value::as_str) == Some(active))
    }

    fn on_text(&mut self, request: &TurnRequest, event: &Value, started: Instant) -> bool {
        let Some(text) = event.get("text").and_then(Value::as_str) else {
            send_turn_error(
                request,
                "agent-protocol",
                "Pi Agent text_delta has no text".into(),
            );
            return true;
        };
        if self.first && !text.is_empty() {
            eprintln!(
                "magicpaper: oracle first chunk +{}ms",
                started.elapsed().as_millis()
            );
            self.first = false;
        }
        self.answer.push_str(text);
        emit_parser_events(&request.tx, self.parser.advance(&self.answer, false));
        false
    }

    fn complete(&mut self, request: &TurnRequest, started: Instant) {
        emit_parser_events(&request.tx, self.parser.advance(&self.answer, true));
        if self.answer.trim().is_empty() {
            send_turn_error(
                request,
                "empty-response",
                "Pi Agent returned no paper answer".into(),
            );
        } else {
            log_llm_terminal(
                &request.terminal,
                request.request_id,
                request.domain,
                "done",
                "pi-agent",
                Some(started.elapsed().as_millis()),
            );
        }
    }
}

fn send_turn_error(request: &TurnRequest, stage: &str, error: String) {
    if log_llm_terminal(
        &request.terminal,
        request.request_id,
        request.domain,
        "error",
        stage,
        None,
    ) {
        let _ = request.tx.send(Err(error));
    }
}

fn emit_parser_events(tx: &Sender<Result<Event, String>>, events: Vec<Result<Event, String>>) {
    for event in events {
        let _ = tx.send(event);
    }
}
