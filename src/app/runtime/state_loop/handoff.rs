//! Non-blocking Runtime App handoff states.

use std::time::{Duration, Instant};

use super::super::{input_mode_for_state, Engine};
use crate::app::state::State;
use crate::platform::{AppToken, InputMode};
use crate::runtime_control::{OpenReaderPoll, OpenReaderRequest};

const HANDOFF_PENDING_TIMEOUT: Duration = Duration::from_secs(5);

/// A Runtime App handoff must not emit an input-mode RPC after its launch
/// request starts: the manager may accept and revoke the token concurrently.
pub(crate) fn input_mode_request_for_state(state: &State) -> Option<InputMode> {
    (!matches!(
        state,
        State::AwaitingHandoffAck { .. } | State::HandoffPending { .. }
    ))
    .then(|| input_mode_for_state(state))
}

impl Engine<'_> {
    pub(super) fn tick_handoff_ack(&mut self, request: OpenReaderRequest) -> State {
        match request.poll() {
            OpenReaderPoll::Pending => State::AwaitingHandoffAck { request },
            OpenReaderPoll::Accepted => {
                let token = request.token().clone();
                eprintln!(
                    "magic-paper: event=reader-handoff-accepted request={} app=koreader",
                    request.request_id()
                );
                State::HandoffPending {
                    token,
                    until: Instant::now() + HANDOFF_PENDING_TIMEOUT,
                }
            }
            OpenReaderPoll::Failed(error) => {
                crate::app::turn_controller::reader_request_failed(&self.font, error)
            }
        }
    }

    pub(super) fn tick_handoff_pending(&self, token: AppToken, until: Instant) -> State {
        if handoff_is_current(self.lifecycle.active_token(), &token, until, Instant::now()) {
            State::HandoffPending { token, until }
        } else {
            eprintln!("magic-paper: KOReader handoff was superseded or timed out; resuming paper");
            State::Listening { last_pen: None }
        }
    }
}

pub(super) fn handoff_is_current(
    active: Option<&AppToken>,
    expected: &AppToken,
    until: Instant,
    now: Instant,
) -> bool {
    now < until && active == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(epoch: u64) -> AppToken {
        AppToken {
            app_id: "magicpaper".into(),
            generation: 7,
            foreground_epoch: epoch,
            lease_id: Some(13),
        }
    }

    #[test]
    fn pending_handoff_requires_the_same_live_foreground_token_and_deadline() {
        let expected = token(11);
        let now = Instant::now();
        assert!(handoff_is_current(
            Some(&expected),
            &expected,
            now + Duration::from_secs(1),
            now,
        ));
        assert!(!handoff_is_current(
            Some(&token(12)),
            &expected,
            now + Duration::from_secs(1),
            now,
        ));
        assert!(!handoff_is_current(Some(&expected), &expected, now, now,));
    }
}
