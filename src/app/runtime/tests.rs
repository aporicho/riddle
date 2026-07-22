use super::input_loop::orphan_recovery_may_release;
use super::{input_mode_for_state, input_mode_needs_request, State};
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
fn orphan_gap_never_impersonates_the_real_up_after_answer_fade() {
    assert!(!orphan_recovery_may_release(&State::AwaitingPenUp));
    assert!(orphan_recovery_may_release(&State::Listening {
        last_pen: None
    }));
}
