//! Runtime state-machine data, kept separate from transition logic.

use std::time::Instant;

use crate::fb::BBox;
use crate::fonts;
use crate::reader;
use crate::ui;

use super::layout_controller::LayoutJob;
use super::oracle_controller::OracleTurn;

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
    AnswerVisible {
        until: Instant,
        region: BBox,
    },
    FadingReply {
        stage: u32,
        next: Instant,
        region: BBox,
    },
    /// The reply is fully erased, but a contact that began while input was
    /// locked is still physically down. Keep swallowing it until its Up, then
    /// re-enable writing so only a later, fresh Down can make page ink.
    AwaitingPenUp,
    /// The guide panel. `panel: None` = dismissed, waiting for pen-up so the
    /// dismissing touch doesn't leave a mark on the page.
    Help {
        panel: Option<ui::help::Help>,
        until: Instant,
    },
    /// A remembered page rising through the paper: date, the writer's own
    /// past ink, MP's old reply — all in faded ink. `saved` is today's page.
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
        origin: FontOrigin,
    },
    /// Device-local experience settings. Font calibration opens as a nested
    /// modal and returns here without losing this panel's saved page.
    Settings {
        panel: ui::settings::SettingsPanel,
    },
    /// Non-secret Pi provider/model/tool preferences. The parent settings page
    /// remains alive so blank-paper dismissal can restore it exactly.
    PiSettings {
        panel: ui::pi_settings::PiSettingsPanel,
        settings: ui::settings::SettingsPanel,
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

pub(super) enum FontOrigin {
    Paper,
    Settings(ui::settings::SettingsPanel),
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
    /// Monotonic origin for layout/animation telemetry. It starts when the
    /// first paper-safe text event reaches the UI, before background layout.
    pub(super) created_at: Instant,
    pub(super) first_damage_logged: bool,
    pub(super) strokes: Vec<Vec<(f32, f32)>>,
    pub(super) stroke_i: usize,
    pub(super) point_i: usize,
    pub(super) region: BBox,
    /// Where the next streamed chunk's first line starts.
    pub(super) next_y: i32,
    /// At most one CPU-heavy font rasterization runs at a time. Later stream
    /// chunks wait in `queued_text` so their vertical positions stay ordered.
    pub(super) layout: Option<LayoutJob>,
    pub(super) queued_text: String,
    pub(super) layout_font: Option<fonts::FontBook>,
    /// Before any pixel is drawn, sentence-sized stream events are briefly
    /// coalesced. A fast, short answer is then centered as one measured block;
    /// a still-open answer falls back to top-safe streaming on the next UI tick.
    pub(super) initial_buffering: bool,
    /// Unicode grapheme clusters that survived fitting and will leave visible
    /// ink. Hidden overflow must not extend the answer dwell time.
    pub(super) visible_graphemes: usize,
    /// A layout worker had to omit overflow at the minimum configured size.
    pub(super) truncated: bool,
}
