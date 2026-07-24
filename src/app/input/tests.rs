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

fn frame(phase: PenPhase, tool: PenTool) -> PenFrame {
    PenFrame {
        sequence: 1,
        kernel_time_ns: 1,
        phase,
        tool,
        x: 10,
        y: 20,
        pressure: 100,
    }
}

#[test]
fn locked_requested_output_ignores_pen_moves_eraser_and_touch_equivalents() {
    assert_eq!(
        gate_pen_frame(
            InputMode::AnimationLocked,
            frame(PenPhase::Move, PenTool::Pen),
            true,
            false,
        ),
        PenGate::Ignore
    );
    assert_eq!(
        gate_pen_frame(
            InputMode::AnimationLocked,
            frame(PenPhase::Down, PenTool::Eraser),
            true,
            false,
        ),
        PenGate::Ignore
    );
}

#[test]
fn only_fresh_pen_down_fades_an_answer_or_preempts_a_heartbeat() {
    let down = frame(PenPhase::Down, PenTool::Pen);
    assert_eq!(
        gate_pen_frame(InputMode::AnimationLocked, down, true, false),
        PenGate::FadeAnswer
    );
    assert_eq!(
        gate_pen_frame(InputMode::AnimationLocked, down, false, true),
        PenGate::CancelHeartbeat
    );
    assert_eq!(
        gate_pen_frame(InputMode::AnimationLocked, down, true, true),
        PenGate::FadeAnswer
    );
    assert_eq!(
        gate_pen_frame(InputMode::AnimationLocked, down, false, false),
        PenGate::Ignore
    );
}

#[test]
fn fading_reply_contacts_are_always_ignored_even_for_a_heartbeat_turn() {
    let down = frame(PenPhase::Down, PenTool::Pen);
    assert_eq!(
        gate_pen_frame(InputMode::AnimationLocked, down, false, false),
        PenGate::Ignore
    );
}

#[test]
fn writing_and_modal_modes_forward_contacts_to_their_local_handlers() {
    let down = frame(PenPhase::Down, PenTool::Pen);
    assert_eq!(
        gate_pen_frame(InputMode::Writing, down, false, false),
        PenGate::Apply
    );
    assert_eq!(
        gate_pen_frame(InputMode::Modal, down, false, false),
        PenGate::Apply
    );
}

#[test]
fn modal_contact_keeps_tool_and_points_until_release_classification() {
    let mut contact = ModalContact::begin(PointerTool::Finger, 20, 30);
    assert!(contact.push(PointerTool::Finger, 22, 31));
    assert!(!contact.push(PointerTool::Pen, 200, 300));
    assert_eq!(contact.tool, PointerTool::Finger);
    assert_eq!(contact.points, vec![Point::new(20, 30), Point::new(22, 31)]);
    assert!(matches!(
        contact.classify(),
        Some(Gesture::Tap {
            tool: PointerTool::Finger,
            ..
        })
    ));
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
