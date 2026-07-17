//! Runtime state-machine data, kept separate from transition logic.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::fb::BBox;
use crate::oracle::{self, Event};
use crate::ui;

const IDLE_COMMIT_FAST: Duration = Duration::from_millis(2200);
const IDLE_COMMIT_SLOW: Duration = Duration::from_millis(2600);

pub(super) type OracleRx = mpsc::Receiver<Result<Event, String>>;

pub(super) struct SpeculativeRequest {
    pub(super) rx: OracleRx,
    pub(super) cancel: oracle::RequestCancel,
}

pub(super) fn cancel_speculative(pending: &mut Option<SpeculativeRequest>, reason: &str) {
    if let Some(request) = pending.take() {
        request.cancel.cancel();
        eprintln!("riddle: speculative oracle discarded ({reason})");
    }
}

pub(super) fn idle_commit_delay(pending: &Option<SpeculativeRequest>) -> Duration {
    match pending
        .as_ref()
        .and_then(|request| request.cancel.recommended_commit_ms())
    {
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
        rx: OracleRx,
    },
    Thinking {
        rx: OracleRx,
        since: Instant,
    },
    Replying {
        plan: WritePlan,
        next: Instant,
        rx: Option<OracleRx>,
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
