//! Runtime state-machine data, kept separate from transition logic.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::fb::BBox;
use crate::oracle::{self, Event};
use crate::reader;
use crate::ui;

const IDLE_COMMIT_FAST: Duration = Duration::from_millis(2200);
const IDLE_COMMIT_SLOW: Duration = Duration::from_millis(2600);

/// A response channel and its cancellation flag are one owned object.  When a
/// UI transition drops the turn, its worker is cancelled as well, so a late
/// OCR/model result can never be consumed by a newer handwritten page.
pub(super) struct OracleTurn {
    rx: mpsc::Receiver<Result<Event, String>>,
    cancel: oracle::RequestCancel,
}

impl OracleTurn {
    pub(super) fn new(
        rx: mpsc::Receiver<Result<Event, String>>,
        cancel: oracle::RequestCancel,
    ) -> Self {
        Self { rx, cancel }
    }

    pub(super) fn request_id(&self) -> u64 {
        self.cancel.request_id()
    }

    pub(super) fn recommended_commit_ms(&self) -> Option<u64> {
        self.cancel.recommended_commit_ms()
    }

    pub(super) fn try_recv(&self) -> Result<Result<Event, String>, mpsc::TryRecvError> {
        self.rx.try_recv()
    }

    pub(super) fn cancel(&self, reason: &str) {
        self.cancel.cancel_with_reason(reason);
    }
}

impl Drop for OracleTurn {
    fn drop(&mut self) {
        // Normal completion commonly drops an already-finished worker.  Keep
        // that path silent; explicit interruption calls `cancel()` first and
        // supplies a useful reason in the structured log.
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

pub(super) enum State {
    Listening {
        last_pen: Option<Instant>,
    },
    Drinking {
        stage: u32,
        next: Instant,
        region: BBox,
        rx: OracleTurn,
    },
    Thinking {
        rx: OracleTurn,
        since: Instant,
    },
    Replying {
        plan: WritePlan,
        next: Instant,
        rx: Option<OracleTurn>,
        /// The visible page has no room for more strokes.  The receiver must
        /// nevertheless stay alive so the faithful transcript and local
        /// directives at the stream tail are still applied and remembered.
        page_full: bool,
    },
    Lingering {
        until: Instant,
        region: BBox,
    },
    FadingReply {
        stage: u32,
        next: Instant,
        region: BBox,
    },
    /// The guide panel. `panel: None` = dismissed, waiting for pen-up so the
    /// dismissing touch doesn't leave a mark on the page.
    Help {
        panel: Option<ui::help::Help>,
        until: Instant,
    },
    /// A remembered page rising through the paper: date, the writer's own
    /// past ink, Tom's old reply — all in faded ink. `saved` is today's page.
    Conjuring {
        plan: ConjurePlan,
        next: Instant,
        saved: Vec<u8>,
    },
    /// The conjured memory rests on the page. Pen contact (or time) dissolves
    /// it and today's page returns. `saved: None` = dismissed, waiting pen-up.
    MemoryShown {
        saved: Option<Vec<u8>>,
        until: Instant,
        region: BBox,
    },
    /// A device-local numbered task page. Horizontal pen strokes delete a
    /// row; a small tap outside all rows restores the underlying page.
    TaskList {
        panel: ui::paper_list::PaperList,
    },
    /// The persistent, non-scheduled TODO page uses the same paper gestures.
    TodoList {
        panel: ui::paper_list::PaperList,
    },
    /// A local three-font picker. Row taps switch immediately and redraw the
    /// preview; a tap on blank paper restores the page underneath.
    FontList {
        panel: ui::font_settings::FontPanel,
    },
    /// The newest local dialogue pages; striking a row forgets both its text
    /// and replay strokes.
    HistoryList {
        panel: ui::paper_list::PaperList,
    },
    /// Ambiguous `read <title>` matches. A short pen tap chooses a book;
    /// blank paper dismisses the catalog without touching any files.
    ReaderList {
        panel: ui::paper_list::PaperList,
        books: Vec<reader::Book>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TurnKind {
    User,
    Heartbeat,
}

/// A memory being rewritten onto the page: pre-positioned strokes with their
/// original radii, drawn in faded ink.
pub(super) struct ConjurePlan {
    pub(super) strokes: Vec<Vec<(i32, i32, i32)>>,
    pub(super) stroke_i: usize,
    pub(super) point_i: usize,
    pub(super) region: BBox,
}

pub(super) struct WritePlan {
    pub(super) strokes: Vec<Vec<(i32, i32)>>,
    pub(super) stroke_i: usize,
    pub(super) point_i: usize,
    pub(super) region: BBox,
    /// Where the next streamed chunk's first line starts.
    pub(super) next_y: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn dropping_turn_cancels_worker_and_rejects_late_result() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let turn = OracleTurn::new(
            rx,
            oracle::RequestCancel::testing(77, Arc::clone(&cancelled)),
        );
        assert_eq!(turn.request_id(), 77);
        drop(turn);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(tx.send(Ok(Event::Ink("stale".into()))).is_err());
    }
}
