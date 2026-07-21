//! Pen-session normalization, qtfb recovery, and the user-input priority
//! boundary.  Keeping these policies out of `runtime` prevents display/network
//! details from becoming part of input state.

use std::time::{Duration, Instant};

use crate::domain::{self, AppEvent, Effect, Priority};
use crate::platform::RefreshIntent;
use crate::{display, ink, qtfb};

use super::state::{State, TurnKind};

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
    pub(super) fn begin(&mut self, source: &'static str, x: i32, y: i32) {
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
        eprintln!("magic-paper: event=pen-session-start session={id} source={source} x={x} y={y}");
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

/// Owns the pure priority model and translates its cancellation effects into
/// today's concrete runtime state. This adapter disappears once the rest of
/// the loop consumes `Effect` directly.
pub(super) struct InputPriority {
    model: domain::Model,
}

impl InputPriority {
    pub(super) fn foreground() -> Self {
        Self {
            model: domain::Model::foreground(),
        }
    }

    pub(super) fn begin_pen(
        &mut self,
        state: &mut State,
        turn_kind: TurnKind,
        surf: &mut crate::surface::Surface,
        disp: &display::Display,
        user_ink: &mut ink::Ink,
    ) -> bool {
        let output_priority = match turn_kind {
            TurnKind::Heartbeat => Priority::AutomaticOutput,
            TurnKind::User => Priority::RequestedOutput,
        };
        let has_output = matches!(
            state,
            State::Drinking { .. } | State::Thinking { .. } | State::Replying { .. }
        );
        if has_output {
            domain::reduce(
                &mut self.model,
                AppEvent::OutputStarted {
                    priority: output_priority,
                },
            );
        }
        let effects = domain::reduce(&mut self.model, AppEvent::UserInputStarted);
        if !effects
            .iter()
            .any(|effect| matches!(effect, Effect::CancelOutput { .. }))
        {
            return false;
        }

        let old = std::mem::replace(state, State::Listening { last_pen: None });
        let (region, request) = match old {
            State::Drinking { region, rx, .. } => (Some(region), Some(rx)),
            State::Thinking { rx, .. } => (None, Some(rx)),
            State::Replying { plan, rx, .. } => (Some(plan.region), rx),
            other => {
                *state = other;
                return false;
            }
        };
        if let Some(request) = request {
            request.cancel("user-input-priority");
        }
        if effects.contains(&Effect::ClearTransientOutput) {
            if let Some(region) = region.filter(|region| !region.is_empty()) {
                let (x, y, w, h) = region.rect();
                surf.fill_rect(
                    x as usize,
                    y as usize,
                    w as usize,
                    h as usize,
                    crate::surface::WHITE,
                );
                disp.present_region(x, y, w, h, RefreshIntent::Ink);
            }
            user_ink.clear();
        }
        eprintln!("magic-paper: event=output-interrupted reason=user-input");
        true
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
mod tests {
    use super::*;

    #[test]
    fn stationary_contact_has_no_time_driven_release() {
        let started = Instant::now();
        let mut state = QtfbPenState::default();
        assert_eq!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 40, started),
            QtfbPenTransition::Draw {
                close_orphan: false,
                recovered_press: true,
            }
        );
        assert_eq!(
            state.transition(
                qtfb::INPUT_PEN_UPDATE,
                40,
                started + Duration::from_millis(500),
            ),
            QtfbPenTransition::Draw {
                close_orphan: false,
                recovered_press: false,
            }
        );
    }

    #[test]
    fn pressure_zero_update_is_release_only_while_down() {
        let now = Instant::now();
        let mut state = QtfbPenState::default();
        assert_eq!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 0, now),
            QtfbPenTransition::Hover
        );
        assert!(matches!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 40, now),
            QtfbPenTransition::Draw { .. }
        ));
        assert_eq!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 0, now),
            QtfbPenTransition::Release {
                was_down: true,
                recovered: true,
            }
        );
    }

    #[test]
    fn pressure_event_after_long_gap_closes_lost_release_first() {
        let started = Instant::now();
        let mut state = QtfbPenState::default();
        assert!(matches!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 40, started),
            QtfbPenTransition::Draw {
                recovered_press: true,
                ..
            }
        ));
        assert_eq!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 40, started + QTFB_ORPHAN_GAP,),
            QtfbPenTransition::Draw {
                close_orphan: true,
                recovered_press: true,
            }
        );
    }

    #[test]
    fn only_first_visible_point_requests_urgent_flush() {
        let mut trace = PenTrace::default();
        trace.begin("test", 10, 20);
        assert!(trace.ink_changed());
        assert!(!trace.ink_changed());
        trace.finish("test");
    }

    #[test]
    fn background_epoch_reset_forgets_old_contact() {
        let now = Instant::now();
        let mut state = QtfbPenState::default();
        assert!(matches!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 40, now),
            QtfbPenTransition::Draw { .. }
        ));
        state.reset();
        assert_eq!(
            state.transition(qtfb::INPUT_PEN_UPDATE, 0, now),
            QtfbPenTransition::Hover
        );
    }

    #[test]
    fn pen_sequences_never_emit_zero_even_after_wrap() {
        let mut sequence = PenSequence(u64::MAX);
        assert_eq!(sequence.next(), 1);
        assert_eq!(sequence.next(), 2);
    }
}
