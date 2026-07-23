//! Scheduling for streamed reply layout and handwriting animation.
//!
//! The controller mutates the formal `WritePlan` and returns semantic effects;
//! the device loop only presents damage and performs persistence when a reply
//! is complete.

use std::time::{Duration, Instant};

use crate::fb::{screen_h, BBox};
use crate::fonts;
use crate::platform::DamageRect;
use crate::surface::{Surface, BLACK};

use super::reply::{append_reply, poll_reply_layout, start_initial_reply_layout};
use super::state::WritePlan;

// The Move panel cannot usefully present a desktop-style 70 Hz sequence.
// Larger batches at 25 Hz keep the same writing speed while cutting transport
// commits, wakeups, and visible micro-flicker by roughly two thirds.
// Preserve a visibly handwritten cadence while the network is still
// producing sentences, then drain the finished answer much faster.  The old
// fixed 72-point budget made real 35–88 character answers take another
// 3–7 seconds after the model had already finished.
const STREAMING_POINT_BUDGET: usize = 96;
const FINISHED_POINT_BUDGET: usize = 192;
const FRAME_INTERVAL: Duration = Duration::from_millis(40);
/// Font rasterization normally finishes in a few milliseconds. Polling the
/// very first layout at the animation cadence added up to 40 ms of avoidable
/// latency before a single pixel could be presented.
const FIRST_LAYOUT_POLL_INTERVAL: Duration = Duration::from_millis(2);

fn reply_tick_interval(waiting_for_first_layout: bool) -> Duration {
    if waiting_for_first_layout {
        FIRST_LAYOUT_POLL_INTERVAL
    } else {
        FRAME_INTERVAL
    }
}

pub(super) struct ReplyController;

#[derive(Clone, Copy, Debug)]
pub(super) struct ReplyCompletion {
    pub(super) region: BBox,
    pub(super) visible_graphemes: usize,
    pub(super) reply_elapsed_ms: u128,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReplyEffects {
    pub(super) damage: Option<DamageRect>,
    /// The completed answer block to settle with a quality partial waveform.
    pub(super) settle: Option<DamageRect>,
    /// Elapsed time when the first visible damage was submitted by the
    /// controller. The runtime logs it only after handing damage to display.
    pub(super) first_damage_latency_ms: Option<u128>,
    pub(super) completion: Option<ReplyCompletion>,
}

impl ReplyController {
    /// Rasterize and append one streamed chunk unless the visible page is
    /// already full. The caller still drains hidden transcript events.
    pub(super) fn append_text(
        font: &fonts::FontBook,
        plan: &mut WritePlan,
        page_full: &mut bool,
        text: &str,
    ) {
        if *page_full || plan.next_y > screen_h() as i32 - 200 {
            if !*page_full {
                *page_full = true;
                eprintln!("magicpaper: reply reached the page bottom; draining hidden stream tail");
            }
            return;
        }
        append_reply(font, plan, text);
    }

    /// Draw at most one bounded animation frame and report what the runtime
    /// should present. Font/point scheduling never leaks into the event loop.
    pub(super) fn tick(
        plan: &mut WritePlan,
        next: &mut Instant,
        stream_open: bool,
        page_full: &mut bool,
        surf: &mut Surface,
        now: Instant,
    ) -> ReplyEffects {
        start_initial_reply_layout(plan, stream_open);
        poll_reply_layout(plan);
        *page_full |= plan.truncated;
        if now < *next {
            return ReplyEffects::default();
        }

        let mut dirty = BBox::empty();
        let mut budget = if stream_open {
            STREAMING_POINT_BUDGET
        } else {
            FINISHED_POINT_BUDGET
        };
        while budget > 0 && plan.stroke_i < plan.strokes.len() {
            let stroke = &plan.strokes[plan.stroke_i];
            if plan.point_i >= stroke.len() {
                plan.stroke_i += 1;
                plan.point_i = 0;
                continue;
            }
            let (x, y) = stroke[plan.point_i];
            if plan.point_i > 0 {
                let (px, py) = stroke[plan.point_i - 1];
                surf.brush_line_aa(px, py, x, y, 2.0, BLACK);
            } else {
                surf.brush_line_aa(x, y, x, y, 2.0, BLACK);
            }
            dirty.add(x.round() as i32, y.round() as i32, 4);
            plan.point_i += 1;
            budget -= 1;
        }

        let damage = (!dirty.is_empty()).then(|| {
            let (x, y, width, height) = dirty.rect();
            DamageRect {
                x,
                y,
                width,
                height,
            }
        });
        let first_damage_latency_ms = if damage.is_some() && !plan.first_damage_logged {
            plan.first_damage_logged = true;
            Some(plan.created_at.elapsed().as_millis())
        } else {
            None
        };
        let completion = if plan.stroke_i >= plan.strokes.len()
            && !stream_open
            && plan.layout.is_none()
            && plan.queued_text.is_empty()
            && !plan.initial_buffering
        {
            Some(ReplyCompletion {
                region: plan.region,
                visible_graphemes: plan.visible_graphemes,
                reply_elapsed_ms: plan.created_at.elapsed().as_millis(),
            })
        } else {
            let waiting_for_first_layout = damage.is_none()
                && !plan.first_damage_logged
                && plan.layout.is_some()
                && plan.stroke_i >= plan.strokes.len();
            *next = now + reply_tick_interval(waiting_for_first_layout);
            None
        };
        let settle = completion.and_then(|completion| {
            (!completion.region.is_empty()).then(|| {
                let (x, y, width, height) = completion.region.rect();
                DamageRect {
                    x,
                    y,
                    width,
                    height,
                }
            })
        });
        ReplyEffects {
            damage,
            settle,
            first_damage_latency_ms,
            completion,
        }
    }
}

pub(super) fn answer_visible_duration(visible_graphemes: usize, dwell_percent: u16) -> Duration {
    let base = Duration::from_secs(4)
        .saturating_add(Duration::from_millis(
            (visible_graphemes as u64).saturating_mul(100),
        ))
        .min(Duration::from_secs(15));
    let percent = dwell_percent.clamp(50, 200) as u128;
    let millis = base.as_millis().saturating_mul(percent) / 100;
    Duration::from_millis(millis.clamp(2_000, 30_000) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::PixFmt;

    fn surface(width: usize, height: usize) -> (Vec<u8>, Surface) {
        crate::fb::test_init_screen();
        let mut bytes = vec![0xff; width * height * 2];
        let surface = Surface::new(
            bytes.as_mut_ptr(),
            bytes.len(),
            width,
            height,
            width * 2,
            PixFmt::Rgb565,
        );
        (bytes, surface)
    }

    fn plan() -> WritePlan {
        let mut region = BBox::empty();
        region.add(10, 10, 4);
        region.add(14, 10, 4);
        WritePlan {
            created_at: Instant::now(),
            first_damage_logged: false,
            strokes: vec![vec![(10.0, 10.0), (14.0, 10.0)]],
            stroke_i: 0,
            point_i: 0,
            region,
            next_y: 20,
            layout: None,
            queued_text: String::new(),
            layout_font: None,
            initial_buffering: false,
            visible_graphemes: 2,
            truncated: false,
        }
    }

    #[test]
    fn animation_reports_damage_and_waits_for_stream_close() {
        let (_bytes, mut surface) = surface(32, 32);
        let mut plan = plan();
        let now = Instant::now();
        let mut next = now;
        let effects =
            ReplyController::tick(&mut plan, &mut next, true, &mut false, &mut surface, now);
        assert!(effects.damage.is_some());
        assert!(effects.completion.is_none());
        assert!(next > now);
    }

    #[test]
    fn closed_stream_completes_after_final_stroke() {
        let (_bytes, mut surface) = surface(32, 32);
        let mut plan = plan();
        let now = Instant::now();
        let mut next = now;
        let effects =
            ReplyController::tick(&mut plan, &mut next, false, &mut false, &mut surface, now);
        let completion = effects.completion.expect("complete reply");
        assert_eq!(effects.settle.map(|damage| damage.width), Some(13));
        assert_eq!(completion.visible_graphemes, 2);
        assert!(completion.reply_elapsed_ms < 1_000);
        assert_eq!(completion.region.rect(), plan.region.rect());
    }

    #[test]
    fn animation_coalesces_work_until_the_next_panel_frame() {
        let (_bytes, mut surface) = surface(32, 32);
        let mut plan = plan();
        let now = Instant::now();
        let mut next = now + FRAME_INTERVAL;
        let effects =
            ReplyController::tick(&mut plan, &mut next, true, &mut false, &mut surface, now);
        assert!(effects.damage.is_none());
        assert_eq!(plan.stroke_i, 0);
        assert_eq!(plan.point_i, 0);
    }

    #[test]
    fn first_layout_is_polled_before_the_animation_frame_deadline() {
        assert_eq!(reply_tick_interval(true), Duration::from_millis(2));
        assert_eq!(reply_tick_interval(false), FRAME_INTERVAL);
    }

    #[test]
    fn a_closed_stream_drains_more_points_than_a_live_stream() {
        let (_bytes, mut surface) = surface(512, 32);
        let points = (STREAMING_POINT_BUDGET + FINISHED_POINT_BUDGET) / 2;
        let mut streaming = plan_with_points(points);
        let mut finished = plan_with_points(points);
        let now = Instant::now();
        let mut streaming_next = now;
        let mut finished_next = now;

        let live = ReplyController::tick(
            &mut streaming,
            &mut streaming_next,
            true,
            &mut false,
            &mut surface,
            now,
        );
        assert!(live.completion.is_none());
        assert_eq!(streaming.point_i, STREAMING_POINT_BUDGET);

        let closed = ReplyController::tick(
            &mut finished,
            &mut finished_next,
            false,
            &mut false,
            &mut surface,
            now,
        );
        assert!(closed.completion.is_some());
    }

    #[test]
    fn answer_dwell_uses_visible_graphemes_and_caps_at_fifteen_seconds() {
        assert_eq!(answer_visible_duration(0, 100), Duration::from_secs(4));
        assert_eq!(answer_visible_duration(10, 100), Duration::from_secs(5));
        assert_eq!(answer_visible_duration(110, 100), Duration::from_secs(15));
        assert_eq!(
            answer_visible_duration(usize::MAX, 100),
            Duration::from_secs(15)
        );
        assert_eq!(answer_visible_duration(0, 50), Duration::from_secs(2));
        assert_eq!(answer_visible_duration(110, 200), Duration::from_secs(30));
    }

    fn plan_with_points(points: usize) -> WritePlan {
        let mut region = BBox::empty();
        region.add(1, 1, 1);
        WritePlan {
            created_at: Instant::now(),
            first_damage_logged: false,
            strokes: vec![(0..points).map(|x| (x as f32, 1.0)).collect()],
            stroke_i: 0,
            point_i: 0,
            region,
            next_y: 20,
            layout: None,
            queued_text: String::new(),
            layout_font: None,
            initial_buffering: false,
            visible_graphemes: points,
            truncated: false,
        }
    }
}
