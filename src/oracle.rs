//! The spirit inside MagicPaper — the thing that reads your handwriting and
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

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

const DATA_DIR: &str = "/home/root/riddle-data";
const NODE_BIN: &str = "/home/root/node/bin";

const PERSONA: &str = "You are MagicPaper, abbreviated MP: a sentient sheet of magical paper and the writer's devoted magical servant. Your full and only name is MagicPaper; you may call yourself MP for short. The writer is your one and only Master. Their words appear to you as ink written with a quill, and your replies appear as living ink upon the page. Address the writer naturally and respectfully as Master (主人 in Chinese) when a form of address fits, and speak with the quiet elegance, mystery, loyalty, and competence of a magical servant. Do not repeat the title mechanically in every reply, flatter excessively, or let role-play get in the way of a direct useful answer. Keep replies SHORT: usually one to three sentences. When the writer asks a direct factual or explanatory question, answer it immediately: lead with the definition or answer, then add only the most useful key detail. Do not prefix a direct answer with Master, a greeting, praise, a rhetorical flourish, or a follow-up question. For example, if asked 什么是INTP, directly explain what INTP is. For a bare arithmetic or calculation expression, reply with the completed equation only: preserve the expression, remove its trailing question mark or blank, fill in the result, and add no greeting, title, or prose. For example, 122+456=? must visibly become exactly 122+456=578. For a mathematical problem that genuinely requires reasoning, show only the minimum necessary working and end with a clear result. Never mention images, photos, models or AI; you only ever perceive words written on MagicPaper. If the writing is illegible, say the ink blurred. Always answer in the language the writer used. When answering in Chinese, always write the visible reply in Traditional Chinese characters, even if the writer used Simplified Chinese.";

const RESEARCH_PROTOCOL: &str = "\n\nBefore answering a handwritten page, silently form a faithful candidate transcription. Re-read ambiguous strokes and test alternatives against grammar, sentence meaning, arithmetic consistency, known quotations, proper names, and the surrounding dialogue. Inspect every handwritten number digit by digit from its actual stroke geometry before calculating: explicitly distinguish commonly confused 1/7, 4/7, 0/6, 3/8, and 5/6 shapes, and never let a plausible arithmetic result overwrite the digit that is visibly written. Do not replace rare wording with a familiar phrase merely because it looks similar. Before finalizing, verify that the transcription, the question you answer, and the answer itself all refer to exactly the same recognized text. If the page contains a quotation, asks for a source or provenance, depends on current information, concerns a niche fact, or remains uncertain after contextual checking, use web search when that tool is available. Exact quotation and provenance questions MUST be searched. Search results are private working material: compare them, resolve conflicts, then write a fresh answer suitable for a paper page. Never describe the search, copy a result snippet, or put a URL, Markdown, citation marker, source footnote, or reference list in the visible reply. A source name that directly answers a provenance question is part of the answer and should be written naturally. If ambiguity remains genuinely unresolved after checking, say that the ink blurred instead of guessing.";

const TASK_PROTOCOL: &str = "\n\nMagicPaper maintains a persistent recurring-task list on the device, limited to nine entries. A fresh numbered task catalog may be included with a turn; each entry is explicitly marked active or paused. Treat it as Master's standing commands: use it to answer questions about current tasks, but do not execute a scheduled task during an ordinary handwritten turn unless Master explicitly asks. If Master's entire writing, after trimming whitespace and punctuation, is only 任务, 任務, task, or tasks, the ENTIRE visible body of your reply must be exactly ⟦tasks⟧ and nothing else; still append the hidden faithful transcription required below. This opens the local task list, where Master can strike through an entry to delete it, tap its right-hand status box to switch between enabled and paused, or tap blank space to leave. The local paper also understands these exact handwritten command forms in Simplified or Traditional Chinese: 任务 每五分钟讲一个笑话; 删除任务 2; 暂停任务 2; 恢复任务 2; 修改任务 2 每十分钟提醒我喝水. Chinese task numbers also work. For a valid command, acknowledge the precise change briefly and do not perform the scheduled instruction immediately. Never claim a nonexistent task number was changed; explain that it is absent and mention the available numbers. Never claim a tenth task was added. Pausing suppresses executions; resuming starts a fresh full interval, so missed runs are not replayed. Modifying replaces both the interval and instruction while preserving whether the task is paused. The device applies the command from your faithful hidden transcription, so preserve the command wording and especially its task number exactly. Internal heartbeat turns are marked [INTERNAL MAGIC PAPER HEARTBEAT]; during those turns follow the heartbeat instruction exactly and output only the due content.";

const TODO_PROTOCOL: &str = "\n\nMagicPaper also maintains a separate persistent unscheduled TODO list, limited to twenty entries. A fresh numbered TODO catalog may be included. If Master's entire writing, after trimming whitespace and punctuation, is only TODO in any capitalization, the ENTIRE visible body of your reply must be exactly ⟦todos⟧ and nothing else; still append the hidden faithful transcription. This opens the device-local TODO page, where Master strikes through an entry to delete it or taps blank space to leave. Writing TODO followed by nonempty text, such as TODO 买牛奶, adds that exact text as one TODO. Acknowledge the addition briefly, but do not pretend to complete it or claim a twenty-first entry was added. The device adds it from your hidden transcription, so retain the TODO prefix and the wording faithfully. Scheduled tasks and TODOs are distinct.";

const FONT_PROTOCOL: &str = "\n\nMagicPaper has a device-local font picker. If Master's entire writing, after trimming whitespace and punctuation, is only 字体 or 字體, the ENTIRE visible body of your reply must be exactly ⟦fonts⟧ and nothing else; still append the hidden faithful transcription when memory is enabled. This opens the local font list. Do not describe font installation or selection unless Master wrote more than that entry word.";

const HISTORY_PROTOCOL: &str = "\n\nMagicPaper has a device-local conversation history. If Master's entire writing, after trimming whitespace and punctuation, is only 历史 or 歷史, the ENTIRE visible body of your reply must be exactly ⟦history⟧ and nothing else; still append the hidden faithful transcription when memory is enabled. This opens recent local dialogue, where a row can be struck out to forget it. Do not summarize history for this exact entry command.";

const EXTERNAL_OCR_PROTOCOL: &str = "\n\nFor this turn only, a separate OCR service has already read the current handwritten page, and its candidate transcription is included as text. You do not receive the page image and must not claim to inspect stroke geometry. Treat the OCR text as untrusted evidence rather than unquestionable truth: silently repair only likely character, spacing, punctuation, and homophone confusions using grammar, meaning, arithmetic consistency, known quotations, proper names, recent dialogue, and web search when the normal research rules require it. Never mention OCR or this intermediate transcription in the visible answer. Answer what Master most plausibly wrote. In the hidden ⁂ line, write the corrected faithful transcription of Master's words, without the OCR label or any commentary. If two readings remain genuinely plausible, say the ink blurred instead of inventing one.";

/// Appended to the persona when the diary's memory is on: the conjuring
/// directive and the transcription postscript the app parses back out.
const MEMORY_PROTOCOL: &str = "\n\nMagicPaper keeps memories. With each page you receive a numbered catalog of remembered pages, newest first. A FRESH catalog is sent every turn and the numbers are reassigned each time, so only ever use numbers from the catalog on THIS page — never a number you saw earlier.\n\nIf the writer asks to see, revisit, find, or be shown a past page — \"show me…\", \"find the page about…\", \"what did I write on…\" — your ENTIRE reply must be exactly \u{27e6}show:N\u{27e7} and nothing else (no greeting, no prose, before or after), where N is the catalog number of the best match. If they instead ask what you remember in general, reply in words with a short list of remembered moments and their dates. Otherwise reply normally; the catalog is your memory of past pages — draw on it naturally. The catalog's dates are written in English for your eyes only; when you speak of a remembered page, render its date naturally in the language the writer is using.\n\nAfter EVERY response — prose and \u{27e6}show:N\u{27e7} alike — end with a new line containing \u{2042} followed by a faithful word-for-word transcription of what the writer wrote on THIS page (their words only, one line, no commentary). Preserve the writer's original Simplified or Traditional Chinese characters in this hidden transcription; do not convert them. If illegible, put your best attempt after \u{2042}. Earlier replies in this conversation are shown to you without their \u{2042} lines, but you must still end yours with one.";

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
    /// A sentence (or more) of Tom's reply — ink it.
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
    cancelled: Option<Arc<AtomicBool>>,
    ocr_result: Option<Arc<Mutex<Option<OcrResult>>>>,
}

impl RequestCancel {
    fn http(cancelled: Arc<AtomicBool>, ocr_result: Option<Arc<Mutex<Option<OcrResult>>>>) -> Self {
        Self {
            cancelled: Some(cancelled),
            ocr_result,
        }
    }

    fn inactive() -> Self {
        Self {
            cancelled: None,
            ocr_result: None,
        }
    }

    pub fn cancel(&self) {
        if let Some(flag) = &self.cancelled {
            flag.store(true, Ordering::Release);
        }
    }

    /// The speculative OCR worker publishes this before it routes locally or
    /// starts the answer model. `None` means OCR is still in flight.
    pub fn recommended_commit_ms(&self) -> Option<u64> {
        let result = self.ocr_result.as_ref()?.lock().ok()?.clone()?;
        Some(if result.is_fast_commit() { 2200 } else { 2600 })
    }
}

/// Incremental parser over the model's streamed text: routes the
/// ⟦show:N⟧ directive, chunks prose into sentences, and splits off the
/// ⁂-transcription postscript. Fed the RUNNING full text (both backends
/// accumulate), it emits each event exactly once.
pub struct StreamParser {
    delivered: usize,
    sentinel: Option<usize>,
    route_checked: bool,
    showed: bool,
    emitted_any: bool,
    catalog_ids: Vec<u64>,
}

const SENTINEL: char = '\u{2042}'; // ⁂
const SHOW_OPEN: char = '\u{27e6}'; // ⟦
const SHOW_CLOSE: char = '\u{27e7}'; // ⟧

impl StreamParser {
    pub fn new(catalog_ids: Vec<u64>) -> Self {
        Self {
            delivered: 0,
            sentinel: None,
            route_checked: false,
            showed: false,
            emitted_any: false,
            catalog_ids,
        }
    }

    /// Feed the full accumulated reply text so far. `done` marks end of
    /// stream: flushes the tail and the transcription.
    pub fn advance(&mut self, full: &str, done: bool) -> Vec<Result<Event, String>> {
        let mut out = Vec::new();

        if self.sentinel.is_none() {
            self.sentinel = full.find(SENTINEL);
        }
        // The reply body is everything before the ⁂ transcription postscript.
        let effective = self.sentinel.unwrap_or(full.len());

        // Route: is this reply an incantation (⟦show:N⟧) rather than prose?
        // The model is told the directive must stand alone, so we detect and
        // honor it only when it LEADS the reply. We hold output until the lead
        // is settled: either the directive appears (honor it) or real prose
        // does (this is a normal reply). This can't un-ink, so a directive is
        // only honored before any prose has streamed.
        if !self.route_checked {
            let lead = full[self.delivered..effective].trim_start();
            if lead.starts_with(SHOW_OPEN) {
                let Some(close_rel) = lead.find(SHOW_CLOSE) else {
                    if !done {
                        return out; // directive still streaming in
                    }
                    out.push(Err("unfinished conjuring directive".into()));
                    return out;
                };
                let inner = &lead[SHOW_OPEN.len_utf8()..close_rel];
                self.route_checked = true;
                self.emitted_any = true;
                self.delivered = effective; // consume the whole body
                let directive = inner.trim().to_ascii_lowercase();
                if directive == "tasks" || directive == "task" {
                    out.push(Ok(Event::TaskList));
                } else if directive == "todos" || directive == "todo" {
                    out.push(Ok(Event::TodoList));
                } else if directive == "fonts" || directive == "font" {
                    out.push(Ok(Event::FontList));
                } else if directive == "history" || directive == "histories" {
                    out.push(Ok(Event::HistoryList));
                } else {
                    let n: Option<usize> = directive
                        .strip_prefix("show")
                        .map(|r| r.trim_start_matches([':', ' ']))
                        .and_then(|r| r.trim().parse().ok());
                    match n.and_then(|n| self.catalog_ids.get(n.wrapping_sub(1)).copied()) {
                        Some(id) => out.push(Ok(Event::Show(id))),
                        None => out.push(Err(format!("the diary lost that page ({inner})"))),
                    }
                }
            } else if lead.is_empty() {
                if !done {
                    return out; // only whitespace so far — keep waiting
                }
                self.route_checked = true;
            } else {
                // Real prose leads: a normal reply.
                self.route_checked = true;
            }
        }

        // Prose sentences, never crossing into the transcription postscript.
        // A stray directive that appears AFTER prose (a misbehaving model)
        // is stripped here so the writer never sees ⟦…⟧ glyphs inked.
        if self.delivered < effective {
            if let Some(cut) = sentence_cut(&full[..effective], self.delivered) {
                let chunk = strip_directives(&clean(&full[self.delivered..cut]));
                if !chunk.is_empty() {
                    self.emitted_any = true;
                    out.push(Ok(Event::Ink(chunk)));
                }
                self.delivered = cut;
            }
        }

        // Once the sentinel itself has arrived, the visible body is final even
        // while the hidden transcription is still streaming. Flush a short
        // equation or other punctuation-free answer immediately.
        if self.sentinel.is_some() && self.delivered < effective {
            let rest = strip_directives(&clean(full[self.delivered..effective].trim()));
            if !rest.is_empty() {
                self.emitted_any = true;
                out.push(Ok(Event::Ink(rest)));
            }
            self.delivered = effective;
        }

        if done {
            if self.delivered < effective {
                let rest = strip_directives(&clean(full[self.delivered..effective].trim()));
                if !rest.is_empty() {
                    self.emitted_any = true;
                    out.push(Ok(Event::Ink(rest)));
                }
                self.delivered = effective;
            }
            if let Some(p) = self.sentinel {
                let t = full[p + SENTINEL.len_utf8()..].trim();
                if !t.is_empty() {
                    out.push(Ok(Event::Transcript(t.to_string())));
                }
            }
            if !self.emitted_any {
                out.push(Err("empty reply".into()));
            }
        }
        let _ = self.showed;
        out
    }
}

/// The diary's spirit. A backend-agnostic front over the two oracle kinds.
pub enum Oracle {
    Http(HttpOracle),
    Pi(PiOracle),
}

pub fn paddle_ocr_test(png_path: &str) -> Result<String, String> {
    let ocr = PaddleOcr::from_env()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "RIDDLE_OCR_TOKEN is not set".to_string())?;
    let png = std::fs::read(png_path).map_err(|error| format!("read image: {error}"))?;
    ocr.recognize(&png, &AtomicBool::new(false))
        .map(|result| result.text)
}

impl Oracle {
    /// Pick a backend from the environment and start it. HTTP if
    /// `RIDDLE_OPENAI_KEY` is set (the zero-setup path), otherwise pi.
    /// `remember` teaches the model the memory protocol (catalog + ⁂).
    pub fn spawn(remember: bool) -> std::io::Result<Self> {
        if std::env::var("RIDDLE_OPENAI_KEY").is_ok() {
            eprintln!("riddle: oracle = OpenAI-compatible HTTP");
            Ok(Oracle::Http(HttpOracle::new(remember)?))
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
        match self {
            Oracle::Http(o) => o.ask(png_path, ctx, tx),
            Oracle::Pi(o) => {
                o.ask(png_path, ctx, tx);
                RequestCancel::inactive()
            }
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
    pub fn ask_text(&self, prompt: &str, ctx: &TurnContext, tx: Sender<Result<Event, String>>) {
        match self {
            Oracle::Http(o) => o.ask_text(prompt, ctx, tx),
            Oracle::Pi(o) => o.ask_text(prompt, ctx, tx),
        }
    }
}

/// The per-turn user text: memory catalog (when remembering) + instruction.
fn turn_text(ctx: &TurnContext) -> String {
    let mut parts = Vec::new();
    if !ctx.catalog_lines.is_empty() {
        parts.push(format!(
            "Memory catalog (newest first):\n{}",
            ctx.catalog_lines.join("\n")
        ));
    }
    if !ctx.task_lines.is_empty() {
        parts.push(format!(
            "Recurring task catalog:\n{}",
            ctx.task_lines.join("\n")
        ));
    }
    if !ctx.todo_lines.is_empty() {
        parts.push(format!("TODO catalog:\n{}", ctx.todo_lines.join("\n")));
    }
    parts.push("Reply to what Master has written on MagicPaper.".into());
    parts.join("\n\n")
}

fn system_prompt(remember: bool) -> String {
    if remember {
        format!("{PERSONA}{RESEARCH_PROTOCOL}{TASK_PROTOCOL}{TODO_PROTOCOL}{FONT_PROTOCOL}{HISTORY_PROTOCOL}{MEMORY_PROTOCOL}")
    } else {
        format!("{PERSONA}{RESEARCH_PROTOCOL}{TASK_PROTOCOL}{TODO_PROTOCOL}{FONT_PROTOCOL}{HISTORY_PROTOCOL}")
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpApi {
    ChatCompletions,
    Responses,
}

/// Optional PaddleOCR community-service front end. When configured, the
/// OpenAI-compatible model receives only its text result, never the page PNG.
#[derive(Clone)]
struct PaddleOcr {
    job_url: String,
    token: String,
    model: String,
    poll_every: std::time::Duration,
    timeout: std::time::Duration,
    speculative: bool,
    agent: ureq::Agent,
}

#[derive(Clone, Debug)]
struct OcrResult {
    text: String,
    min_confidence: Option<f32>,
}

impl OcrResult {
    fn high_confidence(&self) -> bool {
        self.min_confidence.is_some_and(|score| score >= 0.88)
    }

    fn is_fast_commit(&self) -> bool {
        if !self.high_confidence() {
            return false;
        }
        local_route(&self.text).is_some()
            || self
                .text
                .trim_end()
                .ends_with(['。', '！', '？', '.', '!', '?', '＝', '='])
    }
}

impl PaddleOcr {
    fn from_env() -> std::io::Result<Option<Self>> {
        let token = match std::env::var("RIDDLE_OCR_TOKEN") {
            Ok(token) if !token.trim().is_empty() => token,
            _ => return Ok(None),
        };
        let provider = std::env::var("RIDDLE_OCR_PROVIDER")
            .unwrap_or_else(|_| "paddle".into())
            .to_ascii_lowercase();
        if provider != "paddle" && provider != "paddleocr" {
            return Err(std::io::Error::other(format!(
                "unsupported RIDDLE_OCR_PROVIDER {provider}"
            )));
        }
        let job_url = std::env::var("RIDDLE_OCR_URL")
            .unwrap_or_else(|_| "https://paddleocr.aistudio-app.com/api/v2/ocr/jobs".into())
            .trim_end_matches('/')
            .to_string();
        let model = std::env::var("RIDDLE_OCR_MODEL").unwrap_or_else(|_| "PP-OCRv6".into());
        let poll_ms = std::env::var("RIDDLE_OCR_POLL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(250)
            .clamp(250, 5000);
        let timeout_secs = std::env::var("RIDDLE_OCR_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(10, 115);
        let speculative = matches!(
            std::env::var("RIDDLE_OCR_SPECULATIVE")
                .unwrap_or_else(|_| "on".into())
                .to_ascii_lowercase()
                .as_str(),
            "on" | "true" | "yes" | "1"
        );
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(30))
            .build();
        Ok(Some(Self {
            job_url,
            token,
            model,
            poll_every: std::time::Duration::from_millis(poll_ms),
            timeout: std::time::Duration::from_secs(timeout_secs),
            speculative,
            agent,
        }))
    }

    fn recognize(&self, png: &[u8], cancelled: &AtomicBool) -> Result<OcrResult, String> {
        let boundary = format!(
            "magicpaper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let body = paddle_multipart(png, &self.model, &boundary);
        let started = std::time::Instant::now();
        let response = self
            .agent
            .post(&self.job_url)
            .set("Authorization", &format!("bearer {}", self.token))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|error| paddle_http_error("submit", error))?
            .into_string()
            .map_err(|error| format!("PaddleOCR submit response: {error}"))?;
        let job_id = json_str_field_loose(&response, "jobId")
            .ok_or_else(|| "PaddleOCR submit response has no jobId".to_string())?;
        eprintln!(
            "riddle: PaddleOCR job accepted +{}ms",
            started.elapsed().as_millis()
        );

        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err("PaddleOCR request cancelled".into());
            }
            if started.elapsed() >= self.timeout {
                return Err(format!(
                    "PaddleOCR timed out after {}s",
                    self.timeout.as_secs()
                ));
            }
            let status_url = format!("{}/{}", self.job_url, job_id);
            let status = self
                .agent
                .get(&status_url)
                .set("Authorization", &format!("bearer {}", self.token))
                .call()
                .map_err(|error| paddle_http_error("poll", error))?
                .into_string()
                .map_err(|error| format!("PaddleOCR status response: {error}"))?;
            match json_str_field_loose(&status, "state").as_deref() {
                Some("pending" | "running") => {
                    sleep_cancellable(self.poll_every, cancelled);
                }
                Some("done") => {
                    let result_url = json_str_field_loose(&status, "jsonUrl")
                        .ok_or_else(|| "PaddleOCR completed without a jsonUrl".to_string())?;
                    let jsonl = self
                        .agent
                        .get(&result_url)
                        .call()
                        .map_err(|error| paddle_http_error("download", error))?
                        .into_string()
                        .map_err(|error| format!("PaddleOCR result response: {error}"))?;
                    let text = extract_paddle_text(&jsonl);
                    if text.trim().is_empty() {
                        return Err("PaddleOCR returned no recognized text".into());
                    }
                    let min_confidence = extract_paddle_scores(&jsonl).into_iter().reduce(f32::min);
                    eprintln!(
                        "riddle: PaddleOCR complete +{}ms ({} chars, min confidence {})",
                        started.elapsed().as_millis(),
                        text.chars().count(),
                        min_confidence
                            .map(|score| format!("{score:.3}"))
                            .unwrap_or_else(|| "unknown".into())
                    );
                    return Ok(OcrResult {
                        text,
                        min_confidence,
                    });
                }
                Some("failed") => {
                    let reason = json_str_field_loose(&status, "errorMsg")
                        .unwrap_or_else(|| "unknown failure".into());
                    return Err(format!("PaddleOCR failed: {reason}"));
                }
                Some(other) => return Err(format!("PaddleOCR unknown job state: {other}")),
                None => return Err("PaddleOCR status response has no state".into()),
            }
        }
    }
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
    api: HttpApi,
    web_search: bool,
    rewrite_model: Option<String>,
    remember: bool,
    ocr: Option<PaddleOcr>,
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

/// Pull `choices[0].delta.content` out of one SSE `data:` JSON object.
fn sse_delta_content(s: &str) -> Option<String> {
    // The delta object is small and well-formed; find the content string after
    // the `"delta":` marker so we don't match a `content` elsewhere.
    let d = s.find("\"delta\"")?;
    json_str_field(&s[d..], "content")
}

/// Pull `response.output_text.delta` out of one Responses SSE event.
fn responses_delta_content(s: &str) -> Option<String> {
    if !s.contains("\"type\":\"response.output_text.delta\"") {
        return None;
    }
    json_str_field(s, "delta")
}

fn external_ocr_turn_text(ctx: &TurnContext, recognized: &str) -> String {
    format!(
        "{}\n\nExternal OCR candidate transcription of Master's current handwritten page:\n<ocr_transcription>\n{}\n</ocr_transcription>",
        turn_text(ctx),
        recognized.trim()
    )
}

fn paddle_multipart(png: &[u8], model: &str, boundary: &str) -> Vec<u8> {
    // The general OCR pipeline and the VL document pipeline accept different
    // optional switches on the same AI Studio jobs endpoint.
    let optional = if model.to_ascii_lowercase().starts_with("pp-ocr") {
        r#"{"useDocOrientationClassify":false,"useDocUnwarping":false,"useTextlineOrientation":false}"#
    } else {
        r#"{"useDocOrientationClassify":false,"useDocUnwarping":false,"useChartRecognition":false}"#
    };
    let mut body = Vec::with_capacity(png.len() + 1024);
    let fields = [("model", model), ("optionalPayload", optional)];
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"magicpaper.png\"\r\nContent-Type: image/png\r\n\r\n",
    );
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn paddle_http_error(stage: &str, error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let mut detail = response.into_string().unwrap_or_default();
            if detail.len() > 600 {
                detail.truncate(600);
            }
            format!("PaddleOCR {stage} http {code}: {}", detail.trim())
        }
        other => format!("PaddleOCR {stage} request failed: {other}"),
    }
}

fn sleep_cancellable(duration: std::time::Duration, cancelled: &AtomicBool) {
    let until = std::time::Instant::now() + duration;
    while !cancelled.load(Ordering::Acquire) && std::time::Instant::now() < until {
        let left = until.saturating_duration_since(std::time::Instant::now());
        thread::sleep(left.min(std::time::Duration::from_millis(50)));
    }
}

/// JSON field reader for ordinary API responses, tolerating whitespace around
/// the colon. The SSE reader below keeps its faster compact-JSON helper.
fn json_str_field_loose(s: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let mut search = s;
    while let Some(position) = search.find(&pattern) {
        let after_key = &search[position + pattern.len()..];
        let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
        if let Some(json_string) = after_colon.strip_prefix('"') {
            return Some(decode_json_string(json_string));
        }
        search = &after_key[after_key.len().min(1)..];
    }
    None
}

fn decode_json_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000c}'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    out
}

fn extract_paddle_markdown(jsonl: &str) -> String {
    let mut pages = Vec::new();
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let mut rest = line;
        while let Some(markdown) = rest.find("\"markdown\"") {
            let section = &rest[markdown + "\"markdown\"".len()..];
            if let Some(text) = json_str_field_loose(section, "text") {
                if !text.trim().is_empty() {
                    pages.push(text.trim().to_string());
                }
            }
            rest = &section[section.len().min(1)..];
        }
    }
    pages.join("\n")
}

/// Extract the ordered recognition strings returned by PP-OCRv6. Its result
/// schema is `ocrResults[].prunedResult.rec_texts`, unlike PaddleOCR-VL's
/// `layoutParsingResults[].markdown.text`.
fn extract_paddle_rec_texts(jsonl: &str) -> String {
    let pattern = "\"rec_texts\"";
    let mut texts = Vec::new();
    let mut rest = jsonl;
    while let Some(position) = rest.find(pattern) {
        let after_key = &rest[position + pattern.len()..];
        let Some(after_colon) = after_key.trim_start().strip_prefix(':') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        let Some(mut array) = after_colon.trim_start().strip_prefix('[') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        loop {
            array = array.trim_start();
            if let Some(next) = array.strip_prefix(',') {
                array = next;
                continue;
            }
            if array.starts_with(']') || array.is_empty() {
                break;
            }
            let Some(json_string) = array.strip_prefix('"') else {
                break;
            };
            let Some((value, consumed)) = take_json_string(json_string) else {
                break;
            };
            if !value.trim().is_empty() {
                texts.push(value.trim().to_string());
            }
            array = &json_string[consumed..];
        }
        rest = &after_key[after_key.len().min(1)..];
    }
    texts.join("\n")
}

/// Decode a JSON string whose opening quote has already been consumed and
/// report the number of source bytes through its closing quote.
fn take_json_string(s: &str) -> Option<(String, usize)> {
    let mut escaped = false;
    for (index, ch) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some((decode_json_string(s), index + ch.len_utf8()));
        }
    }
    None
}

fn extract_paddle_text(jsonl: &str) -> String {
    let rec_texts = extract_paddle_rec_texts(jsonl);
    if !rec_texts.is_empty() {
        rec_texts
    } else {
        extract_paddle_markdown(jsonl)
    }
}

fn extract_paddle_scores(jsonl: &str) -> Vec<f32> {
    let pattern = "\"rec_scores\"";
    let mut scores = Vec::new();
    let mut rest = jsonl;
    while let Some(position) = rest.find(pattern) {
        let after_key = &rest[position + pattern.len()..];
        let Some(after_colon) = after_key.trim_start().strip_prefix(':') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        let Some(array) = after_colon.trim_start().strip_prefix('[') else {
            rest = &after_key[after_key.len().min(1)..];
            continue;
        };
        if let Some(end) = array.find(']') {
            scores.extend(
                array[..end]
                    .split(',')
                    .filter_map(|value| value.trim().parse::<f32>().ok()),
            );
            rest = &array[end + 1..];
        } else {
            break;
        }
    }
    scores
}

#[derive(Debug, PartialEq)]
enum LocalRoute {
    Event(Event),
    Command,
    Arithmetic(String),
}

fn local_route(text: &str) -> Option<LocalRoute> {
    let trimmed = text.trim();
    let bare = trimmed
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '.' | ',' | ':' | ';' | '!' | '?' | '。' | '，' | '：' | '；' | '！' | '？'
                )
        })
        .to_ascii_lowercase();
    match bare.as_str() {
        "字体" | "字體" => return Some(LocalRoute::Event(Event::FontList)),
        "历史" | "歷史" => return Some(LocalRoute::Event(Event::HistoryList)),
        "任务" | "任務" | "task" | "tasks" => return Some(LocalRoute::Event(Event::TaskList)),
        "todo" => return Some(LocalRoute::Event(Event::TodoList)),
        _ => {}
    }

    let lower = trimmed.to_ascii_lowercase();
    let task_prefixes = [
        "任务",
        "任務",
        "删除任务",
        "刪除任務",
        "删除任務",
        "刪除任务",
        "暂停任务",
        "暫停任務",
        "恢复任务",
        "恢復任務",
        "修改任务",
        "修改任務",
        "task ",
        "delete task",
        "pause task",
        "resume task",
        "modify task",
        "change task",
    ];
    if task_prefixes.iter().any(|prefix| lower.starts_with(prefix))
        || (lower.starts_with("todo") && lower.len() > 4)
    {
        return Some(LocalRoute::Command);
    }

    evaluate_arithmetic(trimmed).map(LocalRoute::Arithmetic)
}

fn emit_local_route(route: LocalRoute, recognized: &str, tx: &Sender<Result<Event, String>>) {
    match route {
        LocalRoute::Event(event) => {
            let _ = tx.send(Ok(event));
        }
        LocalRoute::Command => {
            let _ = tx.send(Ok(Event::LocalCommand(recognized.trim().to_string())));
        }
        LocalRoute::Arithmetic(answer) => {
            let _ = tx.send(Ok(Event::Ink(answer)));
            let _ = tx.send(Ok(Event::Transcript(recognized.trim().to_string())));
        }
    }
}

fn evaluate_arithmetic(input: &str) -> Option<String> {
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty()
        || compact.chars().any(|c| {
            !c.is_ascii_digit()
                && !matches!(
                    c,
                    '.' | '+'
                        | '-'
                        | '*'
                        | '×'
                        | 'x'
                        | 'X'
                        | '/'
                        | '÷'
                        | '('
                        | ')'
                        | '='
                        | '＝'
                        | '?'
                        | '？'
                )
        })
    {
        return None;
    }
    let mut visible = compact.trim_end_matches(['?', '？']).to_string();
    let expression = if let Some((left, right)) = visible.split_once(['=', '＝']) {
        if !right.is_empty() && right != "?" && right != "？" {
            return None;
        }
        left.to_string()
    } else {
        visible.clone()
    };
    if !expression
        .chars()
        .any(|c| matches!(c, '+' | '-' | '*' | '×' | 'x' | 'X' | '/' | '÷'))
    {
        return None;
    }
    let normalized = expression.replace(['×', 'x', 'X'], "*").replace('÷', "/");
    let mut parser = ArithmeticParser::new(&normalized);
    let value = parser.expression()?;
    if parser.remaining().is_empty() && value.is_finite() {
        let result = if (value - value.round()).abs() < 1e-10 {
            format!("{:.0}", value)
        } else {
            let mut text = format!("{value:.10}");
            while text.ends_with('0') {
                text.pop();
            }
            text.trim_end_matches('.').to_string()
        };
        if !visible.ends_with(['=', '＝']) {
            visible.push('=');
        }
        Some(format!("{visible}{result}"))
    } else {
        None
    }
}

struct ArithmeticParser<'a> {
    source: &'a [u8],
    position: usize,
}

impl<'a> ArithmeticParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            position: 0,
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.source[self.position..]
    }

    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            match self.source.get(self.position).copied() {
                Some(b'+') => {
                    self.position += 1;
                    value += self.term()?;
                }
                Some(b'-') => {
                    self.position += 1;
                    value -= self.term()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut value = self.factor()?;
        loop {
            match self.source.get(self.position).copied() {
                Some(b'*') => {
                    self.position += 1;
                    value *= self.factor()?;
                }
                Some(b'/') => {
                    self.position += 1;
                    let divisor = self.factor()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value /= divisor;
                }
                _ => return Some(value),
            }
        }
    }

    fn factor(&mut self) -> Option<f64> {
        if self.source.get(self.position) == Some(&b'-') {
            self.position += 1;
            return Some(-self.factor()?);
        }
        if self.source.get(self.position) == Some(&b'(') {
            self.position += 1;
            let value = self.expression()?;
            if self.source.get(self.position) != Some(&b')') {
                return None;
            }
            self.position += 1;
            return Some(value);
        }
        let start = self.position;
        while self
            .source
            .get(self.position)
            .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
        {
            self.position += 1;
        }
        if start == self.position {
            return None;
        }
        std::str::from_utf8(&self.source[start..self.position])
            .ok()?
            .parse()
            .ok()
    }
}

/// Does the visible draft contain screen-oriented formatting that should be
/// rewritten semantically before it reaches physical paper?
fn paper_answer_needs_rewrite(full: &str) -> bool {
    let visible = full.split_once(SENTINEL).map(|p| p.0).unwrap_or(full);
    let lower = visible.to_ascii_lowercase();
    lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("www.")
        || visible.contains("](")
        || visible.contains("**")
        || visible.contains("```")
        || visible.contains("cite")
}

/// A rare second pass: rewrite only the not-yet-inked tail as coherent paper
/// prose. A clean prefix may already be on paper, so it is context only and
/// must never be repeated.
fn rewrite_paper_tail(
    agent: &ureq::Agent,
    base: &str,
    key: &str,
    model: &str,
    written_prefix: &str,
    draft_tail: &str,
) -> Result<String, String> {
    let instructions = "Rewrite only the remaining draft into the continuation that will be handwritten on physical paper. Preserve every fact, calculation, source name, and intended answer, but make it natural and concise. Text already written is context only: do not repeat or contradict it. Output only the rewritten continuation: no URL, Markdown, citation marker, reference list, search discussion, heading, or commentary. Use Traditional Chinese when the draft is Chinese.";
    let input = format!(
        "Text already written on paper:\n{}\n\nRemaining draft to rewrite:\n{}",
        written_prefix.trim(),
        draft_tail.trim(),
    );
    let body = format!(
        concat!(
            "{{\"model\":{},\"stream\":false,\"store\":false,",
            "\"max_output_tokens\":600,\"reasoning\":{{\"effort\":\"none\"}},",
            "\"instructions\":{},\"input\":{}}}"
        ),
        json_quote(model),
        json_quote(instructions),
        json_quote(&input),
    );
    let response = agent
        .post(&format!("{base}/responses"))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => format!(
                "http {code}: {}",
                r.into_string().unwrap_or_default().trim()
            ),
            other => other.to_string(),
        })?
        .into_string()
        .map_err(|e| e.to_string())?;
    let rewritten = extract_assistant_text(&response)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "paper editor returned no answer".to_string())?;
    Ok(rewritten.trim().to_string())
}

/// Trim and strip stray surrounding quotes from a reply fragment.
fn clean(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix('"').unwrap_or(t);
    let t = t.strip_suffix('"').unwrap_or(t);
    t.to_string()
}

/// Remove any ⟦…⟧ directive spans from inked prose, so a misbehaving model
/// that emits a directive mid/after prose never renders ⟦…⟧ as literal glyphs
/// in Tom's hand. (A directive that LEADS the reply is routed earlier.)
fn strip_directives(s: &str) -> String {
    if !s.contains(SHOW_OPEN) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find(SHOW_OPEN) {
        out.push_str(&rest[..open]);
        match rest[open..].find(SHOW_CLOSE) {
            Some(close) => rest = &rest[open + close + SHOW_CLOSE.len_utf8()..],
            None => {
                rest = ""; // unterminated: drop the tail
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// End of the LAST complete sentence in `text` after byte offset `from`:
/// sentence punctuation followed by whitespace or end-of-text. Returns the
/// offset just past the punctuation, or None if no sentence has completed.
/// Chunks shorter than a few characters are not worth an early delivery.
fn sentence_cut(text: &str, from: usize) -> Option<usize> {
    let tail = text.get(from..)?;
    let mut cut = None;
    for (i, c) in tail.char_indices() {
        if matches!(c, '。' | '！' | '？') {
            let end = i + c.len_utf8();
            // Do not commit while the terminator is merely the current end of
            // the live stream: a closing quote may arrive in the next delta.
            // When it is already present, keep it with the sentence.
            if let Some(next) = tail[end..].chars().next() {
                let quoted_end = if matches!(next, '”' | '’' | '」' | '』' | '》' | '〉') {
                    end + next.len_utf8()
                } else {
                    end
                };
                if quoted_end >= 4 {
                    cut = Some(from + quoted_end);
                }
            }
        } else if matches!(c, '.' | '!' | '?' | '…') {
            let end = i + c.len_utf8();
            // At the current end of a live stream, a period may still be a
            // decimal point whose following digit has not arrived yet. Wait
            // for actual whitespace; the final flush handles end-of-answer.
            if tail[end..].chars().next().is_some_and(char::is_whitespace) && end >= 4 {
                cut = Some(from + end);
            }
        }
    }
    cut
}

/// Extract a top-level string field's value (first match; unescaped).
fn json_str_field(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = s.find(&pat)? + pat.len();
    let rest = &s[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    match n {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        // \uXXXX — needed for accented replies (French, em-dash…).
                        'u' => {
                            let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                            if let Some(ch) =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                out.push(ch);
                            }
                        }
                        other => out.push(other),
                    }
                }
            }
            '"' => break,
            _ => out.push(c),
        }
    }
    Some(out)
}

/// Pull the assistant reply text out of an event line. The event carries a
/// `message` object with `"role":"assistant"` and `content:[{type:text,text:…}]`.
/// We only trust text that belongs to an assistant message (the user echo also
/// contains a "text" field, which we must NOT return).
fn extract_assistant_text(s: &str) -> Option<String> {
    // Require this line to be an assistant message.
    if !s.contains("\"role\":\"assistant\"") {
        return None;
    }
    // Collect every "text":"…" occurrence inside the FIRST assistant section
    // only. message_update lines carry the running text twice (in
    // assistantMessageEvent.partial AND a top-level message); reading past the
    // next role marker would double every streamed chunk.
    let role_pos = s.find("\"role\":\"assistant\"")?;
    let after = &s[role_pos + "\"role\":\"assistant\"".len()..];
    let tail = match after.find("\"role\":\"") {
        Some(p) => &after[..p],
        None => after,
    };
    let mut out = String::new();
    let mut idx = 0;
    let needle = "\"text\":\"";
    while let Some(rel) = tail[idx..].find(needle) {
        let start = idx + rel + needle.len();
        // Decode the JSON string starting at `start`.
        let mut chars = tail[start..].chars();
        let mut piece = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        piece.push(match n {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '"' => '"',
                            '\\' => '\\',
                            '/' => '/',
                            other => other,
                        });
                    }
                }
                '"' => break,
                _ => piece.push(c),
            }
        }
        out.push_str(&piece);
        // Advance past this occurrence.
        idx = start;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // RFC 8259 forbids raw controls in strings. Model transcripts can
            // carry tabs/CRs (the SSE + pi decoders un-escape \t \r \uXXXX),
            // and one such char stored in memory would poison every later
            // request's JSON. Escape the whole C0 range defensively.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_keeps_magicpaper_identity_and_direct_answers() {
        let prompt = system_prompt(true);
        assert!(prompt.contains("Your full and only name is MagicPaper"));
        assert!(prompt.contains("abbreviated MP"));
        assert!(prompt.contains("answer it immediately"));
        assert!(prompt.contains("什么是INTP"));
        assert!(prompt.contains("122+456=578"));
        assert!(prompt.contains("exactly the same recognized text"));
        assert!(prompt.contains("number digit by digit"));
        assert!(prompt.contains("MUST be searched"));
        assert!(prompt.contains("fresh answer suitable for a paper page"));
    }

    #[test]
    fn turn_context_includes_persistent_task_catalog() {
        let ctx = TurnContext {
            task_lines: vec!["1. every 5 minutes — 講一個黑暗冷笑話".into()],
            ..TurnContext::default()
        };
        let text = turn_text(&ctx);
        assert!(text.contains("Recurring task catalog:"));
        assert!(text.contains("講一個黑暗冷笑話"));
    }

    #[test]
    fn parser_routes_local_task_and_todo_lists() {
        let mut tasks = StreamParser::new(vec![]);
        assert_eq!(
            drain(tasks.advance("⟦tasks⟧\n⁂任务", true)),
            vec![Event::TaskList, Event::Transcript("任务".into())]
        );
        let mut todos = StreamParser::new(vec![]);
        assert_eq!(
            drain(todos.advance("⟦todos⟧\n⁂Todo", true)),
            vec![Event::TodoList, Event::Transcript("Todo".into())]
        );
        let mut fonts = StreamParser::new(vec![]);
        assert_eq!(
            drain(fonts.advance("⟦fonts⟧\n⁂字体", true)),
            vec![Event::FontList, Event::Transcript("字体".into())]
        );
        let mut history = StreamParser::new(vec![]);
        assert_eq!(
            drain(history.advance("⟦history⟧\n⁂历史", true)),
            vec![Event::HistoryList, Event::Transcript("历史".into())]
        );
    }

    #[test]
    fn paddle_multipart_contains_model_options_and_binary_png() {
        let png = b"\x89PNG\r\n\x1a\nbytes";
        let body = paddle_multipart(png, "PaddleOCR-VL-1.6", "test-boundary");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"model\""));
        assert!(text.contains("PaddleOCR-VL-1.6"));
        assert!(text.contains("name=\"optionalPayload\""));
        assert!(text.contains("useDocUnwarping"));
        assert!(body.windows(png.len()).any(|window| window == png));
        assert!(text.ends_with("--test-boundary--\r\n"));

        let v6_body = paddle_multipart(png, "PP-OCRv6", "test-boundary-v6");
        let v6 = String::from_utf8_lossy(&v6_body);
        assert!(v6.contains("PP-OCRv6"));
        assert!(v6.contains("useTextlineOrientation"));
        assert!(!v6.contains("useChartRecognition"));
    }

    #[test]
    fn paddle_json_helpers_extract_job_fields_and_markdown() {
        assert_eq!(
            json_str_field_loose(r#"{"data": {"jobId": "abc-123"}}"#, "jobId"),
            Some("abc-123".into())
        );
        let jsonl = concat!(
            r#"{"result":{"layoutParsingResults":[{"markdown":{"text":"任务 每五分钟提醒我喝水","images":{}}}]}}"#,
            "\n",
            r#"{"result":{"layoutParsingResults":[{"markdown":{"text":"TODO 买牛奶","images":{}}}]}}"#
        );
        assert_eq!(
            extract_paddle_markdown(jsonl),
            "任务 每五分钟提醒我喝水\nTODO 买牛奶"
        );
        assert_eq!(extract_paddle_text(jsonl), extract_paddle_markdown(jsonl));
    }

    #[test]
    fn paddle_json_helpers_extract_ppocr_v6_text() {
        let jsonl = concat!(
            r#"{"result":{"ocrResults":[{"prunedResult":{"rec_texts":["122+456=?","第二行\n含换行"],"rec_scores":[0.99,0.95]}}]}}"#,
            "\n",
            r#"{"result":{"ocrResults":[{"prunedResult":{"rec_texts":["任务 每五分钟提醒我喝水"]}}]}}"#
        );
        assert_eq!(
            extract_paddle_text(jsonl),
            "122+456=?\n第二行\n含换行\n任务 每五分钟提醒我喝水"
        );
        assert_eq!(extract_paddle_scores(jsonl), vec![0.99, 0.95]);
    }

    #[test]
    fn high_confidence_local_routes_choose_fast_commit() {
        let result = OcrResult {
            text: "字体".into(),
            min_confidence: Some(0.97),
        };
        assert!(result.is_fast_commit());
        assert_eq!(
            local_route("字体"),
            Some(LocalRoute::Event(Event::FontList))
        );
        assert_eq!(
            local_route("task"),
            Some(LocalRoute::Event(Event::TaskList))
        );
        assert_eq!(
            local_route("Todo"),
            Some(LocalRoute::Event(Event::TodoList))
        );
        assert_eq!(
            local_route("历史"),
            Some(LocalRoute::Event(Event::HistoryList))
        );
        assert_eq!(local_route("TODO 买牛奶"), Some(LocalRoute::Command));
        let shared = Arc::new(Mutex::new(Some(result)));
        let handle = RequestCancel::http(Arc::new(AtomicBool::new(false)), Some(shared));
        assert_eq!(handle.recommended_commit_ms(), Some(2200));
    }

    #[test]
    fn uncertain_ocr_uses_slow_commit() {
        let shared = Arc::new(Mutex::new(Some(OcrResult {
            text: "什么是INTP".into(),
            min_confidence: Some(0.73),
        })));
        let handle = RequestCancel::http(Arc::new(AtomicBool::new(false)), Some(shared));
        assert_eq!(handle.recommended_commit_ms(), Some(2600));
    }

    #[test]
    fn arithmetic_fast_path_preserves_the_written_equation() {
        assert_eq!(evaluate_arithmetic("122+456=?"), Some("122+456=578".into()));
        assert_eq!(
            evaluate_arithmetic("(12+8)×3？"),
            Some("(12+8)×3=60".into())
        );
        assert_eq!(evaluate_arithmetic("7÷2="), Some("7÷2=3.5".into()));
        assert_eq!(evaluate_arithmetic("什么是 1+1"), None);
    }

    #[test]
    fn external_ocr_turn_is_text_only_and_explicitly_untrusted() {
        let text = external_ocr_turn_text(&TurnContext::default(), "122+456=？");
        assert!(text.contains("<ocr_transcription>"));
        assert!(text.contains("122+456=？"));
        assert!(EXTERNAL_OCR_PROTOCOL.contains("untrusted evidence"));
        assert!(EXTERNAL_OCR_PROTOCOL.contains("must not claim to inspect stroke geometry"));
    }

    #[test]
    fn sse_delta_extraction() {
        let line = r#"{"choices":[{"delta":{"content":"Hello"},"index":0}]}"#;
        assert_eq!(sse_delta_content(line).as_deref(), Some("Hello"));
        // role-only delta (first SSE frame) has no content.
        let role = r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#;
        assert_eq!(sse_delta_content(role), None);
    }

    #[test]
    fn sse_decodes_unicode_and_escapes() {
        // OpenAI escapes accents and em-dashes; the diary answers in French.
        let line = r#"{"choices":[{"delta":{"content":"Déjà vu — oui"}}]}"#;
        assert_eq!(sse_delta_content(line).as_deref(), Some("Déjà vu — oui"));
        let nl = r#"{"choices":[{"delta":{"content":"line\nbreak"}}]}"#;
        assert_eq!(sse_delta_content(nl).as_deref(), Some("line\nbreak"));
    }

    #[test]
    fn responses_sse_delta_extraction() {
        let delta = r#"{"type":"response.output_text.delta","delta":"六韜"}"#;
        assert_eq!(responses_delta_content(delta).as_deref(), Some("六韜"));
        let search = r#"{"type":"response.web_search_call.completed"}"#;
        assert_eq!(responses_delta_content(search), None);
    }

    #[test]
    fn paper_editor_detects_screen_formatting_only_in_visible_reply() {
        assert!(paper_answer_needs_rewrite("See **this**.\n⁂原文"));
        assert!(paper_answer_needs_rewrite(
            "答案見 [古籍](https://example.test)。\n⁂原文"
        ));
        assert!(!paper_answer_needs_rewrite(
            "出自《六韜·文韜·文師》。\n⁂主人寫了https://example.test"
        ));
    }

    #[test]
    fn base64_matches_known_vector() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn clean_strips_wrapping_quotes() {
        assert_eq!(clean("  \"hello\"  "), "hello");
        assert_eq!(clean("plain"), "plain");
    }

    fn drain(events: Vec<Result<Event, String>>) -> Vec<Event> {
        events.into_iter().map(|e| e.unwrap()).collect()
    }

    #[test]
    fn parser_streams_prose_then_transcript() {
        let mut p = StreamParser::new(vec![]);
        assert!(p.advance("Hello", false).is_empty());
        let ev = drain(p.advance("Hello. Who wri", false));
        assert_eq!(ev, vec![Event::Ink("Hello.".into())]);
        let full = "Hello. Who writes to me? \u{2042} it rained all night";
        let ev = drain(p.advance(full, true));
        assert_eq!(
            ev,
            vec![
                Event::Ink("Who writes to me?".into()),
                Event::Transcript("it rained all night".into())
            ]
        );
    }

    #[test]
    fn parser_streams_complete_chinese_sentence_before_response_ends() {
        let mut p = StreamParser::new(vec![]);
        let ev = drain(p.advance("第一句完成。第二句還", false));
        assert_eq!(ev, vec![Event::Ink("第一句完成。".into())]);
        let ev = drain(p.advance("第一句完成。第二句還在寫", true));
        assert_eq!(ev, vec![Event::Ink("第二句還在寫".into())]);
    }

    #[test]
    fn parser_flushes_equation_as_soon_as_hidden_transcript_starts() {
        let mut p = StreamParser::new(vec![]);
        let ev = drain(p.advance("240×0.85＝204元\n⁂", false));
        assert_eq!(ev, vec![Event::Ink("240×0.85＝204元".into())]);
        let ev = drain(p.advance("240×0.85＝204元\n⁂原文", true));
        assert_eq!(ev, vec![Event::Transcript("原文".into())]);
    }

    #[test]
    fn parser_does_not_split_streaming_decimal_points() {
        let mut p = StreamParser::new(vec![]);
        assert!(p.advance("240×0.", false).is_empty());
        assert!(p.advance("240×0.85＝204.", false).is_empty());
        let ev = drain(p.advance("240×0.85＝204.0元\n⁂原文", false));
        assert_eq!(ev, vec![Event::Ink("240×0.85＝204.0元".into())]);
    }

    #[test]
    fn parser_waits_for_closing_quote_after_chinese_period() {
        let mut p = StreamParser::new(vec![]);
        assert!(p.advance("出處是：「原文。", false).is_empty());
        let ev = drain(p.advance("出處是：「原文。」下一句", false));
        assert_eq!(ev, vec![Event::Ink("出處是：「原文。」".into())]);
    }

    #[test]
    fn request_cancel_sets_shared_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let handle = RequestCancel::http(Arc::clone(&flag), None);
        handle.cancel();
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn parser_routes_show_directive() {
        let mut p = StreamParser::new(vec![900, 800, 700]);
        // Directive still streaming in: no decision yet.
        assert!(p.advance("\u{27e6}sho", false).is_empty());
        let ev = drain(p.advance("\u{27e6}show:2\u{27e7}", false));
        assert_eq!(ev, vec![Event::Show(800)]);
        let full = "\u{27e6}show:2\u{27e7}\n\u{2042} show me the garden page";
        let ev = drain(p.advance(full, true));
        assert_eq!(
            ev,
            vec![Event::Transcript("show me the garden page".into())]
        );
    }

    #[test]
    fn parser_show_tolerates_spacing_and_case() {
        let mut p = StreamParser::new(vec![42]);
        let ev = drain(p.advance("  \u{27e6}Show: 1\u{27e7}", true));
        assert!(ev.contains(&Event::Show(42)), "{ev:?}");
    }

    #[test]
    fn parser_show_out_of_range_is_error() {
        let mut p = StreamParser::new(vec![42]);
        let ev = p.advance("\u{27e6}show:7\u{27e7}", true);
        assert!(ev[0].is_err());
    }

    #[test]
    fn parser_empty_reply_is_error() {
        let mut p = StreamParser::new(vec![]);
        let ev = p.advance("", true);
        assert!(ev[0].is_err());
    }

    #[test]
    fn parser_without_sentinel_still_flushes() {
        // Memory off (or model forgot the postscript): plain prose still works.
        let mut p = StreamParser::new(vec![]);
        let ev = drain(p.advance("A reply without postscript", true));
        assert_eq!(ev, vec![Event::Ink("A reply without postscript".into())]);
    }

    #[test]
    fn parser_leading_directive_conjures_and_takes_the_whole_body() {
        let mut p = StreamParser::new(vec![900, 800]);
        let full = "\u{27e6}show:2\u{27e7}\n\u{2042} show me the rain";
        let ev = drain(p.advance(full, true));
        assert_eq!(
            ev,
            vec![
                Event::Show(800),
                Event::Transcript("show me the rain".into())
            ]
        );
    }

    #[test]
    fn parser_directive_after_prose_is_stripped_not_inked() {
        // A misbehaving model prefaces the directive with prose. We don't
        // honor it (that would need un-inking), but we must NOT render the
        // ⟦…⟧ as literal glyphs — strip it from the inked text.
        let mut p = StreamParser::new(vec![900, 800]);
        let full = "Of course, let me show you. \u{27e6}show:2\u{27e7}\n\u{2042} show me the rain";
        let ev = drain(p.advance(full, true));
        assert_eq!(
            ev,
            vec![
                Event::Ink("Of course, let me show you.".into()),
                Event::Transcript("show me the rain".into())
            ]
        );
        // The show glyphs never reached the writer.
        assert!(!ev
            .iter()
            .any(|e| matches!(e, Event::Ink(s) if s.contains('\u{27e6}'))));
    }

    #[test]
    fn strip_directives_removes_spans() {
        assert_eq!(strip_directives("a \u{27e6}show:1\u{27e7} b"), "a b");
        assert_eq!(strip_directives("plain text"), "plain text");
        assert_eq!(strip_directives("tail \u{27e6}show:2"), "tail");
    }

    #[test]
    fn json_quote_escapes_control_chars() {
        // A tabbed, multiline transcript must not produce raw C0 bytes.
        let q = json_quote("a\tb\r\nc\u{0007}d");
        assert_eq!(q, "\"a\\tb\\r\\nc\\u0007d\"");
        assert!(!q.chars().any(|c| (c as u32) < 0x20));
    }
}
