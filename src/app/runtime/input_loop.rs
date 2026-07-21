use std::time::Instant;

use super::super::input::QtfbPenTransition;
use super::super::lifecycle::LifecycleStage;
use super::super::lists::{finish_paper_list_stroke, PaperListContext};
use super::super::oracle_controller::cancel_speculative;
use super::super::state::{State, TurnKind};
use super::Engine;
use crate::fb::BBox;
use crate::platform::{DamageRect, PenFrame, PenPhase, PenTool, RefreshIntent};
use crate::{pen, qtfb};

impl Engine<'_> {
    pub(super) fn touch_requested_quit(&mut self) -> bool {
        let quit = self
            .touch_dev
            .as_mut()
            .is_some_and(|device| device.drain_check_quit());
        if quit {
            eprintln!("riddle: 5-finger quit");
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
            return self.finish_pen_contact();
        }
        self.begin_user_input();
        let radius = 2 + frame.pressure as i32 * 3 / pen::MAX_PRESSURE;
        self.apply_frame(frame, "raw", radius)
    }

    fn begin_user_input(&mut self) {
        if self.input_priority.begin_pen(
            &mut self.state,
            self.turn_kind,
            &mut self.surf,
            self.disp,
            &mut self.user_ink,
        ) {
            self.ui_scheduler_lease = None;
            self.turn_kind = TurnKind::User;
            self.turn_tasks.clear();
            self.turn_reply.clear();
            self.turn_transcript = None;
        }
    }

    fn apply_frame(&mut self, frame: PenFrame, source: &'static str, radius: i32) -> bool {
        if matches!(
            self.state,
            State::Listening { .. } | State::Lingering { .. } | State::FadingReply { .. }
        ) {
            self.pen_trace.begin(source, frame.x, frame.y);
        }
        match &mut self.state {
            State::TaskList { panel }
            | State::TodoList { panel }
            | State::HistoryList { panel }
            | State::ReaderList { panel, .. } => {
                self.pen_down = true;
                add_damage(
                    &mut self.ink_dirty,
                    panel.pen_point(&mut self.surf, frame.x, frame.y),
                );
            }
            State::FontList { panel } => {
                self.pen_down = true;
                add_damage(
                    &mut self.ink_dirty,
                    panel.pen_point(&mut self.surf, frame.x, frame.y),
                );
            }
            State::Listening { last_pen } => {
                cancel_speculative(&mut self.speculative, "writing resumed");
                self.speculative_attempted = false;
                self.pen_down = true;
                let damage = draw_page_point(&mut self.user_ink, &mut self.surf, frame, radius);
                add_damage(&mut self.ink_dirty, damage);
                self.ink_flush_urgent |= self.pen_trace.ink_changed();
                *last_pen = Some(Instant::now());
            }
            State::Lingering { region, .. } | State::FadingReply { region, .. } => {
                let (x, y, w, h) = region.rect();
                self.surf.fill_rect(
                    x as usize,
                    y as usize,
                    w as usize,
                    h as usize,
                    crate::surface::WHITE,
                );
                self.disp.present_region(x, y, w, h, RefreshIntent::Ink);
                self.pen_down = true;
                let damage = draw_page_point(&mut self.user_ink, &mut self.surf, frame, radius);
                add_damage(&mut self.ink_dirty, damage);
                self.ink_flush_urgent |= self.pen_trace.ink_changed();
                self.state = State::Listening {
                    last_pen: Some(Instant::now()),
                };
            }
            _ => {}
        }
        true
    }

    fn finish_pen_contact(&mut self) -> bool {
        self.input_priority.end_pen();
        if is_list(&self.state) {
            self.pen_down = false;
            return !self.finish_list_stroke();
        }
        if self.pen_down {
            self.pen_down = false;
            self.user_ink.pen_up();
            if let State::Listening { last_pen } = &mut self.state {
                *last_pen = Some(Instant::now());
            }
        }
        true
    }

    fn finish_list_stroke(&mut self) -> bool {
        let path = finish_paper_list_stroke(
            &mut self.state,
            PaperListContext {
                memory_store: &mut self.store,
                task_store: &mut self.task_store,
                todo_store: &mut self.todo_store,
                next_heartbeat: &mut self.next_heartbeat,
                surf: &mut self.surf,
                font: &mut self.font,
                disp: self.disp,
            },
        );
        if let Some(path) = path {
            self.reader_target = Some(path);
            true
        } else {
            false
        }
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
                    self.pen_down = false;
                    return self.finish_forwarded_pen();
                }
                return true;
            }
            QtfbPenTransition::Draw {
                close_orphan,
                recovered_press,
            } => {
                if close_orphan && !self.close_orphaned_pen() {
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
        self.begin_user_input();
        self.stylus_on = true;
        self.stylus_tapped = true;
        if matches!(
            self.state,
            State::Listening { .. } | State::Lingering { .. } | State::FadingReply { .. }
        ) {
            self.pen_trace.begin("qtfb", frame.x, frame.y);
        }
        self.pen_trace.qtfb_edge(event.input_type, recovered_press);
        self.apply_frame(frame, "qtfb", 2 + event.pressure_percent() / 45)
    }

    fn trace_host_release(&mut self, event: &qtfb::InputEvent, recovered: bool) {
        let _ = event.to_pen_frame(self.pen_sequence.next(), PenPhase::Up);
        self.pen_trace.qtfb_edge(event.input_type, false);
        if recovered {
            self.pen_trace.recovered_release();
            eprintln!("magic-paper: event=pen-release-recovered source=qtfb reason=pressure-zero");
        }
        self.input_priority.end_pen();
        self.stylus_on = false;
    }

    fn close_orphaned_pen(&mut self) -> bool {
        self.input_priority.end_pen();
        self.pen_trace.recovered_release();
        eprintln!(
            "magic-paper: event=pen-release-recovered source=qtfb reason=new-pressure-after-gap"
        );
        self.pen_down = false;
        self.stylus_on = false;
        self.finish_forwarded_pen()
    }

    fn finish_forwarded_pen(&mut self) -> bool {
        if is_list(&self.state) {
            return !self.finish_list_stroke();
        }
        self.user_ink.pen_up();
        if let State::Listening { last_pen } = &mut self.state {
            *last_pen = Some(Instant::now());
        }
        true
    }

    fn handle_touch(&mut self, event: qtfb::InputEvent) -> bool {
        match event.input_type {
            qtfb::INPUT_TOUCH_PRESS => {
                if self.primary_touch.is_none() {
                    self.primary_touch = Some(event.dev_id);
                    self.stylus_on = true;
                    self.stylus_tapped = true;
                }
                if self.primary_touch == Some(event.dev_id) {
                    self.panel_point(event.x, event.y);
                }
            }
            qtfb::INPUT_TOUCH_UPDATE if self.primary_touch == Some(event.dev_id) => {
                self.panel_point(event.x, event.y);
            }
            qtfb::INPUT_TOUCH_RELEASE if self.primary_touch == Some(event.dev_id) => {
                self.primary_touch = None;
                self.stylus_on = false;
                if is_list(&self.state) && self.finish_list_stroke() {
                    return false;
                }
            }
            _ => {}
        }
        true
    }

    fn panel_point(&mut self, x: i32, y: i32) {
        let damage = match &mut self.state {
            State::TaskList { panel }
            | State::TodoList { panel }
            | State::HistoryList { panel }
            | State::ReaderList { panel, .. } => panel.pen_point(&mut self.surf, x, y),
            State::FontList { panel } => panel.pen_point(&mut self.surf, x, y),
            _ => BBox::empty(),
        };
        add_damage(&mut self.ink_dirty, damage);
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
