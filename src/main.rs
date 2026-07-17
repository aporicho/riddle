//! MagicPaper (MP) — living paper for the reMarkable Paper Pro.
//!
//! Write on the page with the pen. After a pause the diary drinks your ink,
//! and an answer writes itself onto the page in a flowing hand, then fades.
//!
//! Two display backends (picked at runtime): windowed via qtfb/AppLoad when
//! QTFB_KEY is set, or full takeover via the vendor engine (quill) when
//! built with --features takeover and launched with xochitl stopped.

mod display;
mod fb;
mod font_panel;
mod fonts;
mod help;
mod ink;
mod memory;
mod oracle;
mod pen;
mod power;
mod qtfb;
mod script;
mod surface;
mod task_panel;
mod tasks;
mod todos;
mod touch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fb::{screen_h, screen_w, BBox};
use oracle::Event;
use surface::{Surface, BLACK, FADED, WHITE};

const PNG_PATH: &str = "/tmp/riddle-page.png";

/// Begin reading a tentative page while its ink is still visible. The page is
/// not committed until the adaptive fast/slow deadline; more pen input
/// invalidates this request.
const IDLE_PREASK: Duration = Duration::from_millis(1000);
const IDLE_COMMIT_FAST: Duration = Duration::from_millis(2200);
const IDLE_COMMIT_SLOW: Duration = Duration::from_millis(2600);
const DRINK_STAGES: u32 = 14;
const DRINK_STAGE_DELAY: Duration = Duration::from_millis(50);
const HISTORY_VISIBLE: usize = 9;
/// Only failed due-task delivery is polled. Normal delivery is scheduled at
/// the exact nearest due time and an empty/paused list schedules nothing.
const HEARTBEAT_RETRY_DEFAULT: Duration = Duration::from_secs(30);
/// How long the diary waits on a silent oracle before giving up on the turn.
/// Generous: thinking models can lead with a long silence.
const ORACLE_PATIENCE: Duration = Duration::from_secs(120);
const REPLY_PX: f32 = 96.0;
const MARGIN_X: i32 = 120;

const USAGE: &str = "\
MagicPaper (MP) — your living magical paper

usage:
  riddle                      open the diary (windowed when AppLoad sets
                              QTFB_KEY, otherwise takeover via libquill)
  riddle --oracle-test [PNG]  run one oracle turn against PNG (default
                              /tmp/riddle-page.png) and print the streamed
                              reply; verifies key + endpoint + model
  riddle --ocr-test PNG       send one PNG only to the configured PaddleOCR
                              service and print its recognized text
  riddle --power-launcher     watch for three quick power-button presses and
                              launch the standalone diary
  riddle --version            print the version

standalone configuration lives in /home/root/.config/riddle/oracle.env.
";

type OracleRx = mpsc::Receiver<Result<Event, String>>;

struct SpeculativeRequest {
    rx: OracleRx,
    cancel: oracle::RequestCancel,
}

fn cancel_speculative(pending: &mut Option<SpeculativeRequest>, reason: &str) {
    if let Some(request) = pending.take() {
        request.cancel.cancel();
        eprintln!("riddle: speculative oracle discarded ({reason})");
    }
}

fn idle_commit_delay(pending: &Option<SpeculativeRequest>) -> Duration {
    match pending
        .as_ref()
        .and_then(|request| request.cancel.recommended_commit_ms())
    {
        Some(2200) => IDLE_COMMIT_FAST,
        _ => IDLE_COMMIT_SLOW,
    }
}

enum State {
    Listening {
        last_pen: Option<Instant>,
    },
    Drinking {
        stage: u32,
        next: Instant,
        region: BBox,
        rx: OracleRx,
    },
    Thinking {
        rx: OracleRx,
        since: Instant,
    },
    Replying {
        plan: WritePlan,
        next: Instant,
        rx: Option<OracleRx>,
    },
    Lingering {
        until: Instant,
        region: BBox,
    },
    FadingReply {
        stage: u32,
        next: Instant,
        region: BBox,
    },
    /// The guide panel. `panel: None` = dismissed, waiting for pen-up so the
    /// dismissing touch doesn't leave a mark on the page.
    Help {
        panel: Option<help::Help>,
        until: Instant,
    },
    /// A remembered page rising through the paper: date, the writer's own
    /// past ink, Tom's old reply — all in faded ink. `saved` is today's page.
    Conjuring {
        plan: ConjurePlan,
        next: Instant,
        saved: Vec<u8>,
    },
    /// The conjured memory rests on the page. Pen contact (or time) dissolves
    /// it and today's page returns. `saved: None` = dismissed, waiting pen-up.
    MemoryShown {
        saved: Option<Vec<u8>>,
        until: Instant,
        region: BBox,
    },
    /// A device-local numbered task page. Horizontal pen strokes delete a
    /// row; a small tap outside all rows restores the underlying page.
    TaskList {
        panel: task_panel::PaperList,
    },
    /// The persistent, non-scheduled TODO page uses the same paper gestures.
    TodoList {
        panel: task_panel::PaperList,
    },
    /// A local three-font picker. Row taps switch immediately and redraw the
    /// preview; a tap on blank paper restores the page underneath.
    FontList {
        panel: font_panel::FontPanel,
    },
    /// The newest local dialogue pages; striking a row forgets both its text
    /// and replay strokes.
    HistoryList {
        panel: task_panel::PaperList,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnKind {
    User,
    Heartbeat,
}

/// A memory being rewritten onto the page: pre-positioned strokes with their
/// original radii, drawn in faded ink.
struct ConjurePlan {
    strokes: Vec<Vec<(i32, i32, i32)>>,
    stroke_i: usize,
    point_i: usize,
    region: BBox,
}

struct WritePlan {
    strokes: Vec<Vec<(i32, i32)>>,
    stroke_i: usize,
    point_i: usize,
    region: BBox,
    /// Where the next streamed chunk's first line starts.
    next_y: i32,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // Diagnostic: run one oracle turn and print the streamed chunks.
        // Lets you verify your endpoint + key + model before ever launching
        // the diary. No display needed.
        Some("--oracle-test") => {
            let png = args.get(2).map(String::as_str).unwrap_or(PNG_PATH);
            std::process::exit(oracle_test(png));
        }
        Some("--ocr-test") => {
            let Some(png) = args.get(2) else {
                eprintln!("riddle: --ocr-test needs a PNG path");
                std::process::exit(2);
            };
            match oracle::paddle_ocr_test(png) {
                Ok(text) => println!("{text}"),
                Err(error) => {
                    eprintln!("OCR test failed: {error}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--power-launcher") => {
            if let Err(e) = power::launcher_loop() {
                eprintln!("riddle: power launcher fatal: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("--version" | "-V") => {
            println!("MagicPaper (MP) {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            return;
        }
        Some(flag) if flag.starts_with('-') => {
            eprintln!("riddle: unknown flag {flag}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
        _ => {}
    }
    if let Err(e) = run() {
        eprintln!("riddle: fatal: {e}");
        std::process::exit(1);
    }
}

fn oracle_test(png: &str) -> i32 {
    let store = memory::MemoryStore::open();
    let task_store = tasks::TaskStore::open();
    let todo_store = todos::TodoStore::open();
    let o = match oracle::Oracle::spawn(
        store.is_some() || task_store.is_some() || todo_store.is_some(),
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("oracle spawn failed: {e}");
            return 1;
        }
    };
    let ctx = build_ctx(&store, &task_store, &todo_store);
    let (tx, rx) = mpsc::channel();
    let t0 = Instant::now();
    o.ask(png, &ctx, tx);
    let mut got = String::new();
    loop {
        match rx.recv() {
            Ok(Ok(Event::Ink(chunk))) => {
                if got.is_empty() {
                    eprintln!("first chunk +{}ms", t0.elapsed().as_millis());
                }
                print!("{chunk} ");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                got.push_str(&chunk);
            }
            Ok(Ok(Event::Show(id))) => {
                println!("[would conjure memory {id} — {}]", memory::spoken_date(id));
                got.push_str("(show)");
            }
            Ok(Ok(Event::TaskList)) => {
                println!("[would open recurring-task list]");
                got.push_str("(tasks)");
            }
            Ok(Ok(Event::TodoList)) => {
                println!("[would open TODO list]");
                got.push_str("(todos)");
            }
            Ok(Ok(Event::FontList)) => {
                println!("[would open font list]");
                got.push_str("(fonts)");
            }
            Ok(Ok(Event::HistoryList)) => {
                println!("[would open history list]");
                got.push_str("(history)");
            }
            Ok(Ok(Event::LocalCommand(command))) => {
                println!("[would apply local command: {command}]");
                got.push_str("(local-command)");
            }
            Ok(Ok(Event::Transcript(t))) => eprintln!("\n[transcript] {t}"),
            Ok(Err(e)) => {
                eprintln!("\noracle error: {e}");
                return 1;
            }
            Err(_) => break, // disconnected = reply complete
        }
    }
    println!(
        "\n--- reply complete ({}ms, {} chars) ---",
        t0.elapsed().as_millis(),
        got.len()
    );
    if got.trim().is_empty() {
        1
    } else {
        0
    }
}

/// What the diary sends alongside the page: its memory of recent turns and
/// the catalog the oracle picks conjured pages from. Empty when memory is off.
fn build_ctx(
    store: &Option<memory::MemoryStore>,
    task_store: &Option<tasks::TaskStore>,
    todo_store: &Option<todos::TodoStore>,
) -> oracle::TurnContext {
    let turns: usize = std::env::var("RIDDLE_MEMORY_TURNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let (history, catalog_lines, catalog_ids) = match store {
        Some(s) => {
            let (lines, ids) = s.catalog(40);
            (s.recent_dialogue(turns), lines, ids)
        }
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let task_lines = task_store
        .as_ref()
        .map(|s| s.catalog_lines())
        .unwrap_or_default();
    let todo_lines = todo_store
        .as_ref()
        .map(|s| s.catalog_lines())
        .unwrap_or_default();
    oracle::TurnContext {
        history,
        catalog_lines,
        catalog_ids,
        task_lines,
        todo_lines,
    }
}

fn run() -> std::io::Result<()> {
    let mut font = fonts::FontBook::open()?;

    let (disp, mut surf) = display::Display::open()?;
    fb::init_screen(surf.w, surf.h);
    let takeover = matches!(disp, display::Display::Quill);
    eprintln!(
        "riddle: display {} ({}x{} stride {})",
        if takeover { "quill/takeover" } else { "qtfb" },
        surf.w,
        surf.h,
        surf.stride
    );

    let mut pen_dev = match pen::PenDevice::open() {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("riddle: raw pen unavailable ({e}), falling back to qtfb pen events");
            None
        }
    };
    // Takeover mode: touch is ours too; 5-finger tap = quit.
    let mut touch_dev = if takeover {
        touch::TouchDevice::open().ok()
    } else {
        None
    };
    // Takeover mode: the power button is ours too (sleep page + suspend).
    let mut power_dev = if takeover {
        power::PowerButton::open()
            .map_err(|e| eprintln!("riddle: no power button ({e})"))
            .ok()
    } else {
        None
    };
    // A single click sleeps after the multi-click window; three quick clicks
    // leave the diary. The waking press is ignored briefly after resume.
    let mut power_grace = Instant::now();
    let mut power_clicks = power::ClickTracker::new();

    let sigterm = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&sigterm))?;

    // Blank page.
    surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
    disp.update_all(surf.w, surf.h);

    // MagicPaper's page memory and its separate persistent task list.
    let mut store = memory::MemoryStore::open();
    if let Some(ref s) = store {
        eprintln!("riddle: memory holds {} pages", s.entries.len());
    }
    let mut task_store = tasks::TaskStore::open();
    if let Some(ref s) = task_store {
        eprintln!(
            "magic-paper: task list holds {} recurring commands",
            s.entries.len()
        );
    }
    let mut todo_store = todos::TodoStore::open();
    if let Some(ref s) = todo_store {
        eprintln!("magic-paper: TODO list holds {} entries", s.entries.len());
    }

    // Warm the oracle now: pi loads Node + extensions + codex auth ONCE here,
    // while you're still picking up the pen, so replies pay only model latency.
    let oracle = match oracle::Oracle::spawn(
        store.is_some() || task_store.is_some() || todo_store.is_some(),
    ) {
        Ok(o) => {
            eprintln!("riddle: oracle ready");
            Some(o)
        }
        Err(e) => {
            eprintln!("riddle: oracle spawn failed: {e}");
            None
        }
    };

    let mut user_ink = ink::Ink::new();
    let mut state = State::Listening { last_pen: None };
    let mut pen_down = false;
    let mut speculative: Option<SpeculativeRequest> = None;
    let mut speculative_attempted = false;
    // The turn being remembered: strokes captured at commit, transcript and
    // reply accumulated as they stream, stored when the turn completes.
    let mut turn_id: u64 = 0;
    let mut turn_strokes: memory::Strokes = Vec::new();
    let mut turn_reply = String::new();
    let mut turn_transcript: Option<String> = None;
    let mut turn_failed = false;
    let mut turn_kind = TurnKind::User;
    let mut turn_task_ids: Vec<u64> = Vec::new();
    let mut next_heartbeat = heartbeat_deadline(&task_store);
    // Raw stylus contact, tracked in every state (the guide dismisses on it).
    // `stylus_on` is the level; `stylus_tapped` latches any contact seen this
    // loop iteration, so a tap that starts AND ends within one drain still
    // registers.
    let mut stylus_on = false;
    let mut stylus_tapped = false;
    let mut ink_dirty = BBox::empty();
    let mut last_flush = Instant::now();
    // Takeover swaps are cheap and synchronous; qtfb needs coalescing.
    let flush_every = if takeover {
        Duration::from_millis(8)
    } else {
        Duration::from_millis(35)
    };

    eprintln!("riddle: the diary is open");

    loop {
        if sigterm.load(Ordering::Relaxed) {
            break;
        }
        if let Some(ref mut t) = touch_dev {
            if t.drain_check_quit() {
                eprintln!("riddle: 5-finger quit");
                break;
            }
        }

        // ---- power button: single-click sleep; triple-click exit ----
        if let Some(ref mut p) = power_dev {
            let now = Instant::now();
            let presses = p.drain_press_count();
            let action = if now >= power_grace {
                power_clicks.push(presses, now)
            } else {
                if presses > 0 {
                    power_clicks.clear();
                }
                power::ClickAction::None
            };
            if action == power::ClickAction::Triple {
                eprintln!("riddle: triple power quit");
                p.wait_for_release(Duration::from_millis(700));
                break;
            }
            if action == power::ClickAction::Single {
                eprintln!("riddle: sleeping (power button)");
                let saved = help::show_sleep(&mut surf, &font);
                disp.full_refresh(surf.w, surf.h);
                // Let the flashing refresh finish before the panel loses power.
                std::thread::sleep(Duration::from_millis(800));
                // Suspend, and confirm via the kernel's success counter. The
                // EPD regulator refuses to sleep while its post-update vpdd
                // timer (≤30s) runs — the whole suspend aborts with "Some
                // devices failed to suspend" — so retry until it sticks.
                let count0 = power::suspend_count();
                let mut attempts = 0;
                'sleeping: loop {
                    if p.grabbed {
                        let _ = std::process::Command::new("systemctl")
                            .arg("suspend")
                            .status();
                    }
                    attempts += 1;
                    let t0 = Instant::now();
                    while t0.elapsed() < Duration::from_secs(6) {
                        std::thread::sleep(Duration::from_millis(400));
                        if power::suspend_count() > count0 {
                            break 'sleeping;
                        }
                    }
                    if attempts >= 8 {
                        eprintln!(
                            "riddle: suspend never happened ({attempts} tries); waking the page"
                        );
                        break;
                    }
                    eprintln!("riddle: suspend aborted (EPD discharge timer), retrying");
                }
                eprintln!("riddle: waking");
                help::restore_sleep(&mut surf, &saved);
                disp.full_refresh(surf.w, surf.h);
                power::wifi_heal();
                // Discard input that queued while asleep — stale pen events
                // would otherwise replay as phantom ink on the restored page.
                if let Some(ref mut pd) = pen_dev {
                    let _ = pd.drain();
                }
                if let Some(ref mut td) = touch_dev {
                    let _ = td.drain_check_quit();
                }
                p.drain_pressed();
                power_clicks.clear();
                power_grace = Instant::now() + Duration::from_secs(3);
                // Recalculate from wall-clock time: tasks may have become due
                // while the tablet slept.
                next_heartbeat = heartbeat_deadline(&task_store);
            }
        }

        // ---- raw pen (preferred path) ----
        if let Some(ref mut pdev) = pen_dev {
            for s in pdev.drain() {
                let writing = s.touching && s.pressure > 40;
                stylus_on = writing;
                stylus_tapped |= writing;
                if !writing {
                    if matches!(
                        &state,
                        State::TaskList { .. }
                            | State::TodoList { .. }
                            | State::HistoryList { .. }
                            | State::FontList { .. }
                    ) {
                        pen_down = false;
                        finish_paper_list_stroke(
                            &mut state,
                            &mut store,
                            &mut task_store,
                            &mut todo_store,
                            &mut next_heartbeat,
                            &mut surf,
                            &mut font,
                            &disp,
                        );
                        continue;
                    }
                    if pen_down {
                        pen_down = false;
                        user_ink.pen_up();
                        if let State::Listening { ref mut last_pen } = state {
                            *last_pen = Some(Instant::now());
                        }
                    }
                    continue;
                }
                match state {
                    State::Listening { ref mut last_pen } => {
                        cancel_speculative(&mut speculative, "writing resumed");
                        speculative_attempted = false;
                        pen_down = true;
                        let d = match s.tool {
                            pen::Tool::Pen => {
                                let r = 2 + s.pressure * 3 / pen::MAX_PRESSURE;
                                user_ink.pen_point(&mut surf, s.x, s.y, r)
                            }
                            pen::Tool::Eraser => user_ink.erase_point(&mut surf, s.x, s.y, 22),
                        };
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                        *last_pen = Some(Instant::now());
                    }
                    State::Lingering { region, .. } | State::FadingReply { region, .. } => {
                        let (x, y, w, h) = region.rect();
                        surf.fill_rect(x as usize, y as usize, w as usize, h as usize, WHITE);
                        disp.update(x, y, w, h, true);
                        pen_down = true;
                        let d = match s.tool {
                            pen::Tool::Pen => {
                                let r = 2 + s.pressure * 3 / pen::MAX_PRESSURE;
                                user_ink.pen_point(&mut surf, s.x, s.y, r)
                            }
                            pen::Tool::Eraser => user_ink.erase_point(&mut surf, s.x, s.y, 22),
                        };
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                        state = State::Listening {
                            last_pen: Some(Instant::now()),
                        };
                    }
                    State::TaskList { ref mut panel }
                    | State::TodoList { ref mut panel }
                    | State::HistoryList { ref mut panel } => {
                        pen_down = true;
                        let d = panel.pen_point(&mut surf, s.x, s.y);
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                    }
                    State::FontList { ref mut panel } => {
                        pen_down = true;
                        let d = panel.pen_point(&mut surf, s.x, s.y);
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- window-system events (qtfb close detection + pen fallback) ----
        let events = match disp.pump() {
            Ok(v) => v,
            Err(_) => break, // qtfb window closed
        };
        for ev in events {
            if pen_dev.is_some() {
                continue;
            }
            match ev.input_type {
                qtfb::INPUT_PEN_PRESS | qtfb::INPUT_PEN_UPDATE => {
                    stylus_on = true;
                    stylus_tapped = true;
                    if let State::TaskList { ref mut panel }
                    | State::TodoList { ref mut panel }
                    | State::HistoryList { ref mut panel } = state
                    {
                        pen_down = true;
                        let d = panel.pen_point(&mut surf, ev.x, ev.y);
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                    } else if let State::FontList { ref mut panel } = state {
                        pen_down = true;
                        let d = panel.pen_point(&mut surf, ev.x, ev.y);
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                    } else if let State::Listening { ref mut last_pen } = state {
                        cancel_speculative(&mut speculative, "writing resumed");
                        speculative_attempted = false;
                        pen_down = true;
                        let r = 2 + ev.d.clamp(0, 100) / 45;
                        let d = user_ink.pen_point(&mut surf, ev.x, ev.y, r);
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                        *last_pen = Some(Instant::now());
                    } else if let State::Lingering { region, .. }
                    | State::FadingReply { region, .. } = state
                    {
                        let (x, y, w, h) = region.rect();
                        surf.fill_rect(x as usize, y as usize, w as usize, h as usize, WHITE);
                        disp.update(x, y, w, h, true);
                        pen_down = true;
                        let r = 2 + ev.d.clamp(0, 100) / 45;
                        let d = user_ink.pen_point(&mut surf, ev.x, ev.y, r);
                        if !d.is_empty() {
                            ink_dirty.add(d.x0, d.y0, 0);
                            ink_dirty.add(d.x1, d.y1, 0);
                        }
                        state = State::Listening {
                            last_pen: Some(Instant::now()),
                        };
                    }
                }
                qtfb::INPUT_PEN_RELEASE => {
                    stylus_on = false;
                    if matches!(
                        &state,
                        State::TaskList { .. }
                            | State::TodoList { .. }
                            | State::HistoryList { .. }
                            | State::FontList { .. }
                    ) {
                        pen_down = false;
                        finish_paper_list_stroke(
                            &mut state,
                            &mut store,
                            &mut task_store,
                            &mut todo_store,
                            &mut next_heartbeat,
                            &mut surf,
                            &mut font,
                            &disp,
                        );
                        continue;
                    }
                    if pen_down {
                        pen_down = false;
                        user_ink.pen_up();
                        if let State::Listening { ref mut last_pen } = state {
                            *last_pen = Some(Instant::now());
                        }
                    }
                }
                _ => {}
            }
        }

        // ---- coalesced ink flush ----
        if !ink_dirty.is_empty() && last_flush.elapsed() >= flush_every {
            let (x, y, w, h) = ink_dirty.rect();
            disp.update(x, y, w, h, true);
            ink_dirty = BBox::empty();
            last_flush = Instant::now();
        }

        // ---- state machine ----
        state = match state {
            State::Listening { last_pen } => match last_pen {
                Some(t)
                    if !pen_down
                        && !stylus_tapped
                        && t.elapsed() >= idle_commit_delay(&speculative)
                        && !user_ink.is_empty() =>
                {
                    speculative_attempted = false;
                    if region_all_white(&surf, user_ink.bbox) {
                        // Everything was erased before the pause: nothing to
                        // commit (and no phantom "?" from erased strokes).
                        cancel_speculative(&mut speculative, "page was erased");
                        user_ink.clear();
                        State::Listening { last_pen: None }
                    } else if help::looks_like_question_mark(user_ink.stroke_list()) {
                        // Absorb the "?" and open the guide instead of asking.
                        cancel_speculative(&mut speculative, "guide gesture");
                        let (qx, qy, qw, qh) = user_ink.bbox.rect();
                        surf.fill_rect(qx as usize, qy as usize, qw as usize, qh as usize, WHITE);
                        disp.update(qx, qy, qw, qh, false);
                        user_ink.clear();
                        let panel = help::show(&mut surf, &font, takeover);
                        let (px, py, pw, ph) = panel.region.rect();
                        disp.update(px, py, pw, ph, false);
                        eprintln!("riddle: guide shown");
                        State::Help {
                            panel: Some(panel),
                            until: Instant::now() + Duration::from_secs(45),
                        }
                    } else if oracle.is_none() {
                        // No spirit at all: don't eat ink that nothing will
                        // answer — leave the writing and put the reason below.
                        cancel_speculative(&mut speculative, "oracle unavailable");
                        let y = (user_ink.bbox.y1 + 90).min(screen_h() as i32 - 400);
                        let plan = plan_reply(&font, &oracle_excuse("no oracle"), Some(y));
                        State::Replying {
                            plan,
                            next: Instant::now(),
                            rx: None,
                        }
                    } else {
                        // Remember this page: strokes now (they're cleared
                        // after the drink), transcript/reply as they stream.
                        turn_id = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        turn_strokes = user_ink.stroke_list().to_vec();
                        turn_reply.clear();
                        turn_transcript = None;
                        turn_failed = false;
                        turn_kind = TurnKind::User;
                        turn_task_ids.clear();
                        let rx = if let Some(request) = speculative.take() {
                            eprintln!(
                                "riddle: committing page with speculative request already running"
                            );
                            request.rx
                        } else {
                            if let Err(e) = user_ink.to_png(&surf, PNG_PATH) {
                                eprintln!("riddle: rasterize failed: {e}");
                            }
                            let (tx, rx) = mpsc::channel();
                            if let Some(ref o) = oracle {
                                let _ = o.ask(
                                    PNG_PATH,
                                    &build_ctx(&store, &task_store, &todo_store),
                                    tx,
                                );
                            }
                            // The backend reads the page synchronously before
                            // ask() returns, so the PNG is no longer needed.
                            if std::env::var_os("RIDDLE_KEEP_PAGE").is_none() {
                                let _ = std::fs::remove_file(PNG_PATH);
                            }
                            rx
                        };
                        let region = user_ink.bbox;
                        State::Drinking {
                            stage: 0,
                            next: Instant::now(),
                            region,
                            rx,
                        }
                    }
                }
                Some(t)
                    if !pen_down
                        && !stylus_tapped
                        && t.elapsed() >= IDLE_PREASK
                        && !user_ink.is_empty()
                        && !speculative_attempted =>
                {
                    speculative_attempted = true;
                    if !region_all_white(&surf, user_ink.bbox)
                        && !help::looks_like_question_mark(user_ink.stroke_list())
                    {
                        if let Some(ref o) = oracle {
                            if o.supports_speculative() {
                                match user_ink.to_png(&surf, PNG_PATH) {
                                    Ok(()) => {
                                        let (tx, rx) = mpsc::channel();
                                        let cancel = o.ask(
                                            PNG_PATH,
                                            &build_ctx(&store, &task_store, &todo_store),
                                            tx,
                                        );
                                        speculative = Some(SpeculativeRequest { rx, cancel });
                                        eprintln!(
                                            "riddle: speculative oracle started after {}ms idle",
                                            IDLE_PREASK.as_millis()
                                        );
                                    }
                                    Err(e) => eprintln!(
                                        "riddle: speculative rasterize failed; will retry at commit: {e}"
                                    ),
                                }
                                if std::env::var_os("RIDDLE_KEEP_PAGE").is_none() {
                                    let _ = std::fs::remove_file(PNG_PATH);
                                }
                            }
                        }
                    }
                    State::Listening { last_pen }
                }
                _ if !pen_down
                    && !stylus_tapped
                    && user_ink.is_empty()
                    && next_heartbeat.is_some_and(|deadline| Instant::now() >= deadline) =>
                {
                    let now = unix_now();
                    let due = task_store.as_ref().map(|s| s.due(now)).unwrap_or_default();
                    if due.is_empty() {
                        // A task was paused/deleted or its wall-clock deadline
                        // moved. Recompute without touching the oracle.
                        next_heartbeat = heartbeat_deadline(&task_store);
                        State::Listening { last_pen }
                    } else if let Some(ref o) = oracle {
                        next_heartbeat = Some(Instant::now() + heartbeat_retry_interval());
                        turn_id = 0;
                        turn_strokes.clear();
                        turn_reply.clear();
                        turn_transcript = None;
                        turn_failed = false;
                        turn_kind = TurnKind::Heartbeat;
                        turn_task_ids = due.iter().map(|t| t.id).collect();
                        let prompt = tasks::heartbeat_prompt(&due);
                        let (tx, rx) = mpsc::channel();
                        o.ask_text(&prompt, &build_ctx(&store, &task_store, &todo_store), tx);
                        eprintln!(
                            "magic-paper: heartbeat — executing {} due task(s)",
                            turn_task_ids.len()
                        );
                        State::Thinking {
                            rx,
                            since: Instant::now(),
                        }
                    } else {
                        next_heartbeat = Some(Instant::now() + heartbeat_retry_interval());
                        eprintln!("magic-paper: heartbeat postponed — oracle unavailable");
                        State::Listening { last_pen }
                    }
                }
                _ => State::Listening { last_pen },
            },

            State::Drinking {
                stage,
                next,
                region,
                rx,
            } => {
                if Instant::now() >= next {
                    ink::dissolve_pass(&mut surf, region, stage, DRINK_STAGES);
                    let (x, y, w, h) = region.rect();
                    disp.update(x, y, w, h, true);
                    if stage + 1 >= DRINK_STAGES {
                        user_ink.clear();
                        State::Thinking {
                            rx,
                            since: Instant::now(),
                        }
                    } else {
                        State::Drinking {
                            stage: stage + 1,
                            next: Instant::now() + DRINK_STAGE_DELAY,
                            region,
                            rx,
                        }
                    }
                } else {
                    State::Drinking {
                        stage,
                        next,
                        region,
                        rx,
                    }
                }
            }

            State::Thinking { rx, since } => match rx.try_recv() {
                Ok(result) => {
                    // First streamed event: start writing now; keep the
                    // receiver so the rest of the reply can append itself.
                    match result {
                        Ok(Event::Show(id)) => {
                            // An incantation: the rest of this turn is the
                            // conjured memory, not a reply. (rx drops here.)
                            match conjure(&font, &store, id, &mut surf, &disp) {
                                Some(st) => st,
                                None => {
                                    eprintln!("riddle: memory {id} is missing");
                                    let plan = plan_reply(&font, &oracle_excuse("lost page"), None);
                                    turn_failed = true;
                                    State::Replying {
                                        plan,
                                        next: Instant::now(),
                                        rx: None,
                                    }
                                }
                            }
                        }
                        Ok(Event::TaskList) => {
                            let lines = task_store
                                .as_ref()
                                .map(|store| store.panel_lines())
                                .unwrap_or_default();
                            let enabled = task_store
                                .as_ref()
                                .map(|store| store.panel_enabled())
                                .unwrap_or_default();
                            let panel = task_panel::PaperList::show(
                                &mut surf,
                                &font,
                                "任務列表",
                                "尚無任務",
                                "橫劃可刪除 · 點右側方框啟用或停用 · 點空白退出",
                                &lines,
                                Some(&enabled),
                            );
                            disp.update_all(surf.w, surf.h);
                            eprintln!("magic-paper: recurring-task list opened");
                            State::TaskList { panel }
                        }
                        Ok(Event::TodoList) => {
                            let lines = todo_store
                                .as_ref()
                                .map(|store| store.panel_lines())
                                .unwrap_or_default();
                            let panel = task_panel::PaperList::show(
                                &mut surf,
                                &font,
                                "TODO 列表",
                                "尚無 TODO",
                                "橫劃 TODO 可刪除 · 點擊空白處退出",
                                &lines,
                                None,
                            );
                            disp.update_all(surf.w, surf.h);
                            eprintln!("magic-paper: TODO list opened");
                            State::TodoList { panel }
                        }
                        Ok(Event::FontList) => {
                            let panel = font_panel::FontPanel::show(&mut surf, &font);
                            disp.update_all(surf.w, surf.h);
                            eprintln!("magic-paper: font list opened");
                            State::FontList { panel }
                        }
                        Ok(Event::HistoryList) => {
                            let lines = store
                                .as_ref()
                                .map(|store| store.panel_lines(HISTORY_VISIBLE))
                                .unwrap_or_default();
                            let panel = task_panel::PaperList::show(
                                &mut surf,
                                &font,
                                "對話歷史",
                                "尚無歷史",
                                "橫劃一段歷史可刪除 · 點擊空白處退出",
                                &lines,
                                None,
                            );
                            disp.update_all(surf.w, surf.h);
                            eprintln!("magic-paper: history list opened");
                            State::HistoryList { panel }
                        }
                        Ok(Event::LocalCommand(command)) => {
                            turn_transcript = Some(command.clone());
                            let (reply, tasks_changed) =
                                apply_local_command(&command, &mut task_store, &mut todo_store);
                            if tasks_changed {
                                next_heartbeat = heartbeat_deadline(&task_store);
                            }
                            turn_reply.push_str(&reply);
                            let plan = plan_reply(&font, &reply, None);
                            State::Replying {
                                plan,
                                next: Instant::now(),
                                rx: None,
                            }
                        }
                        Ok(Event::Ink(text)) => {
                            turn_reply.push_str(&text);
                            let plan = plan_reply(&font, &text, None);
                            State::Replying {
                                plan,
                                next: Instant::now(),
                                rx: Some(rx),
                            }
                        }
                        Ok(Event::Transcript(t)) => {
                            // Transcript with no prose (model skipped the
                            // reply): remember the words, keep waiting.
                            if accept_transcript(
                                &mut turn_transcript,
                                t,
                                turn_kind,
                                &mut task_store,
                                &mut todo_store,
                            ) {
                                next_heartbeat = heartbeat_deadline(&task_store);
                            }
                            State::Thinking { rx, since }
                        }
                        Err(e) => {
                            eprintln!("riddle: oracle failed: {e}");
                            turn_failed = true;
                            let plan = plan_reply(&font, &oracle_excuse(&e), None);
                            State::Replying {
                                plan,
                                next: Instant::now(),
                                rx: None,
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if since.elapsed() >= ORACLE_PATIENCE {
                        // The oracle never answered (stalled stream, dead pi):
                        // say so instead of leaving a blank page forever.
                        eprintln!(
                            "riddle: oracle timed out after {}s",
                            ORACLE_PATIENCE.as_secs()
                        );
                        let plan = plan_reply(&font, &oracle_excuse("timed out"), None);
                        State::Replying {
                            plan,
                            next: Instant::now(),
                            rx: None,
                        }
                    } else {
                        State::Thinking { rx, since }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => State::Listening { last_pen: None },
            },

            State::Replying {
                mut plan,
                next,
                mut rx,
            } => {
                // More of the reply may still be streaming in: append each
                // new chunk below what is already planned, mid-animation.
                if let Some(ref r) = rx {
                    let drop_rx = match r.try_recv() {
                        Ok(Ok(Event::Ink(more))) => {
                            if plan.next_y > screen_h() as i32 - 200 {
                                // The page is full: let the rest go unwritten
                                // rather than inking below the visible page.
                                eprintln!(
                                    "riddle: reply reached the page bottom; trailing text dropped"
                                );
                                true
                            } else {
                                turn_reply.push_str(" ");
                                turn_reply.push_str(&more);
                                append_reply(&font, &mut plan, &more);
                                false
                            }
                        }
                        Ok(Ok(Event::Transcript(t))) => {
                            if accept_transcript(
                                &mut turn_transcript,
                                t,
                                turn_kind,
                                &mut task_store,
                                &mut todo_store,
                            ) {
                                next_heartbeat = heartbeat_deadline(&task_store);
                            }
                            false // the disconnect is still coming
                        }
                        Ok(Ok(
                            Event::Show(_)
                            | Event::TaskList
                            | Event::TodoList
                            | Event::FontList
                            | Event::HistoryList
                            | Event::LocalCommand(_),
                        )) => {
                            eprintln!("riddle: page directive mid-reply ignored");
                            false
                        }
                        Ok(Err(e)) => {
                            eprintln!("riddle: oracle failed mid-reply: {e}");
                            turn_failed = true;
                            true
                        }
                        Err(mpsc::TryRecvError::Disconnected) => true,
                        Err(mpsc::TryRecvError::Empty) => false,
                    };
                    if drop_rx {
                        rx = None;
                    }
                }
                if Instant::now() >= next {
                    let mut dirty = BBox::empty();
                    let mut budget = 26;
                    while budget > 0 && plan.stroke_i < plan.strokes.len() {
                        let stroke = &plan.strokes[plan.stroke_i];
                        if plan.point_i >= stroke.len() {
                            plan.stroke_i += 1;
                            plan.point_i = 0;
                            continue;
                        }
                        let (x, y) = stroke[plan.point_i];
                        if plan.point_i > 0 {
                            let (px, py) = stroke[plan.point_i - 1];
                            surf.brush_line(px, py, x, y, 2, BLACK);
                        } else {
                            surf.stamp(x, y, 2, BLACK);
                        }
                        dirty.add(x, y, 4);
                        plan.point_i += 1;
                        budget -= 1;
                    }
                    if !dirty.is_empty() {
                        let (x, y, w, h) = dirty.rect();
                        disp.update(x, y, w, h, true);
                    }
                    if plan.stroke_i >= plan.strokes.len() && rx.is_none() {
                        // The turn is complete: remember a handwritten page,
                        // or advance the recurring tasks that actually ran.
                        if !turn_failed && !turn_reply.is_empty() {
                            match turn_kind {
                                TurnKind::User => {
                                    if let Some(ref mut s) = store {
                                        s.append(
                                            turn_id,
                                            turn_transcript.as_deref().unwrap_or(""),
                                            turn_reply.trim(),
                                            &turn_strokes,
                                        );
                                    }
                                }
                                TurnKind::Heartbeat => {
                                    if let Some(ref mut s) = task_store {
                                        if let Err(e) = s.mark_ran(&turn_task_ids, unix_now()) {
                                            eprintln!("magic-paper: could not advance tasks: {e}");
                                        } else {
                                            next_heartbeat = heartbeat_deadline(&task_store);
                                            eprintln!(
                                                "magic-paper: completed {} heartbeat task(s)",
                                                turn_task_ids.len()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        turn_strokes = Vec::new();
                        turn_task_ids.clear();
                        let chars: usize = plan.strokes.iter().map(|s| s.len()).sum();
                        let linger = Duration::from_millis(4000 + (chars as u64) * 2);
                        let region = plan.region;
                        State::Lingering {
                            until: Instant::now() + linger.min(Duration::from_secs(20)),
                            region,
                        }
                    } else {
                        State::Replying {
                            plan,
                            next: Instant::now() + Duration::from_millis(14),
                            rx,
                        }
                    }
                } else {
                    State::Replying { plan, next, rx }
                }
            }

            State::Lingering { until, region } => {
                if Instant::now() >= until {
                    State::FadingReply {
                        stage: 0,
                        next: Instant::now(),
                        region,
                    }
                } else {
                    State::Lingering { until, region }
                }
            }

            State::Help { panel, until } => match panel {
                Some(p) => {
                    if stylus_tapped || Instant::now() >= until {
                        let region = p.dismiss(&mut surf);
                        let (x, y, w, h) = region.rect();
                        disp.update(x, y, w, h, false);
                        eprintln!("riddle: guide dismissed");
                        State::Help { panel: None, until }
                    } else {
                        State::Help {
                            panel: Some(p),
                            until,
                        }
                    }
                }
                // Dismissed: swallow the closing touch, listen again on pen-up.
                None if stylus_on => State::Help { panel: None, until },
                None => State::Listening { last_pen: None },
            },

            State::Conjuring {
                mut plan,
                next,
                saved,
            } => {
                if stylus_tapped {
                    // The writer interrupts: today's page returns at once.
                    surf.paste_rect(0, 0, screen_w(), screen_h(), &saved);
                    disp.full_refresh(surf.w, surf.h);
                    State::MemoryShown {
                        saved: None,
                        until: Instant::now(),
                        region: plan.region,
                    }
                } else if Instant::now() >= next {
                    // The memory pours back faster than Tom writes: it is
                    // remembered, not composed.
                    let mut dirty = BBox::empty();
                    let mut budget = 48;
                    while budget > 0 && plan.stroke_i < plan.strokes.len() {
                        let stroke = &plan.strokes[plan.stroke_i];
                        if plan.point_i >= stroke.len() {
                            plan.stroke_i += 1;
                            plan.point_i = 0;
                            continue;
                        }
                        let (x, y, r) = stroke[plan.point_i];
                        if plan.point_i > 0 {
                            let (px, py, pr) = stroke[plan.point_i - 1];
                            surf.brush_line(px, py, x, y, r.min(pr + 1), FADED);
                        } else {
                            surf.stamp(x, y, r, FADED);
                        }
                        dirty.add(x, y, r + 2);
                        plan.point_i += 1;
                        budget -= 1;
                    }
                    if !dirty.is_empty() {
                        let (x, y, w, h) = dirty.rect();
                        disp.update(x, y, w, h, true);
                    }
                    if plan.stroke_i >= plan.strokes.len() {
                        let region = plan.region;
                        State::MemoryShown {
                            saved: Some(saved),
                            until: Instant::now() + Duration::from_secs(120),
                            region,
                        }
                    } else {
                        State::Conjuring {
                            plan,
                            next: Instant::now() + Duration::from_millis(10),
                            saved,
                        }
                    }
                } else {
                    State::Conjuring { plan, next, saved }
                }
            }

            State::MemoryShown {
                saved,
                until,
                region,
            } => match saved {
                Some(s) => {
                    if stylus_tapped || Instant::now() >= until {
                        // The paper swallows its memory; today's page returns.
                        surf.paste_rect(0, 0, screen_w(), screen_h(), &s);
                        disp.full_refresh(surf.w, surf.h);
                        eprintln!("riddle: memory dismissed");
                        State::MemoryShown {
                            saved: None,
                            until,
                            region,
                        }
                    } else {
                        State::MemoryShown {
                            saved: Some(s),
                            until,
                            region,
                        }
                    }
                }
                // Dismissed: swallow the closing touch, listen again on pen-up.
                None if stylus_on => State::MemoryShown {
                    saved: None,
                    until,
                    region,
                },
                None => State::Listening { last_pen: None },
            },

            State::TaskList { panel } => State::TaskList { panel },
            State::TodoList { panel } => State::TodoList { panel },
            State::FontList { panel } => State::FontList { panel },
            State::HistoryList { panel } => State::HistoryList { panel },

            State::FadingReply {
                stage,
                next,
                region,
            } => {
                const STAGES: u32 = 10;
                if Instant::now() >= next {
                    ink::dissolve_pass(&mut surf, region, stage, STAGES);
                    let (x, y, w, h) = region.rect();
                    disp.update(x, y, w, h, true);
                    if stage + 1 >= STAGES {
                        // Settle only the erased reply region with the balanced
                        // waveform. A full-panel flash after every answer is
                        // distracting; global refreshes remain at sleep/wake
                        // and memory-page transitions where they are needed.
                        disp.update(x, y, w, h, false);
                        State::Listening { last_pen: None }
                    } else {
                        State::FadingReply {
                            stage: stage + 1,
                            next: Instant::now() + Duration::from_millis(80),
                            region,
                        }
                    }
                } else {
                    State::FadingReply {
                        stage,
                        next,
                        region,
                    }
                }
            }
        };

        stylus_tapped = false;
        std::thread::sleep(Duration::from_millis(2));
    }

    eprintln!("riddle: the diary closes");
    cancel_speculative(&mut speculative, "diary closed");
    disp.terminate();
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn heartbeat_retry_interval() -> Duration {
    let secs = std::env::var("RIDDLE_HEARTBEAT_RETRY_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(HEARTBEAT_RETRY_DEFAULT.as_secs())
        .max(5);
    Duration::from_secs(secs)
}

/// Smart heartbeat: sleep logically until the nearest active task is due.
/// No active task means no heartbeat and, crucially, no oracle/API request.
fn heartbeat_deadline(task_store: &Option<tasks::TaskStore>) -> Option<Instant> {
    let due = task_store.as_ref()?.next_due()?;
    let wait = due.saturating_sub(unix_now());
    eprintln!("magic-paper: next task check in {wait}s");
    Some(Instant::now() + Duration::from_secs(wait))
}

fn finish_paper_list_stroke(
    state: &mut State,
    memory_store: &mut Option<memory::MemoryStore>,
    task_store: &mut Option<tasks::TaskStore>,
    todo_store: &mut Option<todos::TodoStore>,
    next_heartbeat: &mut Option<Instant>,
    surf: &mut Surface,
    font: &mut fonts::FontBook,
    disp: &display::Display,
) {
    if matches!(state, State::FontList { .. }) {
        let action = match state {
            State::FontList { panel } => panel.pen_up(),
            _ => None,
        };
        match action {
            Some(font_panel::Action::Select(id)) => {
                if let Err(error) = font.select(id) {
                    eprintln!("magic-paper: could not persist font selection: {error}");
                }
                if let State::FontList { panel } = state {
                    panel.redraw(surf, font);
                }
                disp.update_all(surf.w, surf.h);
                eprintln!("magic-paper: selected font {}", id.stable_id());
            }
            Some(font_panel::Action::Dismiss) => {
                let old = std::mem::replace(state, State::Listening { last_pen: None });
                match old {
                    State::FontList { panel } => panel.dismiss(surf),
                    _ => unreachable!(),
                }
                disp.update_all(surf.w, surf.h);
                eprintln!("magic-paper: font list dismissed");
            }
            Some(font_panel::Action::Redraw) => {
                if let State::FontList { panel } = state {
                    panel.redraw(surf, font);
                }
                disp.update_all(surf.w, surf.h);
            }
            None => {}
        }
        return;
    }

    let action = match state {
        State::TaskList { panel } | State::TodoList { panel } | State::HistoryList { panel } => {
            panel.pen_up()
        }
        _ => None,
    };
    let Some(action) = action else {
        return;
    };
    match action {
        task_panel::Action::Delete(number) => {
            match state {
                State::TaskList { .. } => match task_store.as_mut() {
                    Some(store) => match store.delete_number(number) {
                        Ok(task) => {
                            eprintln!(
                                "magic-paper: task {number} deleted from paper list — {}",
                                task.instruction
                            );
                            *next_heartbeat = heartbeat_deadline(task_store);
                        }
                        Err(e) => eprintln!("magic-paper: could not delete task {number}: {e}"),
                    },
                    None => eprintln!("magic-paper: task storage is disabled"),
                },
                State::TodoList { .. } => match todo_store.as_mut() {
                    Some(store) => match store.delete_number(number) {
                        Ok(todo) => eprintln!(
                            "magic-paper: TODO {number} deleted from paper list — {}",
                            todo.text
                        ),
                        Err(e) => eprintln!("magic-paper: could not delete TODO {number}: {e}"),
                    },
                    None => eprintln!("magic-paper: TODO storage is disabled"),
                },
                State::HistoryList { .. } => match memory_store.as_mut() {
                    Some(store) => match store.delete_number(number, HISTORY_VISIBLE) {
                        Ok(entry) => eprintln!(
                            "magic-paper: history {number} deleted — {}",
                            entry.transcript
                        ),
                        Err(error) => {
                            eprintln!("magic-paper: could not delete history {number}: {error}")
                        }
                    },
                    None => eprintln!("magic-paper: memory storage is disabled"),
                },
                _ => {}
            }
            redraw_paper_list(state, memory_store, task_store, todo_store, surf, font);
            disp.update_all(surf.w, surf.h);
        }
        task_panel::Action::Toggle(number) => {
            if let State::TaskList { .. } = state {
                match task_store.as_mut() {
                    Some(store) => match store.toggle_number(number, unix_now()) {
                        Ok(task) => {
                            eprintln!(
                                "magic-paper: task {number} {} from paper list",
                                if task.paused { "disabled" } else { "enabled" }
                            );
                            *next_heartbeat = heartbeat_deadline(task_store);
                        }
                        Err(error) => {
                            eprintln!("magic-paper: could not toggle task {number}: {error}")
                        }
                    },
                    None => eprintln!("magic-paper: task storage is disabled"),
                }
            }
            redraw_paper_list(state, memory_store, task_store, todo_store, surf, font);
            disp.update_all(surf.w, surf.h);
        }
        task_panel::Action::Dismiss => {
            let old = std::mem::replace(state, State::Listening { last_pen: None });
            match old {
                State::TaskList { panel }
                | State::TodoList { panel }
                | State::HistoryList { panel } => panel.dismiss(surf),
                _ => unreachable!(),
            }
            disp.update_all(surf.w, surf.h);
            eprintln!("magic-paper: paper list dismissed");
        }
        task_panel::Action::Redraw => {
            redraw_paper_list(state, memory_store, task_store, todo_store, surf, font);
            disp.update_all(surf.w, surf.h);
        }
    }
}

fn redraw_paper_list(
    state: &mut State,
    memory_store: &Option<memory::MemoryStore>,
    task_store: &Option<tasks::TaskStore>,
    todo_store: &Option<todos::TodoStore>,
    surf: &mut Surface,
    font: &fonts::FontBook,
) {
    match state {
        State::TaskList { panel } => {
            let lines = task_store
                .as_ref()
                .map(|store| store.panel_lines())
                .unwrap_or_default();
            let enabled = task_store
                .as_ref()
                .map(|store| store.panel_enabled())
                .unwrap_or_default();
            panel.redraw(
                surf,
                font,
                "任務列表",
                "尚無任務",
                "橫劃可刪除 · 點右側方框啟用或停用 · 點空白退出",
                &lines,
                Some(&enabled),
            );
        }
        State::TodoList { panel } => {
            let lines = todo_store
                .as_ref()
                .map(|store| store.panel_lines())
                .unwrap_or_default();
            panel.redraw(
                surf,
                font,
                "TODO 列表",
                "尚無 TODO",
                "橫劃 TODO 可刪除 · 點擊空白處退出",
                &lines,
                None,
            );
        }
        State::HistoryList { panel } => {
            let lines = memory_store
                .as_ref()
                .map(|store| store.panel_lines(HISTORY_VISIBLE))
                .unwrap_or_default();
            panel.redraw(
                surf,
                font,
                "對話歷史",
                "尚無歷史",
                "橫劃一段歷史可刪除 · 點擊空白處退出",
                &lines,
                None,
            );
        }
        _ => {}
    }
}

fn apply_local_command(
    command: &str,
    task_store: &mut Option<tasks::TaskStore>,
    todo_store: &mut Option<todos::TodoStore>,
) -> (String, bool) {
    if let Some(store) = task_store.as_mut() {
        match store.apply_from_transcript(command, unix_now()) {
            Ok(Some(change)) => {
                let reply = match change {
                    tasks::TaskChange::Added(task) => {
                        format!("任務已新增：{}。", task.instruction)
                    }
                    tasks::TaskChange::Deleted { number, task } => {
                        format!("已刪除任務 {number}：{}。", task.instruction)
                    }
                    tasks::TaskChange::Paused { number, .. } => {
                        format!("任務 {number} 已暫停。")
                    }
                    tasks::TaskChange::Resumed { number, .. } => {
                        format!("任務 {number} 已恢復。")
                    }
                    tasks::TaskChange::Modified { number, after, .. } => {
                        format!("任務 {number} 已修改為：{}。", after.instruction)
                    }
                };
                eprintln!("magic-paper: local task command applied — {command}");
                return (reply, true);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("magic-paper: local task command rejected: {error}");
                return (
                    "任務指令無法執行，請檢查編號、間隔或任務上限。".into(),
                    false,
                );
            }
        }
    }

    if let Some(store) = todo_store.as_mut() {
        match store.add_from_transcript(command, unix_now()) {
            Ok(Some(todo)) => {
                eprintln!("magic-paper: local TODO added — {}", todo.text);
                return (format!("TODO 已新增：{}。", todo.text), false);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("magic-paper: local TODO rejected: {error}");
                return ("TODO 無法新增，請檢查內容或列表上限。".into(), false);
            }
        }
    }

    ("指令未能辨識，請再寫一次。".into(), false)
}

fn accept_transcript(
    slot: &mut Option<String>,
    transcript: String,
    kind: TurnKind,
    task_store: &mut Option<tasks::TaskStore>,
    todo_store: &mut Option<todos::TodoStore>,
) -> bool {
    let first = slot.is_none();
    let mut scheduled_tasks_changed = false;
    if first && kind == TurnKind::User {
        if let Some(store) = task_store.as_mut() {
            match store.apply_from_transcript(&transcript, unix_now()) {
                Ok(Some(change)) => {
                    scheduled_tasks_changed = true;
                    eprintln!("magic-paper: task list changed — {change:?}");
                }
                Ok(None) => {}
                Err(e) => eprintln!("magic-paper: task command rejected: {e}"),
            }
        }
        if let Some(store) = todo_store.as_mut() {
            match store.add_from_transcript(&transcript, unix_now()) {
                Ok(Some(todo)) => eprintln!("magic-paper: TODO added — {}", todo.text),
                Ok(None) => {}
                Err(e) => eprintln!("magic-paper: TODO command rejected: {e}"),
            }
        }
    }
    *slot = Some(transcript);
    scheduled_tasks_changed
}

/// True if the region no longer holds any dark pixels (fully erased).
fn region_all_white(surf: &Surface, region: BBox) -> bool {
    if region.is_empty() {
        return true;
    }
    for y in region.y0..=region.y1 {
        for x in region.x0..=region.x1 {
            if surf.luma(x, y) < 200 {
                return false;
            }
        }
    }
    true
}

/// What MP writes when the spirit cannot answer: short and actionable. The
/// raw error still goes to stderr.
fn oracle_excuse(e: &str) -> String {
    if e.contains("no oracle") {
        "MagicPaper lies dormant: it found no oracle. \
         Put an API key in oracle.env, then open me again."
            .into()
    } else if e.starts_with("http 401") || e.starts_with("http 403") {
        "The oracle refused MagicPaper's key. Check RIDDLE_OPENAI_KEY in oracle.env.".into()
    } else if e.starts_with("http ") {
        let code = e.split(':').next().unwrap_or("an error");
        format!("The oracle rejected MagicPaper's plea ({code}). Check the model and endpoint in oracle.env.")
    } else if e.contains("request failed") || e.contains("timed out") {
        "MagicPaper cannot reach its oracle. Is the tablet connected to Wi-Fi?".into()
    } else if e.contains("empty reply") {
        "The spirit read your words but said nothing. Write again.".into()
    } else {
        "The ink blurred before it could answer. Write again.".into()
    }
}

/// Summon a remembered page: snapshot today's page, clear the paper, and plan
/// the memory's rewriting — the date in a small hand, the writer's own strokes
/// exactly as they were penned, Tom's old reply beneath — all in faded ink.
fn conjure(
    font: &fonts::FontBook,
    store: &Option<memory::MemoryStore>,
    id: u64,
    surf: &mut Surface,
    disp: &display::Display,
) -> Option<State> {
    let s = store.as_ref()?;
    let entry = s.get(id)?.clone();
    let strokes = s.strokes(id).unwrap_or_default();
    eprintln!(
        "riddle: conjuring memory {id} ({})",
        memory::spoken_date(id)
    );

    let saved = surf.copy_rect(0, 0, screen_w(), screen_h());
    surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
    disp.update_all(surf.w, surf.h);

    let mut all: Vec<Vec<(i32, i32, i32)>> = Vec::new();
    let mut region = BBox::empty();

    // The date, small and centered near the top, like a diary heading.
    let date = memory::spoken_date(entry.id);
    let mut raster = script::rasterize_line(font, &date, 54.0);
    script::thin(&mut raster);
    let x0 = (screen_w() as i32 - raster.width as i32) / 2;
    let mut ink_bottom = 64;
    for stroke in script::trace(&raster) {
        let mapped: Vec<(i32, i32, i32)> = stroke
            .iter()
            .map(|&(sx, sy)| (x0 + sx, 64 + sy, 1))
            .collect();
        for &(x, y, r) in &mapped {
            region.add(x, y, r + 2);
            ink_bottom = ink_bottom.max(y);
        }
        all.push(mapped);
    }

    // The writer's own hand, exactly as it was penned.
    for stroke in &strokes {
        for &(x, y, r) in stroke {
            region.add(x, y, r + 2);
            ink_bottom = ink_bottom.max(y);
        }
        all.push(stroke.clone());
    }

    // Tom's old reply, below.
    if !entry.reply.is_empty() {
        let y = (ink_bottom + 130).min(screen_h() as i32 - 400);
        let reply = plan_reply(font, &entry.reply, Some(y));
        for stroke in reply.strokes {
            let mapped: Vec<(i32, i32, i32)> = stroke.iter().map(|&(x, y)| (x, y, 2)).collect();
            for &(x, y, r) in &mapped {
                region.add(x, y, r + 2);
            }
            all.push(mapped);
        }
    }

    Some(State::Conjuring {
        plan: ConjurePlan {
            strokes: all,
            stroke_i: 0,
            point_i: 0,
            region,
        },
        next: Instant::now(),
        saved,
    })
}

/// Lay out reply text and produce screen-space strokes. `y_start` continues a
/// streamed reply below its previous chunk; None places the first chunk.
fn plan_reply(font: &fonts::FontBook, text: &str, y_start: Option<i32>) -> WritePlan {
    let max_w = (screen_w() as i32 - 2 * MARGIN_X) as f32;
    let lines = script::wrap(font, text, REPLY_PX, max_w);
    let line_h = (REPLY_PX * 1.25) as i32;
    let total_h = line_h * lines.len() as i32;
    let mut y = y_start.unwrap_or(((screen_h() as i32 - total_h) / 3).max(60));
    let mut strokes = Vec::new();
    let mut region = BBox::empty();
    let mut seed = 0x1234u32;
    let mut jitter = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((seed >> 16) % 7) as i32 - 3
    };

    for line_text in &lines {
        let mut raster = script::rasterize_line(font, line_text, REPLY_PX);
        script::thin(&mut raster);
        let line_strokes = script::trace(&raster);
        let x0 = (screen_w() as i32 - raster.width as i32) / 2;
        let wobble = jitter();
        for s in line_strokes {
            let mapped: Vec<(i32, i32)> = s
                .iter()
                .map(|&(sx, sy)| (x0 + sx, y + sy + wobble))
                .collect();
            for &(x, yy) in &mapped {
                region.add(x, yy, 5);
            }
            strokes.push(mapped);
        }
        y += line_h;
    }

    WritePlan {
        strokes,
        stroke_i: 0,
        point_i: 0,
        region,
        next_y: y,
    }
}

/// Splice a streamed continuation chunk into a running write animation.
fn append_reply(font: &fonts::FontBook, plan: &mut WritePlan, more: &str) {
    let cont = plan_reply(font, more, Some(plan.next_y));
    if cont.strokes.is_empty() {
        return;
    }
    plan.region.add(cont.region.x0, cont.region.y0, 0);
    plan.region.add(cont.region.x1, cont.region.y1, 0);
    plan.strokes.extend(cont.strokes);
    plan.next_y = cont.next_y;
}
