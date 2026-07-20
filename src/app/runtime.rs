//! MagicPaper's device runtime and central interaction loop.
//!
//! Write on the page with the pen. After a pause the diary drinks your ink,
//! and an answer writes itself onto the page in a flowing hand, then fades.
//!
//! Two display backends (picked at runtime): windowed via qtfb/AppLoad when
//! QTFB_KEY is set, or full takeover via the vendor engine (quill) when
//! built with --features takeover and launched with xochitl stopped.

use crate::{
    agent, display, fb, fonts, ink, memory, oracle, pen, power, qtfb, reader, tasks, todos, touch,
    ui,
};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::fb::{screen_h, screen_w, BBox};
use crate::oracle::Event;
use crate::surface::{BLACK, FADED, WHITE};

pub(super) const PNG_PATH: &str = "/tmp/riddle-page.png";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RunOutcome {
    Closed,
    OpenReader(PathBuf),
}

/// Begin reading a tentative page while its ink is still visible. The page is
/// not committed until the adaptive fast/slow deadline; more pen input
/// invalidates this request.
const IDLE_PREASK: Duration = Duration::from_millis(1000);
const DRINK_STAGES: u32 = 14;
const DRINK_STAGE_DELAY: Duration = Duration::from_millis(50);
/// How long the diary waits on a silent oracle before giving up on the turn.
/// Generous: thinking models can lead with a long silence.
const ORACLE_PATIENCE: Duration = Duration::from_secs(120);

use super::context::build_ctx;
use super::lists::{
    accept_transcript, apply_local_command, finish_paper_list_stroke, HISTORY_VISIBLE,
};
use super::reply::{append_reply, conjure, oracle_excuse, plan_reply, region_all_white};
use super::state::{cancel_speculative, idle_commit_delay, SpeculativeRequest, State, TurnKind};
use super::timing::{heartbeat_deadline, heartbeat_retry_interval, unix_now};

pub(super) fn run() -> std::io::Result<RunOutcome> {
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
    let manager_owns_power = std::env::var_os("REMAGIC_MANAGED").is_some();
    let mut power_dev = if takeover && !manager_owns_power {
        power::PowerButton::open()
            .map_err(|e| eprintln!("riddle: no power button ({e})"))
            .ok()
    } else {
        if manager_owns_power {
            eprintln!("riddle: Remagic manager owns the power-button lifecycle");
        }
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
    if let Some(error) = reader::take_launch_error() {
        eprintln!("magic-paper: KOReader handoff reported: {error}");
        state = State::Replying {
            plan: plan_reply(&font, &error, None),
            next: Instant::now(),
            rx: None,
        };
    }
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
    // Under Remagic the screenless agent owns scheduled execution. The UI
    // only consumes queued results, so a timer can never race Master's pen.
    let mut next_heartbeat = if manager_owns_power {
        None
    } else {
        heartbeat_deadline(&task_store)
    };
    let mut next_agent_poll = Instant::now();
    // Raw stylus contact, tracked in every state (the guide dismisses on it).
    // `stylus_on` is the level; `stylus_tapped` latches any contact seen this
    // loop iteration, so a tap that starts AND ends within one drain still
    // registers.
    let mut stylus_on = false;
    let mut stylus_tapped = false;
    let mut ink_dirty = BBox::empty();
    let mut last_flush = Instant::now();
    let mut reader_target: Option<PathBuf> = None;
    // Takeover swaps are cheap and synchronous; qtfb needs coalescing.
    let flush_every = if takeover {
        Duration::from_millis(8)
    } else {
        Duration::from_millis(35)
    };

    eprintln!("riddle: the diary is open");

    'main: loop {
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
                let saved = ui::help::show_sleep(&mut surf, &font);
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
                ui::help::restore_sleep(&mut surf, &saved);
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
                            | State::ReaderList { .. }
                    ) {
                        pen_down = false;
                        if let Some(path) = finish_paper_list_stroke(
                            &mut state,
                            &mut store,
                            &mut task_store,
                            &mut todo_store,
                            &mut next_heartbeat,
                            &mut surf,
                            &mut font,
                            &disp,
                        ) {
                            reader_target = Some(path);
                            break 'main;
                        }
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
                    | State::HistoryList { ref mut panel }
                    | State::ReaderList { ref mut panel, .. } => {
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
                    | State::HistoryList { ref mut panel }
                    | State::ReaderList { ref mut panel, .. } = state
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
                            | State::ReaderList { .. }
                    ) {
                        pen_down = false;
                        if let Some(path) = finish_paper_list_stroke(
                            &mut state,
                            &mut store,
                            &mut task_store,
                            &mut todo_store,
                            &mut next_heartbeat,
                            &mut surf,
                            &mut font,
                            &disp,
                        ) {
                            reader_target = Some(path);
                            break 'main;
                        }
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
                    } else if ui::help::looks_like_question_mark(user_ink.stroke_list()) {
                        // Absorb the "?" and open the guide instead of asking.
                        cancel_speculative(&mut speculative, "guide gesture");
                        let (qx, qy, qw, qh) = user_ink.bbox.rect();
                        surf.fill_rect(qx as usize, qy as usize, qw as usize, qh as usize, WHITE);
                        disp.update(qx, qy, qw, qh, false);
                        user_ink.clear();
                        let panel = ui::help::show(&mut surf, &font, takeover);
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
                        && !ui::help::looks_like_question_mark(user_ink.stroke_list())
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
                _ if manager_owns_power
                    && !pen_down
                    && !stylus_tapped
                    && user_ink.is_empty()
                    && Instant::now() >= next_agent_poll =>
                {
                    next_agent_poll = Instant::now() + Duration::from_secs(1);
                    match agent::take_pending() {
                        Ok(Some(reply)) => {
                            eprintln!("magic-paper: showing queued scheduled result");
                            turn_kind = TurnKind::User;
                            turn_reply.clear();
                            State::Replying {
                                plan: plan_reply(&font, &reply, None),
                                next: Instant::now(),
                                rx: None,
                            }
                        }
                        Ok(None) => State::Listening { last_pen },
                        Err(error) => {
                            eprintln!("magic-paper: could not read agent queue: {error}");
                            State::Listening { last_pen }
                        }
                    }
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
                            let panel = ui::paper_list::PaperList::show(
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
                            let panel = ui::paper_list::PaperList::show(
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
                            let panel = ui::font_settings::FontPanel::show(&mut surf, &font);
                            disp.update_all(surf.w, surf.h);
                            eprintln!("magic-paper: font list opened");
                            State::FontList { panel }
                        }
                        Ok(Event::HistoryList) => {
                            let lines = store
                                .as_ref()
                                .map(|store| store.panel_lines(HISTORY_VISIBLE))
                                .unwrap_or_default();
                            let panel = ui::paper_list::PaperList::show(
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
                        Ok(Event::Help) => {
                            let panel = ui::help::show(&mut surf, &font, takeover);
                            let (x, y, w, h) = panel.region.rect();
                            disp.update(x, y, w, h, false);
                            eprintln!("magic-paper: instruction manual opened");
                            State::Help {
                                panel: Some(panel),
                                until: Instant::now() + Duration::from_secs(180),
                            }
                        }
                        Ok(Event::Reader(query)) => match reader::Catalog::open() {
                            Ok(catalog) => {
                                eprintln!(
                                    "magic-paper: reader catalog holds {} books",
                                    catalog.len()
                                );
                                match catalog.lookup(query.as_deref()) {
                                    reader::Lookup::Open(path) => {
                                        eprintln!(
                                            "magic-paper: handing page to KOReader — {}",
                                            path.display()
                                        );
                                        reader_target = Some(path);
                                        break 'main;
                                    }
                                    reader::Lookup::Choose(books) => {
                                        let lines: Vec<String> =
                                            books.iter().map(reader::Book::panel_label).collect();
                                        let panel = ui::paper_list::PaperList::show_selectable(
                                            &mut surf,
                                            &font,
                                            "選擇要閱讀的書",
                                            "沒有相符書籍",
                                            "用筆點書名開啟 · 點空白退出",
                                            &lines,
                                        );
                                        disp.update_all(surf.w, surf.h);
                                        eprintln!(
                                            "magic-paper: {} ambiguous reader candidates shown",
                                            books.len()
                                        );
                                        State::ReaderList { panel, books }
                                    }
                                    reader::Lookup::Missing => {
                                        let text = match query {
                                            Some(title) => {
                                                format!("沒有找到《{}》，請寫更完整的書名。", title)
                                            }
                                            None => "沒有找到可用的 KOReader 書庫。".into(),
                                        };
                                        let plan = plan_reply(&font, &text, None);
                                        State::Replying {
                                            plan,
                                            next: Instant::now(),
                                            rx: None,
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                eprintln!("magic-paper: could not scan reader catalog: {error}");
                                let plan = plan_reply(&font, "書庫暫時無法讀取。", None);
                                State::Replying {
                                    plan,
                                    next: Instant::now(),
                                    rx: None,
                                }
                            }
                        },
                        Ok(Event::FullRefresh) => {
                            eprintln!("magic-paper: manual full-screen refresh");
                            disp.full_refresh(surf.w, surf.h);
                            State::Listening { last_pen: None }
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
                            | Event::Help
                            | Event::Reader(_)
                            | Event::FullRefresh
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
            State::ReaderList { panel, books } => State::ReaderList { panel, books },

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
    Ok(match reader_target {
        Some(path) => RunOutcome::OpenReader(path),
        None => RunOutcome::Closed,
    })
}
