use super::input_loop::orphan_recovery_may_release;
use super::state_loop::input_mode_request_for_state;
use super::{input_mode_for_state, input_mode_needs_request, suspend_visible_state, State};
use crate::platform::InputMode;

#[test]
fn writable_and_post_fade_contact_states_have_opposite_input_modes() {
    assert_eq!(
        input_mode_for_state(&State::Listening { last_pen: None }),
        InputMode::Writing
    );
    assert_eq!(
        input_mode_for_state(&State::AwaitingPenUp),
        InputMode::AnimationLocked
    );
}

#[test]
fn a_new_foreground_epoch_reasserts_even_an_unchanged_mode() {
    assert!(!input_mode_needs_request(
        true,
        InputMode::Modal,
        InputMode::Modal
    ));
    assert!(input_mode_needs_request(
        false,
        InputMode::Modal,
        InputMode::Modal
    ));
    assert!(input_mode_needs_request(
        true,
        InputMode::Writing,
        InputMode::AnimationLocked
    ));
}

#[test]
fn accepted_handoff_never_sends_a_post_ack_input_mode_request() {
    let token = crate::platform::AppToken {
        app_id: "magicpaper".into(),
        generation: 1,
        foreground_epoch: 1,
        lease_id: Some(1),
    };
    assert_eq!(
        input_mode_for_state(&State::HandoffPending {
            token: token.clone(),
            until: std::time::Instant::now(),
        }),
        InputMode::AnimationLocked
    );
    assert_eq!(
        input_mode_request_for_state(&State::HandoffPending {
            token,
            until: std::time::Instant::now(),
        }),
        None
    );
    assert_eq!(
        input_mode_request_for_state(&State::Listening { last_pen: None }),
        Some(InputMode::Writing)
    );
}

#[test]
fn backgrounding_an_accepted_handoff_restores_a_writable_foreground_state() {
    crate::fb::test_init_screen();
    let mut bytes = vec![0xff; 32 * 32 * 2];
    let mut surface = crate::surface::Surface::new(
        bytes.as_mut_ptr(),
        bytes.len(),
        32,
        32,
        64,
        crate::surface::PixFmt::Rgb565,
    );
    let mut ink = crate::ink::Ink::new();
    let mut state = State::HandoffPending {
        token: crate::platform::AppToken {
            app_id: "magicpaper".into(),
            generation: 1,
            foreground_epoch: 1,
            lease_id: Some(1),
        },
        until: std::time::Instant::now(),
    };
    suspend_visible_state(&mut state, &mut surface, &mut ink);
    assert!(matches!(state, State::Listening { last_pen: None }));
    assert_eq!(input_mode_for_state(&state), InputMode::Writing);
}

#[test]
fn background_before_open_app_ack_cancels_and_discards_the_late_result() {
    crate::fb::test_init_screen();
    let mut bytes = vec![0xff; 32 * 32 * 2];
    let mut surface = crate::surface::Surface::new(
        bytes.as_mut_ptr(),
        bytes.len(),
        32,
        32,
        64,
        crate::surface::PixFmt::Rgb565,
    );
    let mut ink = crate::ink::Ink::new();
    let (request, cancelled) =
        crate::runtime_control::OpenReaderRequest::pending_for_test(crate::platform::AppToken {
            app_id: "magicpaper".into(),
            generation: 1,
            foreground_epoch: 1,
            lease_id: Some(1),
        });
    let mut state = State::AwaitingHandoffAck { request };
    suspend_visible_state(&mut state, &mut surface, &mut ink);
    assert!(matches!(state, State::Listening { last_pen: None }));
    assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn orphan_gap_never_impersonates_the_real_up_after_answer_fade() {
    assert!(!orphan_recovery_may_release(&State::AwaitingPenUp));
    assert!(orphan_recovery_may_release(&State::Listening {
        last_pen: None
    }));
}
