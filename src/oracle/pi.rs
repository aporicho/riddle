//! Resident `pi --mode rpc` backend.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use super::{
    base64, extract_assistant_text, json_quote, json_str_field, system_prompt, turn_text, Event,
    StreamParser, TurnContext, DATA_DIR, NODE_BIN,
};

/// A warm pi RPC process. `ask` sends a turn; reply events arrive on the
/// channel, then the sender is dropped (disconnect = done).
pub struct PiOracle {
    stdin: Arc<Mutex<ChildStdin>>,
    /// Where to deliver the current reply's events. Set before each prompt,
    /// dropped on agent_end so the receiver sees a disconnect when done.
    pending: Arc<Mutex<Option<Sender<Result<Event, String>>>>>,
    /// The current turn's stream parser (routing + transcription).
    parser: Arc<Mutex<Option<StreamParser>>>,
    /// When the current prompt was sent; the reader thread logs the time to
    /// first delivered chunk (the latency the writer actually feels).
    asked: Arc<Mutex<Option<std::time::Instant>>>,
    _child: Child,
}

impl PiOracle {
    /// Spawn the resident pi process and its stdout reader thread. This pays
    /// the warmup cost once; call it at diary startup.
    pub fn spawn(remember: bool) -> std::io::Result<Self> {
        let _ = std::fs::create_dir_all(DATA_DIR);
        let path = std::env::var("PATH").unwrap_or_default();

        // Overridable so pi setups other than the stock on-device install
        // (different bin dir, provider, or model) can still power the diary.
        let node_bin = std::env::var("RIDDLE_PI_BIN_DIR").unwrap_or_else(|_| NODE_BIN.to_string());
        let provider =
            std::env::var("RIDDLE_PI_PROVIDER").unwrap_or_else(|_| "openai-codex".to_string());
        let model = std::env::var("RIDDLE_PI_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".to_string());

        let persona = system_prompt(remember);

        // Use pi's ABSOLUTE path: Rust's Command resolves the program name via
        // the PARENT's PATH, not the child env we set below, so a bare "pi"
        // would not be found when riddle is launched with a minimal PATH.
        let pi_bin = format!("{node_bin}/pi");
        let mut child = Command::new(&pi_bin)
            .current_dir(DATA_DIR)
            .env("HOME", "/home/root")
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
                // The diary only ever writes back — never let the model touch
                // tools; also trims the tool schemas from every request.
                "--no-tools",
                "--system-prompt",
                persona.as_str(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Keep pi's stderr for diagnosis instead of discarding it.
            .stderr(
                std::fs::File::create("/tmp/riddle-oracle.log")
                    .map(Stdio::from)
                    .unwrap_or_else(|_| Stdio::null()),
            )
            .spawn()?;

        let pid = child.id();
        eprintln!("riddle: oracle pi rpc spawned (pid {pid}, bin {pi_bin})");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Arc<Mutex<Option<Sender<Result<Event, String>>>>> = Arc::new(Mutex::new(None));
        let parser: Arc<Mutex<Option<StreamParser>>> = Arc::new(Mutex::new(None));

        // Reader thread: parse JSONL events, feeding the running reply text
        // through the turn's StreamParser — the quill writes far slower than
        // the model streams, so the rest arrives while the first line is drawn.
        let pending_r = Arc::clone(&pending);
        let parser_r = Arc::clone(&parser);
        let asked: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let asked_r = Arc::clone(&asked);
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let mut last_text = String::new();

            let emit = |events: Vec<Result<Event, String>>| {
                if events.is_empty() {
                    return;
                }
                if let Some(t0) = asked_r.lock().unwrap().take() {
                    eprintln!("riddle: oracle first chunk +{}ms", t0.elapsed().as_millis());
                }
                if let Some(tx) = pending_r.lock().unwrap().as_ref() {
                    for ev in events {
                        let _ = tx.send(ev);
                    }
                }
            };

            for line in reader.split(b'\n').map_while(Result::ok) {
                let Ok(s) = String::from_utf8(line) else {
                    continue;
                };
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                // Cheap field extraction avoids a JSON dep; the event stream is
                // well-formed one-object-per-line.
                let ev_type = json_str_field(s, "type");
                match ev_type.as_deref() {
                    // message_update / message_end carry the assistant
                    // message's running (then definitive) full text.
                    // (agent_end is NOT used for text — its `messages` array
                    // also contains user messages, which extract_assistant_text
                    // would wrongly concatenate in a multi-turn session.)
                    Some("message_update") | Some("message_end") => {
                        if let Some(t) = extract_assistant_text(s) {
                            if !t.is_empty() {
                                last_text = t;
                            }
                        }
                        if let Some(p) = parser_r.lock().unwrap().as_mut() {
                            emit(p.advance(&last_text, false));
                        }
                    }
                    // agent_end: the turn is over. Flush the parser, then drop
                    // the sender so the diary's receiver disconnects.
                    Some("agent_end") => {
                        if let Some(p) = parser_r.lock().unwrap().as_mut() {
                            emit(p.advance(&last_text, true));
                        }
                        *parser_r.lock().unwrap() = None;
                        pending_r.lock().unwrap().take();
                        last_text.clear();
                    }
                    _ => {}
                }
            }
            // Process died: fail any in-flight request.
            if let Some(tx) = pending_r.lock().unwrap().take() {
                let _ = tx.send(Err("pi rpc process exited".into()));
            }
        });

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            parser,
            asked,
            _child: child,
        })
    }

    /// Send a handwriting turn. Reply events are delivered on `tx` as they
    /// stream; `tx` is dropped when the reply is complete.
    pub fn ask(&self, png_path: &str, ctx: &TurnContext, tx: Sender<Result<Event, String>>) {
        let img = match std::fs::read(png_path) {
            Ok(b) => base64(&b),
            Err(e) => {
                let _ = tx.send(Err(format!("read image: {e}")));
                return;
            }
        };
        *self.pending.lock().unwrap() = Some(tx.clone());
        *self.parser.lock().unwrap() = Some(StreamParser::new(ctx.catalog_ids.clone()));
        *self.asked.lock().unwrap() = Some(std::time::Instant::now());

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
            if let Some(tx) = self.pending.lock().unwrap().take() {
                let _ = tx.send(Err("pi rpc write failed".into()));
            }
        }
    }

    pub fn ask_text(&self, prompt: &str, ctx: &TurnContext, tx: Sender<Result<Event, String>>) {
        *self.pending.lock().unwrap() = Some(tx.clone());
        *self.parser.lock().unwrap() = Some(StreamParser::new(Vec::new()));
        *self.asked.lock().unwrap() = Some(std::time::Instant::now());

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
            if let Some(tx) = self.pending.lock().unwrap().take() {
                let _ = tx.send(Err("pi rpc write failed".into()));
            }
        }
    }
}
