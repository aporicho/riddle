//! Ownership and polling policy for one OCR/oracle turn.
//!
//! Receivers and cancellation tokens live together so dropping or replacing a
//! turn is always a stale-result barrier. The device loop consumes typed poll
//! events instead of reasoning about mpsc disconnection and timeout details.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::oracle::{self, Event};
use crate::pi_preferences::PiPreferenceValues;

const IDLE_COMMIT_FAST: Duration = Duration::from_millis(2200);
const IDLE_COMMIT_SLOW: Duration = Duration::from_millis(2600);
const ORACLE_PATIENCE: Duration = Duration::from_secs(120);

pub(super) struct OracleTurn {
    rx: mpsc::Receiver<Result<Event, String>>,
    cancel: oracle::RequestCancel,
    generation: u64,
    current_generation: Arc<AtomicU64>,
}

/// Creates and owns the selected OCR/oracle backend. Callers receive a single
/// `OracleTurn`; channel construction and cancellation pairing stay here.
pub(super) struct OracleController {
    oracle: Option<oracle::Oracle>,
    current_generation: Arc<AtomicU64>,
    control_rx: Option<mpsc::Receiver<oracle::AgentControlStatus>>,
}

impl OracleController {
    pub(super) fn spawn(remember: bool) -> Self {
        let current_generation = Arc::new(AtomicU64::new(0));
        match oracle::Oracle::spawn(remember) {
            Ok(oracle) => {
                eprintln!("magicpaper: oracle ready");
                Self {
                    oracle: Some(oracle),
                    current_generation,
                    control_rx: None,
                }
            }
            Err(error) => {
                eprintln!("magicpaper: oracle spawn failed: {error}");
                Self {
                    oracle: None,
                    current_generation,
                    control_rx: None,
                }
            }
        }
    }

    pub(super) fn is_available(&self) -> bool {
        self.oracle.is_some()
    }

    pub(super) fn apply_pi_preferences(&mut self, values: PiPreferenceValues) {
        if let Some(oracle) = &mut self.oracle {
            oracle.apply_pi_preferences(values);
        }
    }

    pub(super) fn start_agent_control(&mut self, command: oracle::AgentControlCommand) {
        let Some(oracle) = &self.oracle else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        if oracle.start_agent_control(command, tx) {
            self.control_rx = Some(rx);
        }
    }

    pub(super) fn poll_agent_control(&mut self) -> Option<oracle::AgentControlStatus> {
        let receiver = self.control_rx.as_ref()?;
        match receiver.try_recv() {
            Ok(status) => {
                self.control_rx = None;
                Some(status)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.control_rx = None;
                Some(oracle::AgentControlStatus::NetworkError)
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }

    pub(super) fn supports_speculative(&self) -> bool {
        self.oracle
            .as_ref()
            .is_some_and(oracle::Oracle::supports_speculative)
    }

    pub(super) fn ask_capture(
        &self,
        capture: crate::ink::PageCapture,
        context: &oracle::TurnContext,
    ) -> Option<OracleTurn> {
        let oracle = self.oracle.as_ref()?;
        let (tx, rx) = mpsc::channel();
        let cancel = oracle.ask_capture(capture, context, tx);
        Some(self.new_turn(rx, cancel))
    }

    pub(super) fn ask_speculative_capture(
        &self,
        capture: crate::ink::PageCapture,
        context: &oracle::TurnContext,
    ) -> Option<OracleTurn> {
        let oracle = self.oracle.as_ref()?;
        let (tx, rx) = mpsc::channel();
        let cancel = oracle.ask_speculative_capture(capture, context, tx)?;
        Some(self.new_turn(rx, cancel))
    }

    pub(super) fn ask_text(
        &self,
        prompt: &str,
        context: &oracle::TurnContext,
    ) -> Option<OracleTurn> {
        let oracle = self.oracle.as_ref()?;
        let (tx, rx) = mpsc::channel();
        let cancel = oracle.ask_text(prompt, context, tx);
        Some(self.new_turn(rx, cancel))
    }

    fn new_turn(
        &self,
        rx: mpsc::Receiver<Result<Event, String>>,
        cancel: oracle::RequestCancel,
    ) -> OracleTurn {
        let generation = self
            .current_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        self.current_generation.store(generation, Ordering::Release);
        OracleTurn::new(rx, cancel, generation, Arc::clone(&self.current_generation))
    }

    /// Invalidate every receiver created before this lifecycle boundary. This
    /// is independent from network cancellation, so a late worker can never
    /// become visible after the app returns to foreground.
    pub(super) fn invalidate_active_turn(&self) {
        let next = self
            .current_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        self.current_generation.store(next, Ordering::Release);
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum FirstTurnPoll {
    Event(Result<Event, String>),
    Pending,
    TimedOut { request_id: u64 },
    Closed { request_id: u64 },
    Stale { request_id: u64 },
}

#[derive(Debug, PartialEq)]
pub(super) enum StreamPoll {
    Event(Event),
    Error { request_id: u64, error: String },
    Pending,
    Closed { request_id: u64 },
    Stale { request_id: u64 },
}

impl OracleTurn {
    pub(super) fn new(
        rx: mpsc::Receiver<Result<Event, String>>,
        cancel: oracle::RequestCancel,
        generation: u64,
        current_generation: Arc<AtomicU64>,
    ) -> Self {
        Self {
            rx,
            cancel,
            generation,
            current_generation,
        }
    }

    pub(super) fn request_id(&self) -> u64 {
        self.cancel.request_id()
    }

    pub(super) fn recommended_commit_ms(&self) -> Option<u64> {
        self.cancel.recommended_commit_ms()
    }

    pub(super) fn cancel(&self, reason: &str) {
        self.cancel.cancel_with_reason(reason);
    }

    pub(super) fn poll_first(&self, since: Instant) -> FirstTurnPoll {
        if !self.is_current() {
            return FirstTurnPoll::Stale {
                request_id: self.request_id(),
            };
        }
        match self.rx.try_recv() {
            Ok(event) => FirstTurnPoll::Event(event),
            Err(mpsc::TryRecvError::Empty) if since.elapsed() >= ORACLE_PATIENCE => {
                let request_id = self.request_id();
                self.cancel("ui-timeout");
                FirstTurnPoll::TimedOut { request_id }
            }
            Err(mpsc::TryRecvError::Empty) => FirstTurnPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => FirstTurnPoll::Closed {
                request_id: self.request_id(),
            },
        }
    }

    pub(super) fn poll_stream(&self) -> StreamPoll {
        if !self.is_current() {
            return StreamPoll::Stale {
                request_id: self.request_id(),
            };
        }
        match self.rx.try_recv() {
            Ok(Ok(event)) => StreamPoll::Event(event),
            Ok(Err(error)) => StreamPoll::Error {
                request_id: self.request_id(),
                error,
            },
            Err(mpsc::TryRecvError::Empty) => StreamPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => StreamPoll::Closed {
                request_id: self.request_id(),
            },
        }
    }

    fn is_current(&self) -> bool {
        self.current_generation.load(Ordering::Acquire) == self.generation
    }
}

impl Drop for OracleTurn {
    fn drop(&mut self) {
        self.cancel.cancel_with_reason("receiver-dropped");
    }
}

pub(super) type SpeculativeRequest = OracleTurn;

pub(super) fn cancel_speculative(pending: &mut Option<SpeculativeRequest>, reason: &str) {
    if let Some(request) = pending.take() {
        request.cancel(reason);
    }
}

pub(super) fn idle_commit_delay(pending: &Option<SpeculativeRequest>) -> Duration {
    match pending.as_ref().and_then(OracleTurn::recommended_commit_ms) {
        Some(2200) => IDLE_COMMIT_FAST,
        _ => IDLE_COMMIT_SLOW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn testing_turn(
        request_id: u64,
    ) -> (
        mpsc::Sender<Result<Event, String>>,
        OracleTurn,
        Arc<AtomicBool>,
    ) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let current_generation = Arc::new(AtomicU64::new(1));
        let (tx, rx) = mpsc::channel();
        let turn = OracleTurn::new(
            rx,
            oracle::RequestCancel::testing(request_id, Arc::clone(&cancelled)),
            1,
            current_generation,
        );
        (tx, turn, cancelled)
    }

    #[test]
    fn dropping_turn_cancels_worker_and_rejects_late_result() {
        let (tx, turn, cancelled) = testing_turn(77);
        assert_eq!(turn.request_id(), 77);
        drop(turn);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(tx.send(Ok(Event::Ink("stale".into()))).is_err());
    }

    #[test]
    fn first_and_stream_polls_hide_channel_details() {
        let (tx, turn, _) = testing_turn(8);
        assert_eq!(turn.poll_first(Instant::now()), FirstTurnPoll::Pending);
        tx.send(Ok(Event::Ink("one".into()))).unwrap();
        assert_eq!(
            turn.poll_first(Instant::now()),
            FirstTurnPoll::Event(Ok(Event::Ink("one".into())))
        );
        tx.send(Err("bad stream".into())).unwrap();
        assert_eq!(
            turn.poll_stream(),
            StreamPoll::Error {
                request_id: 8,
                error: "bad stream".into(),
            }
        );
    }

    #[test]
    fn timeout_cancels_at_the_controller_boundary() {
        let (_tx, turn, cancelled) = testing_turn(9);
        assert_eq!(
            turn.poll_first(Instant::now() - ORACLE_PATIENCE),
            FirstTurnPoll::TimedOut { request_id: 9 }
        );
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn generation_barrier_discards_late_results_after_background() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let current_generation = Arc::new(AtomicU64::new(4));
        let (tx, rx) = mpsc::channel();
        let turn = OracleTurn::new(
            rx,
            oracle::RequestCancel::testing(10, cancelled),
            4,
            Arc::clone(&current_generation),
        );
        current_generation.store(5, Ordering::Release);
        tx.send(Ok(Event::Ink("late".into()))).unwrap();
        assert_eq!(turn.poll_stream(), StreamPoll::Stale { request_id: 10 });
    }
}
