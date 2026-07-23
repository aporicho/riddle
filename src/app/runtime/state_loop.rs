use std::time::{Duration, Instant};

use super::super::context::build_ctx;
use super::super::oracle_controller::{cancel_speculative, idle_commit_delay, FirstTurnPoll};
use super::super::reply::{oracle_excuse, plan_reply_async, region_all_white};
use super::super::state::{State, TurnKind};
use super::super::timing::{heartbeat_deadline, heartbeat_retry_interval, unix_now};
use super::super::turn_controller::{consume_first_event, FirstEventContext};
use super::{input_mode_for_state, Engine, IDLE_PREASK};
use crate::fb::screen_h;
use crate::platform::RefreshIntent;
use crate::{agent, tasks, ui};

mod cleanup;
mod memory;
mod reply;

impl Engine<'_> {
    pub(super) fn tick_state(&mut self) {
        if let Some(status) = self.oracle.poll_agent_control() {
            super::super::pi_settings_controller::set_status(
                &mut self.state,
                &mut self.surf,
                &self.font,
                self.disp,
                status,
            );
        }
        let state = std::mem::replace(&mut self.state, State::Listening { last_pen: None });
        let next_state = match state {
            State::Listening { last_pen } => self.tick_listening(last_pen),
            State::Drinking {
                stage,
                next,
                region,
                rx,
            } => self.tick_drinking(stage, next, region, rx),
            State::Thinking { rx, since } => self.tick_thinking(rx, since),
            State::Replying {
                plan,
                next,
                rx,
                page_full,
            } => self.tick_replying(plan, next, rx, page_full),
            State::AnswerVisible { until, region } => {
                if Instant::now() >= until {
                    State::FadingReply {
                        stage: 0,
                        next: Instant::now(),
                        region,
                    }
                } else {
                    State::AnswerVisible { until, region }
                }
            }
            State::Help { panel, until } => self.tick_help(panel, until),
            State::Conjuring { plan, next, saved } => self.tick_conjuring(plan, next, saved),
            State::MemoryShown {
                saved,
                until,
                region,
            } => self.tick_memory_shown(saved, until, region),
            State::FadingReply {
                stage,
                next,
                region,
            } => self.tick_fading(stage, next, region),
            State::AwaitingPenUp => State::AwaitingPenUp,
            stable @ (State::TaskList { .. }
            | State::TodoList { .. }
            | State::FontList { .. }
            | State::Settings { .. }
            | State::PiSettings { .. }
            | State::HistoryList { .. }
            | State::ReaderList { .. }) => stable,
        };
        let mode = input_mode_for_state(&next_state);
        let mode_ready = !self.input_mode_failed() && self.set_input_mode(mode);
        self.state = if mode == crate::platform::InputMode::Writing && !mode_ready {
            State::AwaitingPenUp
        } else {
            next_state
        };
    }

    fn tick_listening(&mut self, last_pen: Option<Instant>) -> State {
        let idle = last_pen.map(|time| time.elapsed());
        if idle.is_some_and(|elapsed| {
            !self.pen_down
                && !self.stylus_tapped
                && elapsed >= idle_commit_delay(&self.speculative)
                && !self.user_ink.is_empty()
        }) {
            return self.commit_page();
        }
        if idle.is_some_and(|elapsed| {
            !self.pen_down
                && !self.stylus_tapped
                && elapsed >= IDLE_PREASK
                && !self.user_ink.is_empty()
                && !self.speculative_attempted
        }) {
            self.start_preask();
            return State::Listening { last_pen };
        }
        if self.should_poll_agent() {
            return self.poll_agent_queue(last_pen);
        }
        if self.should_run_heartbeat() {
            return self.start_heartbeat(last_pen);
        }
        State::Listening { last_pen }
    }

    fn commit_page(&mut self) -> State {
        self.speculative_attempted = false;
        if region_all_white(&self.surf, self.user_ink.bbox) {
            cancel_speculative(&mut self.speculative, "page was erased");
            self.pen_trace.finish("page-erased");
            self.user_ink.clear();
            return State::Listening { last_pen: None };
        }
        if ui::help::looks_like_question_mark(self.user_ink.stroke_list()) {
            return self.open_gesture_help();
        }
        if !self.set_input_mode(crate::platform::InputMode::AnimationLocked) {
            cancel_speculative(&mut self.speculative, "input mode lock failed");
            self.pen_trace.finish("input-mode-lock-failed");
            return State::Listening { last_pen: None };
        }
        if !self.oracle.is_available() {
            cancel_speculative(&mut self.speculative, "oracle unavailable");
            self.pen_trace.finish("oracle-unavailable");
            let y = (self.user_ink.bbox.y1 + 90).min(screen_h() as i32 - 400);
            let (x, ink_y, width, height) = self.user_ink.bbox.rect();
            self.surf.fill_rect(
                x as usize,
                ink_y as usize,
                width as usize,
                height as usize,
                crate::surface::WHITE,
            );
            self.disp
                .present_region(x, ink_y, width, height, RefreshIntent::Content);
            self.user_ink.clear();
            return State::Replying {
                plan: plan_reply_async(&self.font, &oracle_excuse("no oracle"), Some(y)),
                next: Instant::now(),
                rx: None,
                page_full: false,
            };
        }
        self.start_user_turn()
    }

    fn open_gesture_help(&mut self) -> State {
        cancel_speculative(&mut self.speculative, "guide gesture");
        self.pen_trace.finish("guide-gesture");
        let (x, y, width, height) = self.user_ink.bbox.rect();
        self.surf.fill_rect(
            x as usize,
            y as usize,
            width as usize,
            height as usize,
            crate::surface::WHITE,
        );
        self.disp
            .present_region(x, y, width, height, RefreshIntent::Content);
        self.user_ink.clear();
        let panel = ui::help::show(&mut self.surf, &self.font, self.takeover);
        let (x, y, width, height) = panel.region.rect();
        self.disp
            .present_region(x, y, width, height, RefreshIntent::Ui);
        eprintln!("magicpaper: guide shown");
        State::Help {
            panel: Some(panel),
            until: Instant::now() + Duration::from_secs(45),
        }
    }

    fn start_user_turn(&mut self) -> State {
        self.turn_id = self
            .store
            .as_ref()
            .map(|store| store.next_id(unix_now()))
            .unwrap_or_else(unix_now);
        self.turn_strokes = self.user_ink.stroke_list().to_vec();
        self.turn_reply.clear();
        self.turn_transcript = None;
        self.turn_failed = false;
        self.turn_kind = TurnKind::User;
        self.turn_tasks.clear();
        let pen_session = self.pen_trace.finish("idle-commit").unwrap_or(0);
        let (request, speculative) = match self.speculative.take() {
            Some(request) => (request, true),
            None => {
                let capture = self
                    .user_ink
                    .capture(&self.surf)
                    .expect("nonempty committed ink has a capture");
                let request = self
                    .oracle
                    .ask_capture(
                        capture,
                        &build_ctx(&self.store, &self.task_store, &self.todo_store),
                    )
                    .expect("oracle was checked before committing the page");
                (request, false)
            }
        };
        eprintln!(
            "magic-paper: event=idle-commit request={} session={pen_session} speculative={speculative} strokes={}",
            request.request_id(),
            self.turn_strokes.len()
        );
        State::Drinking {
            stage: 0,
            next: Instant::now(),
            region: self.user_ink.bbox,
            rx: request,
        }
    }

    fn start_preask(&mut self) {
        self.speculative_attempted = true;
        if region_all_white(&self.surf, self.user_ink.bbox)
            || ui::help::looks_like_question_mark(self.user_ink.stroke_list())
            || !self.oracle.supports_speculative()
        {
            return;
        }
        match self.user_ink.capture(&self.surf) {
            Ok(capture) => {
                if let Some(request) = self.oracle.ask_speculative_capture(
                    capture,
                    &build_ctx(&self.store, &self.task_store, &self.todo_store),
                ) {
                    eprintln!(
                        "magic-paper: event=ocr-preask request={} idle_ms={}",
                        request.request_id(),
                        IDLE_PREASK.as_millis(),
                    );
                    self.speculative = Some(request);
                }
            }
            Err(error) => {
                eprintln!("magicpaper: speculative rasterize failed; will retry at commit: {error}")
            }
        }
    }

    fn should_poll_agent(&self) -> bool {
        self.agent_queue_mode
            && !self.pen_down
            && !self.stylus_tapped
            && self.user_ink.is_empty()
            && Instant::now() >= self.next_agent_poll
    }

    fn poll_agent_queue(&mut self, last_pen: Option<Instant>) -> State {
        self.next_agent_poll = Instant::now() + Duration::from_secs(1);
        match agent::take_pending() {
            Ok(Some(reply)) => {
                eprintln!("magic-paper: showing queued scheduled result");
                if !self.set_input_mode(crate::platform::InputMode::AnimationLocked) {
                    return State::Listening { last_pen: None };
                }
                self.turn_kind = TurnKind::User;
                self.turn_reply.clear();
                State::Replying {
                    plan: plan_reply_async(&self.font, &reply, None),
                    next: Instant::now(),
                    rx: None,
                    page_full: false,
                }
            }
            Ok(None) => State::Listening { last_pen },
            Err(error) => {
                eprintln!("magic-paper: could not read agent queue: {error}");
                State::Listening { last_pen }
            }
        }
    }

    fn should_run_heartbeat(&self) -> bool {
        !self.pen_down
            && !self.stylus_tapped
            && self.user_ink.is_empty()
            && !self.hosted
            && self
                .next_heartbeat
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn start_heartbeat(&mut self, last_pen: Option<Instant>) -> State {
        let due = self
            .task_store
            .as_ref()
            .map(|store| store.due(unix_now()))
            .unwrap_or_default();
        if due.is_empty() {
            self.next_heartbeat = heartbeat_deadline(&self.task_store);
            return State::Listening { last_pen };
        }
        if !self.oracle.is_available() {
            self.next_heartbeat = Some(Instant::now() + heartbeat_retry_interval());
            eprintln!("magic-paper: heartbeat postponed — oracle unavailable");
            return State::Listening { last_pen };
        }
        let lease = match tasks::acquire_scheduler_lease() {
            Ok(lease) => lease,
            Err(error) => {
                self.next_heartbeat = Some(Instant::now() + heartbeat_retry_interval());
                eprintln!("magic-paper: heartbeat delegated to external agent: {error}");
                return State::Listening { last_pen };
            }
        };
        self.begin_heartbeat(due, lease)
    }

    fn begin_heartbeat(&mut self, due: Vec<tasks::Task>, lease: tasks::SchedulerLease) -> State {
        if !self.set_input_mode(crate::platform::InputMode::AnimationLocked) {
            return State::Listening { last_pen: None };
        }
        self.ui_scheduler_lease = Some(lease);
        self.next_heartbeat = Some(Instant::now() + heartbeat_retry_interval());
        self.turn_id = 0;
        self.turn_strokes.clear();
        self.turn_reply.clear();
        self.turn_transcript = None;
        self.turn_failed = false;
        self.turn_kind = TurnKind::Heartbeat;
        let prompt = tasks::heartbeat_prompt(&due);
        self.turn_tasks = due;
        let rx = self
            .oracle
            .ask_text(
                &prompt,
                &build_ctx(&self.store, &self.task_store, &self.todo_store),
            )
            .expect("oracle was checked before heartbeat");
        eprintln!(
            "magic-paper: heartbeat — executing {} due task(s)",
            self.turn_tasks.len()
        );
        State::Thinking {
            rx,
            since: Instant::now(),
        }
    }

    fn tick_thinking(
        &mut self,
        rx: super::super::oracle_controller::OracleTurn,
        since: Instant,
    ) -> State {
        match rx.poll_first(since) {
            FirstTurnPoll::Event(result) => consume_first_event(
                result,
                rx,
                since,
                FirstEventContext {
                    font: &self.font,
                    memory_store: &self.store,
                    task_store: &mut self.task_store,
                    todo_store: &mut self.todo_store,
                    next_heartbeat: &mut self.next_heartbeat,
                    surface: &mut self.surf,
                    display: self.disp,
                    refresh: &mut self.refresh,
                    takeover: self.takeover,
                    turn_transcript: &mut self.turn_transcript,
                    turn_reply: &mut self.turn_reply,
                    turn_failed: &mut self.turn_failed,
                    turn_kind: self.turn_kind,
                },
            ),
            FirstTurnPoll::Pending => State::Thinking { rx, since },
            FirstTurnPoll::TimedOut { request_id } => {
                eprintln!(
                    "magic-paper: event=turn-error request={request_id} stage=timeout timeout_seconds=120"
                );
                State::Replying {
                    plan: plan_reply_async(&self.font, &oracle_excuse("timed out"), None),
                    next: Instant::now(),
                    rx: None,
                    page_full: false,
                }
            }
            FirstTurnPoll::Closed { request_id } => {
                eprintln!(
                    "magic-paper: event=turn-stream-closed request={request_id} phase=thinking"
                );
                if self.turn_kind == TurnKind::Heartbeat {
                    self.ui_scheduler_lease = None;
                }
                State::Listening { last_pen: None }
            }
            FirstTurnPoll::Stale { request_id } => {
                eprintln!(
                    "magic-paper: event=turn-discarded request={request_id} reason=stale-generation"
                );
                State::Listening { last_pen: None }
            }
        }
    }

    fn tick_help(&mut self, panel: Option<ui::help::Help>, until: Instant) -> State {
        match panel {
            Some(panel)
                if Instant::now() >= until && !self.stylus_on && self.modal_contact.is_none() =>
            {
                let region = panel.dismiss(&mut self.surf);
                let (x, y, width, height) = region.rect();
                self.disp
                    .present_region(x, y, width, height, RefreshIntent::Ui);
                eprintln!("magicpaper: guide dismissed");
                State::Help { panel: None, until }
            }
            Some(panel) => State::Help {
                panel: Some(panel),
                until,
            },
            None if self.stylus_on => State::Help { panel: None, until },
            None => State::Listening { last_pen: None },
        }
    }

    pub(super) fn wait_for_next_tick(&self) {
        let wait = match &self.state {
            State::Listening { last_pen: None }
            | State::TaskList { .. }
            | State::TodoList { .. }
            | State::FontList { .. }
            | State::Settings { .. }
            | State::PiSettings { .. }
            | State::HistoryList { .. }
            | State::ReaderList { .. } => Duration::from_millis(25),
            State::Thinking { .. } => Duration::from_millis(4),
            _ => Duration::from_millis(2),
        };
        self.disp.wait(wait, !self.ink_dirty.is_empty());
    }
}
