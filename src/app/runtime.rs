//! MagicPaper's device runtime and central interaction loop.
//!
//! The root owns setup/teardown and durable runtime state. Lifecycle, input,
//! power, and page-state transitions live in focused sibling modules.

use crate::{
    display, fb, fonts, ink, memory, pen, power, runtime_control, runtime_env, tasks, todos, touch,
};

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::fb::{screen_h, screen_w, BBox};
use crate::platform::{InputMode, RefreshIntent};
use crate::surface::WHITE;

use super::input::{InputPriority, ModalContact, PenSequence, PenTrace, QtfbPenState};
use super::lifecycle::{LifecycleClient, LifecycleStage};
use super::oracle_controller::{OracleController, SpeculativeRequest};
use super::state::{State, TurnKind};
use super::timing::heartbeat_deadline;

mod input_loop;
mod lifecycle_loop;
mod power_loop;
mod state_loop;

pub(super) const PNG_PATH: &str = "/tmp/riddle-page.png";
pub(super) const IDLE_PREASK: Duration = Duration::from_millis(1000);
pub(super) const DRINK_STAGES: u32 = 14;
pub(super) const DRINK_STAGE_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RunOutcome {
    Closed,
    Failed,
}

pub(super) enum LifecycleExit {
    Complete {
        deadline: Duration,
    },
    Failed {
        stage: LifecycleStage,
        message: String,
        retryable: bool,
    },
}

struct Devices {
    pen: Option<pen::PenDevice>,
    touch: Option<touch::TouchDevice>,
    power: Option<power::PowerButton>,
}

struct Stores {
    memory: Option<memory::MemoryStore>,
    tasks: Option<tasks::TaskStore>,
    todos: Option<todos::TodoStore>,
}

pub(super) struct Engine<'a> {
    disp: &'a display::Display,
    surf: crate::surface::Surface,
    font: fonts::FontBook,
    lifecycle: LifecycleClient,
    lifecycle_frame_sequence: u64,
    live_ink: display::LegacyLiveInkAdapter<'a>,
    takeover: bool,
    hosted: bool,
    managed: bool,
    input_mode: InputMode,
    input_mode_synced: bool,
    agent_queue_mode: bool,
    pen_dev: Option<pen::PenDevice>,
    touch_dev: Option<touch::TouchDevice>,
    power_dev: Option<power::PowerButton>,
    power_grace: Instant,
    power_clicks: power::ClickTracker,
    store: Option<memory::MemoryStore>,
    task_store: Option<tasks::TaskStore>,
    todo_store: Option<todos::TodoStore>,
    oracle: OracleController,
    user_ink: ink::Ink,
    state: State,
    pen_down: bool,
    qtfb_pen: QtfbPenState,
    pen_sequence: PenSequence,
    pen_trace: PenTrace,
    input_priority: InputPriority,
    lifecycle_foreground: bool,
    lifecycle_exit: LifecycleExit,
    speculative: Option<SpeculativeRequest>,
    speculative_attempted: bool,
    turn_id: u64,
    turn_strokes: memory::Strokes,
    turn_reply: String,
    turn_transcript: Option<String>,
    turn_failed: bool,
    turn_kind: TurnKind,
    turn_tasks: Vec<tasks::Task>,
    ui_scheduler_lease: Option<tasks::SchedulerLease>,
    next_heartbeat: Option<Instant>,
    next_agent_poll: Instant,
    stylus_on: bool,
    stylus_tapped: bool,
    ink_dirty: BBox,
    ink_flush_urgent: bool,
    last_flush: Instant,
    reader_target: Option<PathBuf>,
    primary_touch: Option<i32>,
    modal_contact: Option<ModalContact>,
    flush_every: Duration,
}

pub(super) fn run(launch_mode: runtime_env::LaunchMode) -> std::io::Result<RunOutcome> {
    let runtime_managed = runtime_env::validate_launch(launch_mode)?;
    // Validate and duplicate the manager-owned lifecycle endpoint before the
    // application opens QTFB or any raw input/display resource.
    let mut lifecycle = LifecycleClient::discover(runtime_managed)?;
    let font = fonts::FontBook::open()?;
    let allow_takeover = launch_mode == runtime_env::LaunchMode::LegacyTakeover;
    let (disp, mut surf) = display::Display::open(allow_takeover)?;
    fb::init_screen(surf.w, surf.h);
    commit_initial_frame(&disp, &mut surf, &mut lifecycle)?;

    let sigterm = termination_flag()?;
    let mut engine = Engine::new(&disp, surf, font, lifecycle, runtime_managed);
    eprintln!("riddle: the diary is open");
    if engine.set_input_mode(InputMode::Writing) {
        engine.run_loop(&sigterm);
    }
    let (mut lifecycle, lifecycle_exit) = engine.close();
    disp.terminate();
    finish_lifecycle(&mut lifecycle, lifecycle_exit)
}

fn commit_initial_frame(
    disp: &display::Display,
    surf: &mut crate::surface::Surface,
    lifecycle: &mut LifecycleClient,
) -> std::io::Result<()> {
    surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
    if let Err(error) = disp.present_all_checked(surf.w, surf.h, RefreshIntent::Content) {
        let _ = lifecycle.report_failed(
            LifecycleStage::Start,
            &format!("initial frame commit failed: {error}"),
            true,
            Duration::from_millis(100),
        );
        return Err(error);
    }
    if let Err(error) = lifecycle.report_ready_after_frame(1) {
        let _ = lifecycle.report_failed(
            LifecycleStage::Start,
            &format!("could not report initial readiness: {error}"),
            true,
            Duration::from_millis(100),
        );
        return Err(error);
    }
    Ok(())
}

fn termination_flag() -> std::io::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&flag))?;
    Ok(flag)
}

fn finish_lifecycle(
    lifecycle: &mut LifecycleClient,
    exit: LifecycleExit,
) -> std::io::Result<RunOutcome> {
    let failed = matches!(&exit, LifecycleExit::Failed { .. });
    let result = match exit {
        LifecycleExit::Complete { deadline } => lifecycle.report_shutdown_complete(0, deadline),
        LifecycleExit::Failed {
            stage,
            message,
            retryable,
        } => lifecycle.report_failed(stage, &message, retryable, Duration::from_millis(100)),
    };
    if let Err(error) = result {
        eprintln!("magic-paper: final lifecycle event was not delivered: {error}");
    }
    Ok(if failed {
        RunOutcome::Failed
    } else {
        RunOutcome::Closed
    })
}

impl<'a> Engine<'a> {
    fn new(
        disp: &'a display::Display,
        surf: crate::surface::Surface,
        font: fonts::FontBook,
        lifecycle: LifecycleClient,
        managed: bool,
    ) -> Self {
        let takeover = matches!(disp, display::Display::Quill);
        let hosted = !takeover;
        let devices = open_devices(takeover, hosted);
        let stores = open_stores();
        let remember = stores.memory.is_some() || stores.tasks.is_some() || stores.todos.is_some();
        let oracle = OracleController::spawn(remember);
        let agent_queue_mode = hosted || tasks::external_scheduler_active();
        let next_heartbeat = (!agent_queue_mode)
            .then(|| heartbeat_deadline(&stores.tasks))
            .flatten();
        log_display(disp, &surf, takeover);
        Self {
            disp,
            live_ink: display::LegacyLiveInkAdapter::new(disp),
            surf,
            font,
            lifecycle,
            lifecycle_frame_sequence: 1,
            takeover,
            hosted,
            managed,
            input_mode: InputMode::AnimationLocked,
            input_mode_synced: false,
            agent_queue_mode,
            pen_dev: devices.pen,
            touch_dev: devices.touch,
            power_dev: devices.power,
            power_grace: Instant::now(),
            power_clicks: power::ClickTracker::new(),
            store: stores.memory,
            task_store: stores.tasks,
            todo_store: stores.todos,
            oracle,
            user_ink: ink::Ink::new(),
            state: State::Listening { last_pen: None },
            pen_down: false,
            qtfb_pen: QtfbPenState::default(),
            pen_sequence: PenSequence::default(),
            pen_trace: PenTrace::default(),
            input_priority: InputPriority::foreground(),
            lifecycle_foreground: true,
            lifecycle_exit: LifecycleExit::Complete {
                deadline: Duration::from_millis(100),
            },
            speculative: None,
            speculative_attempted: false,
            turn_id: 0,
            turn_strokes: Vec::new(),
            turn_reply: String::new(),
            turn_transcript: None,
            turn_failed: false,
            turn_kind: TurnKind::User,
            turn_tasks: Vec::new(),
            ui_scheduler_lease: None,
            next_heartbeat,
            next_agent_poll: Instant::now(),
            stylus_on: false,
            stylus_tapped: false,
            ink_dirty: BBox::empty(),
            ink_flush_urgent: false,
            last_flush: Instant::now(),
            reader_target: None,
            primary_touch: None,
            modal_contact: None,
            flush_every: if takeover {
                Duration::from_millis(8)
            } else {
                Duration::from_millis(16)
            },
        }
    }

    fn close(mut self) -> (LifecycleClient, LifecycleExit) {
        eprintln!("riddle: the diary closes");
        self.input_priority.enter_background();
        self.oracle.invalidate_active_turn();
        super::oracle_controller::cancel_speculative(&mut self.speculative, "diary closed");
        self.cancel_modal_contact();
        suspend_visible_state(&mut self.state, &mut self.surf, &mut self.user_ink);
        self.pen_trace.finish("diary-closed");
        (self.lifecycle, self.lifecycle_exit)
    }

    pub(super) fn fail(&mut self, stage: LifecycleStage, message: String) {
        self.lifecycle_exit = LifecycleExit::Failed {
            stage,
            message,
            retryable: true,
        };
    }

    pub(super) fn set_input_mode(&mut self, mode: InputMode) -> bool {
        if !input_mode_needs_request(self.input_mode_synced, self.input_mode, mode) {
            return true;
        }
        if self.managed {
            let token = self
                .lifecycle
                .active_token()
                .cloned()
                .or_else(runtime_env::launch_token);
            let result = token
                .as_ref()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "managed input mode has no foreground lifecycle token",
                    )
                })
                .and_then(|token| runtime_control::set_input_mode(token, mode));
            if let Err(error) = result {
                // The local half closes immediately even if the runtime kept
                // its previous setting. No further contact may mutate paper.
                self.input_mode = InputMode::AnimationLocked;
                self.input_mode_synced = false;
                self.fail(
                    LifecycleStage::Runtime,
                    format!("could not set input mode {}: {error}", mode.as_str()),
                );
                eprintln!(
                    "magic-paper: event=input-mode-failed requested={} error={error}",
                    mode.as_str()
                );
                return false;
            }
        }
        self.input_mode = mode;
        self.input_mode_synced = true;
        eprintln!(
            "magic-paper: event=input-mode-changed mode={} ink_enabled={}",
            mode.as_str(),
            mode.ink_enabled()
        );
        true
    }

    pub(super) fn input_mode_failed(&self) -> bool {
        matches!(self.lifecycle_exit, LifecycleExit::Failed { .. })
    }
}

fn input_mode_needs_request(synced: bool, current: InputMode, requested: InputMode) -> bool {
    !synced || current != requested
}

pub(super) fn input_mode_for_state(state: &State) -> InputMode {
    match state {
        State::Listening { .. } => InputMode::Writing,
        State::Help { .. }
        | State::Conjuring { .. }
        | State::MemoryShown { .. }
        | State::TaskList { .. }
        | State::TodoList { .. }
        | State::FontList { .. }
        | State::HistoryList { .. }
        | State::ReaderList { .. } => InputMode::Modal,
        State::Drinking { .. }
        | State::Thinking { .. }
        | State::Replying { .. }
        | State::AnswerVisible { .. }
        | State::FadingReply { .. }
        | State::AwaitingPenUp => InputMode::AnimationLocked,
    }
}

fn open_devices(takeover: bool, hosted: bool) -> Devices {
    if !takeover {
        eprintln!("riddle: hosted QTFB input enabled; raw input devices left ungrabbed");
        if hosted {
            eprintln!("riddle: Remagic manager owns the power-button lifecycle");
        }
        return Devices {
            pen: None,
            touch: None,
            power: None,
        };
    }
    let pen = pen::PenDevice::open()
        .map_err(|error| eprintln!("riddle: raw pen unavailable ({error})"))
        .ok();
    let power = power::PowerButton::open()
        .map_err(|error| eprintln!("riddle: no power button ({error})"))
        .ok();
    Devices {
        pen,
        touch: touch::TouchDevice::open().ok(),
        power,
    }
}

fn open_stores() -> Stores {
    let memory = memory::MemoryStore::open();
    let tasks = tasks::TaskStore::open();
    let todos = todos::TodoStore::open();
    if let Some(store) = &memory {
        eprintln!("riddle: memory holds {} pages", store.entries.len());
    }
    if let Some(store) = &tasks {
        eprintln!(
            "magic-paper: task list holds {} recurring commands",
            store.entries.len()
        );
    }
    if let Some(store) = &todos {
        eprintln!(
            "magic-paper: TODO list holds {} entries",
            store.entries.len()
        );
    }
    Stores {
        memory,
        tasks,
        todos,
    }
}

fn log_display(disp: &display::Display, surf: &crate::surface::Surface, takeover: bool) {
    eprintln!(
        "riddle: display {} ({}x{} stride {})",
        if takeover { "quill/takeover" } else { "qtfb" },
        surf.w,
        surf.h,
        surf.stride
    );
    let _ = disp;
}

pub(super) fn clear_region(surf: &mut crate::surface::Surface, region: BBox) {
    if !region.is_empty() {
        let (x, y, w, h) = region.rect();
        surf.fill_rect(x as usize, y as usize, w as usize, h as usize, WHITE);
    }
}

pub(super) fn suspend_visible_state(
    state: &mut State,
    surf: &mut crate::surface::Surface,
    user_ink: &mut ink::Ink,
) {
    let old = std::mem::replace(state, State::Listening { last_pen: None });
    *state = match old {
        State::Drinking { region, rx, .. } => {
            rx.cancel("application-entered-background");
            clear_region(surf, region);
            user_ink.clear();
            State::Listening { last_pen: None }
        }
        State::Thinking { rx, .. } => {
            rx.cancel("application-entered-background");
            clear_region(surf, user_ink.bbox);
            user_ink.clear();
            State::Listening { last_pen: None }
        }
        State::Replying { plan, rx, .. } => {
            if let Some(rx) = rx {
                rx.cancel("application-entered-background");
            }
            clear_region(surf, plan.region);
            user_ink.clear();
            State::Listening { last_pen: None }
        }
        State::AnswerVisible { region, .. } | State::FadingReply { region, .. } => {
            clear_region(surf, region);
            State::Listening { last_pen: None }
        }
        State::AwaitingPenUp => State::Listening { last_pen: None },
        State::Conjuring { saved, .. } => {
            surf.paste_rect(0, 0, screen_w(), screen_h(), &saved);
            State::Listening { last_pen: None }
        }
        State::MemoryShown {
            saved: Some(saved), ..
        } => {
            surf.paste_rect(0, 0, screen_w(), screen_h(), &saved);
            State::Listening { last_pen: None }
        }
        State::MemoryShown { saved: None, .. } | State::Listening { .. } => {
            State::Listening { last_pen: None }
        }
        stable @ (State::Help { .. }
        | State::TaskList { .. }
        | State::TodoList { .. }
        | State::FontList { .. }
        | State::HistoryList { .. }
        | State::ReaderList { .. }) => stable,
    };
}

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;
