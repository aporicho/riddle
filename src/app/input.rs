//! Pen-session normalization, qtfb recovery, and the user-input priority
//! boundary.  Keeping these policies out of `runtime` prevents display/network
//! details from becoming part of input state.

use std::time::{Duration, Instant};

use crate::domain::{self, AppEvent};
use crate::fonts::FontBook;
use crate::platform::{InputMode, PenFrame, PenPhase, PenTool};
use crate::qtfb;
use crate::surface::Surface;
use crate::ui::pointer::{Gesture, GesturePolicy, HitRect, Point, PointerTool, PreviewBacking};
use crate::ui::{font_settings, help, paper_list};

/// A new pressure-bearing event after this silence closes an orphaned qtfb
/// stroke before opening the next one. Elapsed time alone never releases a
/// stationary pen.
const QTFB_ORPHAN_GAP: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QtfbPenTransition {
    Hover,
    Release {
        was_down: bool,
        recovered: bool,
    },
    Draw {
        close_orphan: bool,
        recovered_press: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PenGate {
    Apply,
    Ignore,
    FadeAnswer,
    CancelHeartbeat,
}

pub(super) enum ModalPreview {
    Paper(paper_list::Preview),
    Font(font_settings::Preview),
    Help(help::HelpPreview),
}

impl ModalPreview {
    fn rect(&self) -> HitRect {
        match self {
            Self::Paper(preview) => preview.rect(),
            Self::Font(preview) => preview.rect(),
            Self::Help(preview) => preview.rect(),
        }
    }

    fn is_visible(&self) -> bool {
        match self {
            Self::Paper(preview) => preview.is_visible(),
            Self::Font(preview) => preview.is_visible(),
            Self::Help(preview) => preview.is_visible(),
        }
    }

    fn update(&mut self, point: Point) -> bool {
        match self {
            Self::Paper(preview) => preview.update(point),
            Self::Font(preview) => preview.update(point),
            Self::Help(preview) => preview.update(point),
        }
    }

    fn render(&self, surface: &mut Surface, fonts: &FontBook) {
        match self {
            Self::Paper(preview) => preview.render(surface),
            Self::Font(preview) => preview.render(surface, fonts),
            Self::Help(preview) => preview.render(surface),
        }
    }

    fn release_gesture(self, end: Point, classified: Option<Gesture>) -> Gesture {
        match self {
            Self::Paper(preview) => preview.release_gesture(end, classified),
            Self::Font(preview) => preview.release_gesture(end),
            Self::Help(preview) => preview.release_gesture(end),
        }
    }
}

pub(super) struct ActiveModalPreview {
    target: ModalPreview,
    backing: PreviewBacking,
}

impl ActiveModalPreview {
    fn new(target: ModalPreview, surface: &mut Surface, fonts: &FontBook) -> Option<Self> {
        let backing = PreviewBacking::capture(surface, target.rect())?;
        target.render(surface, fonts);
        Some(Self { target, backing })
    }

    fn initial_damage(&self) -> Option<HitRect> {
        self.target.is_visible().then(|| self.backing.rect())
    }

    fn update(&mut self, point: Point, surface: &mut Surface, fonts: &FontBook) -> Option<HitRect> {
        if !self.target.update(point) {
            return None;
        }
        self.backing.restore(surface);
        self.target.render(surface, fonts);
        Some(self.backing.rect())
    }

    fn finish(
        self,
        end: Point,
        classified: Option<Gesture>,
        surface: &mut Surface,
    ) -> (Gesture, HitRect) {
        self.backing.restore(surface);
        (
            self.target.release_gesture(end, classified),
            self.backing.rect(),
        )
    }
}

pub(super) struct ModalContact {
    pub(super) tool: PointerTool,
    pub(super) points: Vec<Point>,
    pub(super) started_at: Instant,
    preview: Option<ActiveModalPreview>,
}

impl ModalContact {
    pub(super) fn begin(tool: PointerTool, x: i32, y: i32) -> Self {
        Self {
            tool,
            points: vec![Point::new(x, y)],
            started_at: Instant::now(),
            preview: None,
        }
    }

    pub(super) fn push(&mut self, tool: PointerTool, x: i32, y: i32) -> bool {
        if self.tool != tool {
            return false;
        }
        self.points.push(Point::new(x, y));
        true
    }

    pub(super) fn classify(self) -> Option<Gesture> {
        GesturePolicy::default().classify(self.tool, &self.points, self.started_at.elapsed())
    }

    pub(super) fn last_point(&self) -> Point {
        self.points
            .last()
            .copied()
            .unwrap_or_else(|| Point::new(0, 0))
    }

    pub(super) fn classified(&self) -> Option<Gesture> {
        GesturePolicy::default().classify(self.tool, &self.points, self.started_at.elapsed())
    }

    pub(super) fn attach_preview(
        &mut self,
        target: ModalPreview,
        surface: &mut Surface,
        fonts: &FontBook,
    ) -> Option<HitRect> {
        let preview = ActiveModalPreview::new(target, surface, fonts)?;
        let damage = preview.initial_damage();
        self.preview = Some(preview);
        damage
    }

    pub(super) fn update_preview(
        &mut self,
        point: Point,
        surface: &mut Surface,
        fonts: &FontBook,
    ) -> Option<HitRect> {
        self.preview.as_mut()?.update(point, surface, fonts)
    }

    pub(super) fn finish_preview(&mut self, surface: &mut Surface) -> Option<(Gesture, HitRect)> {
        let classified = self.classified();
        let end = self.last_point();
        self.preview
            .take()
            .map(|preview| preview.finish(end, classified, surface))
    }
}

/// Decide a contact before it can mutate the page. Locked modes continue to
/// receive events, but only a fresh marker-tip Down has semantic meaning.
pub(super) fn gate_pen_frame(
    mode: InputMode,
    frame: PenFrame,
    answer_visible: bool,
    heartbeat_in_flight: bool,
) -> PenGate {
    if mode != InputMode::AnimationLocked {
        return PenGate::Apply;
    }
    let fresh_pen_down = frame.tool == PenTool::Pen && frame.phase == PenPhase::Down;
    if heartbeat_in_flight && fresh_pen_down {
        PenGate::CancelHeartbeat
    } else if answer_visible && fresh_pen_down {
        PenGate::FadeAnswer
    } else {
        PenGate::Ignore
    }
}

/// Stateful repair for the qtfb v1 stream. It turns pressure-zero updates into
/// releases and splits a new pressured frame from an old stroke whose release
/// was lost by the host.
#[derive(Default)]
pub(super) struct QtfbPenState {
    down: bool,
    last_pressure_event: Option<Instant>,
}

impl QtfbPenState {
    /// Start a fresh foreground input epoch. Host events queued while the app
    /// was parked must not inherit contact state from the previous lease.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn transition(
        &mut self,
        input_type: i32,
        pressure: i32,
        now: Instant,
    ) -> QtfbPenTransition {
        if input_type == qtfb::INPUT_PEN_RELEASE
            || (input_type == qtfb::INPUT_PEN_UPDATE && pressure == 0 && self.down)
        {
            let was_down = self.down;
            self.down = false;
            self.last_pressure_event = None;
            return QtfbPenTransition::Release {
                was_down,
                recovered: input_type == qtfb::INPUT_PEN_UPDATE,
            };
        }
        if input_type == qtfb::INPUT_PEN_UPDATE && pressure == 0 {
            return QtfbPenTransition::Hover;
        }
        let close_orphan = self.down
            && self
                .last_pressure_event
                .is_some_and(|last| now.saturating_duration_since(last) >= QTFB_ORPHAN_GAP);
        let recovered_press = !self.down || close_orphan;
        self.down = true;
        self.last_pressure_event = Some(now);
        QtfbPenTransition::Draw {
            close_orphan,
            recovered_press,
        }
    }
}

#[derive(Default)]
pub(super) struct PenSequence(u64);

impl PenSequence {
    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1).max(1);
        self.0
    }
}

#[derive(Default)]
pub(super) struct PenTrace {
    next_id: u64,
    active: Option<ActivePenTrace>,
}

struct ActivePenTrace {
    id: u64,
    source: &'static str,
    started: Instant,
    first_ink: Option<Instant>,
    presented: bool,
    presses: u32,
    updates: u32,
    releases: u32,
    recovered_press: u32,
    recovered_release: u32,
}

impl PenTrace {
    pub(super) fn begin(&mut self, source: &'static str, _x: i32, _y: i32) {
        if self.active.is_some() {
            return;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = self.next_id;
        self.active = Some(ActivePenTrace {
            id,
            source,
            started: Instant::now(),
            first_ink: None,
            presented: false,
            presses: 0,
            updates: 0,
            releases: 0,
            recovered_press: 0,
            recovered_release: 0,
        });
    }

    pub(super) fn qtfb_edge(&mut self, input_type: i32, recovered: bool) {
        let Some(trace) = self.active.as_mut() else {
            return;
        };
        match input_type {
            qtfb::INPUT_PEN_PRESS => trace.presses += 1,
            qtfb::INPUT_PEN_UPDATE => trace.updates += 1,
            qtfb::INPUT_PEN_RELEASE => trace.releases += 1,
            _ => {}
        }
        if recovered {
            trace.recovered_press += 1;
        }
    }

    pub(super) fn ink_changed(&mut self) -> bool {
        if let Some(trace) = self.active.as_mut() {
            if trace.first_ink.is_none() {
                trace.first_ink = Some(Instant::now());
                return true;
            }
        }
        false
    }

    pub(super) fn presented(&mut self) {
        let Some(trace) = self.active.as_mut() else {
            return;
        };
        if trace.presented {
            return;
        }
        let Some(first_ink) = trace.first_ink else {
            return;
        };
        trace.presented = true;
        eprintln!(
            "magic-paper: event=local-ink-presented session={} latency_ms={}",
            trace.id,
            first_ink.elapsed().as_millis()
        );
    }

    pub(super) fn recovered_release(&mut self) {
        if let Some(trace) = self.active.as_mut() {
            trace.recovered_release += 1;
        }
    }

    pub(super) fn finish(&mut self, reason: &str) -> Option<u64> {
        let trace = self.active.take()?;
        eprintln!(
            "magic-paper: event=pen-session-finish session={} reason={reason} source={} duration_ms={} presses={} updates={} releases={} recovered_press={} recovered_release={}",
            trace.id,
            trace.source,
            trace.started.elapsed().as_millis(),
            trace.presses,
            trace.updates,
            trace.releases,
            trace.recovered_press,
            trace.recovered_release,
        );
        Some(trace.id)
    }
}

/// Keeps the domain model informed about accepted physical input. Output
/// cancellation is deliberately handled by the input gate: requested answers
/// ignore contacts, while only unfinished automatic heartbeats are preempted.
pub(super) struct InputPriority {
    model: domain::Model,
}

impl InputPriority {
    pub(super) fn foreground() -> Self {
        Self {
            model: domain::Model::foreground(),
        }
    }

    pub(super) fn begin_pen(&mut self) {
        let _ = domain::reduce(&mut self.model, AppEvent::UserInputStarted);
    }

    pub(super) fn end_pen(&mut self) {
        domain::reduce(&mut self.model, AppEvent::UserInputFinished);
    }

    pub(super) fn enter_foreground(&mut self) {
        let _ = domain::reduce(&mut self.model, AppEvent::EnterForeground);
    }

    pub(super) fn enter_background(&mut self) {
        let _ = domain::reduce(&mut self.model, AppEvent::EnterBackground);
    }
}

#[cfg(test)]
#[path = "input/tests.rs"]
mod tests;
