//! Resident `pi --mode rpc` backend.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::ink::PageCapture;

use super::{
    base64, extract_assistant_text, json_quote, json_str_field, system_prompt, turn_text, Event,
    StreamParser, TurnContext, DATA_DIR, NODE_BIN,
};

/// A warm pi RPC process. `ask` sends a turn; reply events arrive on the
/// channel, then the sender is dropped (disconnect = done).
pub struct PiOracle {
    stdin: Arc<Mutex<ChildStdin>>,
    state: Arc<ReaderState>,
    _child: Child,
}

struct ReaderState {
    pending: Mutex<Option<Sender<Result<Event, String>>>>,
    parser: Mutex<Option<StreamParser>>,
    asked: Mutex<Option<std::time::Instant>>,
    /// pi's update events have no turn id. This gate prevents a cancelled
    /// turn from overwriting the next receiver before its agent_end arrives.
    busy: AtomicBool,
    cancelled: Mutex<Option<Arc<AtomicBool>>>,
}

impl ReaderState {
    fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            parser: Mutex::new(None),
            asked: Mutex::new(None),
            busy: AtomicBool::new(false),
            cancelled: Mutex::new(None),
        }
    }
}

fn spawn_process(remember: bool) -> std::io::Result<(Child, String)> {
    let data_dir = crate::runtime_env::persistent_path("RIDDLE_PI_DATA_DIR", "pi", DATA_DIR);
    let _ = std::fs::create_dir_all(&data_dir);
    let path = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("RIDDLE_PI_HOME").unwrap_or_else(|_| "/home/root".into());
    let node_bin = std::env::var("RIDDLE_PI_BIN_DIR").unwrap_or_else(|_| NODE_BIN.to_string());
    let provider = std::env::var("RIDDLE_PI_PROVIDER").unwrap_or_else(|_| "openai-codex".into());
    let model = std::env::var("RIDDLE_PI_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".into());
    let persona = system_prompt(remember);
    let pi_bin = format!("{node_bin}/pi");
    let child = Command::new(&pi_bin)
        .current_dir(data_dir)
        .env("HOME", home)
        .env("PATH", format!("{node_bin}:{path}"))
        .args([
            "--mode",
            "rpc",
            "--provider",
            provider.as_str(),
            "--model",
            model.as_str(),
            "--thinking",
            "off",
            "--no-tools",
            "--system-prompt",
            persona.as_str(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(
            std::fs::File::create("/tmp/riddle-oracle.log")
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        )
        .spawn()?;
    Ok((child, pi_bin))
}

fn spawn_reader(stdout: ChildStdout, state: Arc<ReaderState>) {
    thread::spawn(move || read_events(stdout, &state));
}

fn read_events(stdout: ChildStdout, state: &ReaderState) {
    let reader = BufReader::new(stdout);
    let mut last_text = String::new();
    for line in reader.split(b'\n').map_while(Result::ok) {
        let Ok(line) = String::from_utf8(line) else {
            continue;
        };
        let line = line.trim();
        if !line.is_empty() {
            route_event(state, line, &mut last_text);
        }
    }
    fail_exited_process(state);
}

fn route_event(state: &ReaderState, line: &str, last_text: &mut String) {
    match json_str_field(line, "type").as_deref() {
        Some("message_update") | Some("message_end") => {
            if let Some(text) = extract_assistant_text(line).filter(|text| !text.is_empty()) {
                *last_text = text;
            }
            if !is_cancelled(state) {
                let events = state
                    .parser
                    .lock()
                    .unwrap()
                    .as_mut()
                    .map(|parser| parser.advance(last_text, false))
                    .unwrap_or_default();
                emit_events(state, events);
            }
        }
        Some("agent_end") => finish_turn(state, last_text),
        _ => {}
    }
}

fn emit_events(state: &ReaderState, events: Vec<Result<Event, String>>) {
    if events.is_empty() || is_cancelled(state) {
        return;
    }
    if let Some(started) = state.asked.lock().unwrap().take() {
        eprintln!(
            "riddle: oracle first chunk +{}ms",
            started.elapsed().as_millis()
        );
    }
    if let Some(tx) = state.pending.lock().unwrap().as_ref() {
        for event in events {
            let _ = tx.send(event);
        }
    }
}

fn is_cancelled(state: &ReaderState) -> bool {
    state
        .cancelled
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn finish_turn(state: &ReaderState, last_text: &mut String) {
    if !is_cancelled(state) {
        let events = state
            .parser
            .lock()
            .unwrap()
            .as_mut()
            .map(|parser| parser.advance(last_text, true))
            .unwrap_or_default();
        emit_events(state, events);
    }
    state.parser.lock().unwrap().take();
    if let Some(flag) = state.cancelled.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Release);
    }
    state.pending.lock().unwrap().take();
    state.cancelled.lock().unwrap().take();
    state.busy.store(false, Ordering::Release);
    last_text.clear();
}

fn fail_exited_process(state: &ReaderState) {
    if let Some(flag) = state.cancelled.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Release);
    }
    if let Some(tx) = state.pending.lock().unwrap().take() {
        let _ = tx.send(Err("pi rpc process exited".into()));
    }
    state.cancelled.lock().unwrap().take();
    state.busy.store(false, Ordering::Release);
}

impl PiOracle {
    /// Spawn the resident pi process and its stdout reader thread. This pays
    /// the warmup cost once; call it at diary startup.
    pub fn spawn(remember: bool) -> std::io::Result<Self> {
        crate::runtime_env::require_external_integrations("pi oracle")?;
        let (mut child, pi_bin) = spawn_process(remember)?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("pi stdin was piped");
        let stdout = child.stdout.take().expect("pi stdout was piped");
        let state = Arc::new(ReaderState::new());
        spawn_reader(stdout, Arc::clone(&state));
        eprintln!("riddle: oracle pi rpc spawned (pid {pid}, bin {pi_bin})");
        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            state,
            _child: child,
        })
    }

    /// Send a handwriting turn. Reply events are delivered on `tx` as they
    /// stream; `tx` is dropped when the reply is complete.
    pub fn ask(
        &self,
        png_path: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> Option<Arc<AtomicBool>> {
        let img = match std::fs::read(png_path) {
            Ok(b) => base64(&b),
            Err(e) => {
                let _ = tx.send(Err(format!("read image: {e}")));
                return None;
            }
        };
        let cancelled = self.begin_turn(tx, StreamParser::new(ctx.catalog_ids.clone()))?;

        // pi keeps its own conversation, so history isn't resent — only the
        // catalog (it changes every turn) rides along.
        let cmd = format!(
            "{{\"type\":\"prompt\",\"message\":{},\"images\":[{{\"type\":\"image\",\"data\":\"{}\",\"mimeType\":\"image/png\"}}]}}\n",
            json_quote(&turn_text(ctx)),
            img
        );
        let mut stdin = self.stdin.lock().unwrap();
        if stdin
            .write_all(cmd.as_bytes())
            .and_then(|_| stdin.flush())
            .is_err()
        {
            self.fail_turn("pi rpc write failed");
        }
        Some(cancelled)
    }

    pub fn ask_capture(
        &self,
        capture: PageCapture,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> Option<Arc<AtomicBool>> {
        let cancelled = self.begin_turn(tx, StreamParser::new(ctx.catalog_ids.clone()))?;
        let worker_cancel = Arc::clone(&cancelled);
        let stdin = Arc::clone(&self.stdin);
        let state = Arc::clone(&self.state);
        let message = json_quote(&turn_text(ctx));
        thread::spawn(move || {
            let png = match capture.encode_png(&worker_cancel) {
                Ok(png) => png,
                Err(_) if worker_cancel.load(Ordering::Acquire) => {
                    abandon_prepared_turn(&state);
                    return;
                }
                Err(error) => {
                    fail_reader_state(&state, &format!("encode page: {error}"));
                    return;
                }
            };
            if worker_cancel.load(Ordering::Acquire) {
                abandon_prepared_turn(&state);
                return;
            }
            let command = format!(
                "{{\"type\":\"prompt\",\"message\":{},\"images\":[{{\"type\":\"image\",\"data\":\"{}\",\"mimeType\":\"image/png\"}}]}}\n",
                message,
                base64(&png),
            );
            let failed = {
                let mut input = stdin.lock().unwrap();
                input
                    .write_all(command.as_bytes())
                    .and_then(|_| input.flush())
                    .is_err()
            };
            if failed {
                fail_reader_state(&state, "pi rpc write failed");
            }
        });
        Some(cancelled)
    }

    pub fn ask_text(
        &self,
        prompt: &str,
        ctx: &TurnContext,
        tx: Sender<Result<Event, String>>,
    ) -> Option<Arc<AtomicBool>> {
        let cancelled = self.begin_turn(tx, StreamParser::new(Vec::new()))?;

        let context = turn_text(ctx);
        let message = format!("{context}\n\n{prompt}");
        let cmd = format!(
            "{{\"type\":\"prompt\",\"message\":{}}}\n",
            json_quote(&message)
        );
        let mut stdin = self.stdin.lock().unwrap();
        if stdin
            .write_all(cmd.as_bytes())
            .and_then(|_| stdin.flush())
            .is_err()
        {
            self.fail_turn("pi rpc write failed");
        }
        Some(cancelled)
    }

    fn begin_turn(
        &self,
        tx: Sender<Result<Event, String>>,
        parser: StreamParser,
    ) -> Option<Arc<AtomicBool>> {
        if self
            .state
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ = tx.send(Err(
                "pi rpc previous turn is still finishing after cancellation".into(),
            ));
            return None;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.state.pending.lock().unwrap() = Some(tx);
        *self.state.parser.lock().unwrap() = Some(parser);
        *self.state.asked.lock().unwrap() = Some(std::time::Instant::now());
        *self.state.cancelled.lock().unwrap() = Some(Arc::clone(&cancelled));
        Some(cancelled)
    }

    fn fail_turn(&self, message: &str) {
        fail_reader_state(&self.state, message);
    }
}

fn fail_reader_state(state: &ReaderState, message: &str) {
    if let Some(tx) = state.pending.lock().unwrap().take() {
        let _ = tx.send(Err(message.into()));
    }
    state.parser.lock().unwrap().take();
    state.asked.lock().unwrap().take();
    if let Some(flag) = state.cancelled.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Release);
    }
    state.cancelled.lock().unwrap().take();
    state.busy.store(false, Ordering::Release);
}

fn abandon_prepared_turn(state: &ReaderState) {
    state.pending.lock().unwrap().take();
    state.parser.lock().unwrap().take();
    state.asked.lock().unwrap().take();
    state.cancelled.lock().unwrap().take();
    state.busy.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests;
