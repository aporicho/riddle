use std::time::Instant;

mod modal;

use super::super::input::{gate_pen_frame, PenGate, QtfbPenTransition};
use super::super::lifecycle::LifecycleStage;
use super::super::oracle_controller::cancel_speculative;
use super::super::state::{State, TurnKind};
use super::Engine;
use crate::fb::BBox;
use crate::platform::{DamageRect, PenFrame, PenPhase, PenTool, RefreshIntent};
use crate::ui::pointer::PointerTool;
use crate::{pen, qtfb};

impl Engine<'_> {
    pub(super) fn touch_requested_quit(&mut self) -> bool {
        let quit = self
            .touch_dev
            .as_mut()
            .is_some_and(|device| device.drain_check_quit());
        if quit {
            eprintln!("magicpaper: 5-finger quit");
        }
        quit
    }

    pub(super) fn drain_raw_pen(&mut self) {
        let samples = self
            .pen_dev
            .as_mut()
            .map(|device| device.drain())
            .unwrap_or_default();
        for sample in samples {
            if !self.handle_raw_sample(sample) {
                break;
            }
        }
    }

    fn handle_raw_sample(&mut self, sample: pen::PenSample) -> bool {
        let writing = sample.touching && sample.pressure > 40;
        let phase = if writing {
            if self.pen_down {
                PenPhase::Move
            } else {
                PenPhase::Down
            }
        } else {
            PenPhase::Up
        };
        let frame = sample.to_frame(self.pen_sequence.next(), phase);
        self.stylus_on = writing;
        self.stylus_tapped |= writing;
        if !writing {
            self.update_modal_contact(PointerTool::Pen, frame.x, frame.y);
            return self.finish_pen_contact();
        }
        let radius = 2 + frame.pressure as i32 * 3 / pen::MAX_PRESSURE;
        match self.prepare_pen_frame(frame) {
            Ok(true) => self.apply_frame(frame, "raw", radius),
            Ok(false) => true,
            Err(()) => false,
        }
    }

    /// Return true when this frame may reach the page/modal handler, false
    /// when it is intentionally swallowed, and Err when fail-closed mode
    /// negotiation requires the application loop to stop.
    fn prepare_pen_frame(&mut self, frame: PenFrame) -> Result<bool, ()> {
        self.pen_down = true;
        let action = gate_pen_frame(
            self.input_mode,
            frame,
            matches!(self.state, State::AnswerVisible { .. }),
            self.turn_kind == TurnKind::Heartbeat
                && matches!(
                    self.state,
                    State::Thinking { .. }
                        | State::Replying { .. }
                        | State::AnswerVisible { .. }
                        | State::FadingReply { .. }
                ),
        );
        match action {
            PenGate::Apply => {
                self.input_priority.begin_pen();
                Ok(true)
            }
            PenGate::Ignore => Ok(false),
            PenGate::FadeAnswer => {
                self.begin_answer_fade();
                Ok(false)
            }
            PenGate::CancelHeartbeat => {
                if self.cancel_heartbeat_for_pen() {
                    self.input_priority.begin_pen();
                    Ok(true)
                } else {
                    Err(())
                }
            }
        }
    }

    fn begin_answer_fade(&mut self) {
        let old = std::mem::replace(&mut self.state, State::AwaitingPenUp);
        self.state = match old {
            State::AnswerVisible { region, .. } => {
                eprintln!("magic-paper: event=answer-fade-trigger source=fresh-pen-down");
                State::FadingReply {
                    stage: 0,
                    next: Instant::now(),
                    region,
                }
            }
            other => other,
        };
    }

    fn cancel_heartbeat_for_pen(&mut self) -> bool {
        let old = std::mem::replace(&mut self.state, State::AwaitingPenUp);
        let (request, region) = match old {
            State::Thinking { rx, .. } => (Some(rx), None),
            State::Replying { plan, rx, .. } => (rx, Some(plan.region)),
            State::AnswerVisible { region, .. } | State::FadingReply { region, .. } => {
                (None, Some(region))
            }
            other => {
                self.state = other;
                return true;
            }
        };
        if let Some(request) = request {
            request.cancel("heartbeat-preempted-by-pen");
        }
        if let Some(region) = region.filter(|region| !region.is_empty()) {
            let (x, y, width, height) = region.rect();
            self.surf.fill_rect(
                x as usize,
                y as usize,
                width as usize,
                height as usize,
                crate::surface::WHITE,
            );
            self.disp
                .present_region(x, y, width, height, RefreshIntent::MonoQuality);
        }
        self.ui_scheduler_lease = None;
        self.turn_kind = TurnKind::User;
        self.turn_tasks.clear();
        self.turn_strokes.clear();
        self.turn_reply.clear();
        self.turn_transcript = None;
        self.turn_failed = false;
        self.user_ink.clear();
        if !self.set_input_mode(crate::platform::InputMode::Writing) {
            return false;
        }
        self.state = State::Listening { last_pen: None };
        eprintln!("magic-paper: event=heartbeat-cancelled reason=fresh-pen-down");
        true
    }

    fn apply_frame(&mut self, frame: PenFrame, source: &'static str, radius: i32) -> bool {
        if self.input_mode == crate::platform::InputMode::Modal {
            self.record_modal_pen(frame);
            return true;
        }
        if let State::Listening { last_pen } = &mut self.state {
            cancel_speculative(&mut self.speculative, "writing resumed");
            self.speculative_attempted = false;
            self.pen_down = true;
            let damage = draw_page_point(&mut self.user_ink, &mut self.surf, frame, radius);
            // Draw before starting/logging telemetry so stderr backpressure can
            // never delay the first local ink mutation.
            self.pen_trace.begin(source, frame.x, frame.y);
            add_damage(&mut self.ink_dirty, damage);
            self.ink_flush_urgent |= self.pen_trace.ink_changed();
            *last_pen = Some(Instant::now());
        }
        true
    }

    fn finish_pen_contact(&mut self) -> bool {
        self.input_priority.end_pen();
        let was_down = std::mem::replace(&mut self.pen_down, false);
        self.stylus_on = false;
        if self
            .modal_contact
            .as_ref()
            .is_some_and(|contact| contact.tool == PointerTool::Pen)
        {
            return self.finish_modal_contact(PointerTool::Pen);
        }
        if is_list(&self.state) {
            return true;
        }
        if matches!(self.state, State::AwaitingPenUp) {
            if !self.set_input_mode(crate::platform::InputMode::Writing) {
                return false;
            }
            self.state = State::Listening { last_pen: None };
            eprintln!("magic-paper: event=answer-fade-contact-released writing_reenabled=true");
            return true;
        }
        if was_down && matches!(self.state, State::Listening { .. }) {
            let settled = self.user_ink.pen_up(&mut self.surf);
            if !settled.is_empty() {
                add_damage(&mut self.ink_dirty, settled);
                let (x, y, width, height) = settled.rect();
                self.disp
                    .present_region(x, y, width, height, RefreshIntent::MonoQuality);
            }
            if let State::Listening { last_pen } = &mut self.state {
                *last_pen = Some(Instant::now());
            }
        }
        true
    }

    pub(super) fn pump_host_events(&mut self) -> bool {
        let events = match self.disp.pump() {
            Ok(events) => events,
            Err(error) => {
                self.fail(
                    LifecycleStage::Runtime,
                    format!("display/input host disconnected: {error}"),
                );
                return false;
            }
        };
        for event in events {
            if is_pen_event(event.input_type) {
                if !self.handle_host_pen(event) {
                    break;
                }
            } else if !self.handle_touch(event) {
                break;
            }
        }
        true
    }

    fn handle_host_pen(&mut self, event: qtfb::InputEvent) -> bool {
        let transition =
            self.qtfb_pen
                .transition(event.input_type, event.pressure_percent(), Instant::now());
        let (frame, recovered_press) = match transition {
            QtfbPenTransition::Hover => return true,
            QtfbPenTransition::Release {
                was_down,
                recovered,
            } => {
                self.trace_host_release(&event, recovered);
                if was_down {
                    self.update_modal_contact(PointerTool::Pen, event.x, event.y);
                    return self.finish_forwarded_pen();
                }
                return true;
            }
            QtfbPenTransition::Draw {
                close_orphan,
                recovered_press,
            } => {
                if close_orphan
                    && orphan_recovery_may_release(&self.state)
                    && !self.close_orphaned_pen()
                {
                    return false;
                }
                let phase = if recovered_press {
                    PenPhase::Down
                } else {
                    PenPhase::Move
                };
                (
                    event.to_pen_frame(self.pen_sequence.next(), phase),
                    recovered_press,
                )
            }
        };
        self.stylus_on = true;
        self.stylus_tapped = true;
        match self.prepare_pen_frame(frame) {
            Ok(true) => {
                let keep_running =
                    self.apply_frame(frame, "qtfb", 2 + event.pressure_percent() / 45);
                self.pen_trace.qtfb_edge(event.input_type, recovered_press);
                keep_running
            }
            Ok(false) => true,
            Err(()) => false,
        }
    }

    fn trace_host_release(&mut self, event: &qtfb::InputEvent, recovered: bool) {
        let _ = event.to_pen_frame(self.pen_sequence.next(), PenPhase::Up);
        self.pen_trace.qtfb_edge(event.input_type, false);
        if recovered {
            self.pen_trace.recovered_release();
            eprintln!("magic-paper: event=pen-release-recovered source=qtfb reason=pressure-zero");
        }
    }

    fn close_orphaned_pen(&mut self) -> bool {
        self.pen_trace.recovered_release();
        eprintln!(
            "magic-paper: event=pen-release-recovered source=qtfb reason=new-pressure-after-gap"
        );
        self.finish_forwarded_pen()
    }

    fn finish_forwarded_pen(&mut self) -> bool {
        self.finish_pen_contact()
    }

    fn handle_touch(&mut self, event: qtfb::InputEvent) -> bool {
        match event.input_type {
            qtfb::INPUT_TOUCH_PRESS => {
                if self.primary_touch.is_none() {
                    self.primary_touch = Some(event.dev_id);
                    self.stylus_on = true;
                    self.stylus_tapped = true;
                    if self.input_mode == crate::platform::InputMode::Modal {
                        self.begin_modal_contact(PointerTool::Finger, event.x, event.y);
                    }
                }
            }
            qtfb::INPUT_TOUCH_UPDATE if self.primary_touch == Some(event.dev_id) => {
                self.update_modal_contact(PointerTool::Finger, event.x, event.y);
            }
            qtfb::INPUT_TOUCH_RELEASE if self.primary_touch == Some(event.dev_id) => {
                self.primary_touch = None;
                self.stylus_on = false;
                self.update_modal_contact(PointerTool::Finger, event.x, event.y);
                if !self.finish_modal_contact(PointerTool::Finger) {
                    return false;
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn open_reader_target(&mut self) {
        if let Some(path) = self.reader_target.take() {
            self.state = super::super::turn_controller::request_reader(&self.font, &path);
        }
    }

    pub(super) fn flush_live_ink(&mut self) -> bool {
        if self.ink_dirty.is_empty()
            || (!self.ink_flush_urgent && self.last_flush.elapsed() < self.flush_every)
        {
            return true;
        }
        let (x, y, width, height) = self.ink_dirty.rect();
        match self.live_ink.present_damage(DamageRect {
            x,
            y,
            width,
            height,
        }) {
            Ok(()) => {
                self.ink_dirty = BBox::empty();
                self.last_flush = Instant::now();
                self.ink_flush_urgent = false;
                self.pen_trace.presented();
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                self.ink_flush_urgent = true;
                true
            }
            Err(error) => {
                self.fail(
                    LifecycleStage::Runtime,
                    format!("live-ink transport failed: {error}"),
                );
                false
            }
        }
    }
}

/// A time gap is useful for recovering an ordinary lost QTFB release, but it
/// is not physical proof that the contact which dismissed an answer went Up.
/// That safety barrier may be released only by the host's actual Up/zero frame.
pub(super) fn orphan_recovery_may_release(state: &State) -> bool {
    !matches!(state, State::AwaitingPenUp)
}

fn is_pen_event(input_type: i32) -> bool {
    matches!(
        input_type,
        qtfb::INPUT_PEN_PRESS | qtfb::INPUT_PEN_UPDATE | qtfb::INPUT_PEN_RELEASE
    )
}

fn is_list(state: &State) -> bool {
    matches!(
        state,
        State::TaskList { .. }
            | State::TodoList { .. }
            | State::HistoryList { .. }
            | State::FontList { .. }
            | State::Settings { .. }
            | State::PiSettings { .. }
            | State::ReaderList { .. }
    )
}

fn add_damage(target: &mut BBox, damage: BBox) {
    if !damage.is_empty() {
        target.add(damage.x0, damage.y0, 0);
        target.add(damage.x1, damage.y1, 0);
    }
}

fn draw_page_point(
    ink: &mut crate::ink::Ink,
    surface: &mut crate::surface::Surface,
    frame: PenFrame,
    radius: i32,
) -> BBox {
    match frame.tool {
        PenTool::Pen => ink.pen_point(surface, frame.x, frame.y, radius),
        PenTool::Eraser => ink.erase_point(surface, frame.x, frame.y, 22),
    }
}
