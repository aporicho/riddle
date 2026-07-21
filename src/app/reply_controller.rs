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

use super::reply::{append_reply, poll_reply_layout};
use super::state::WritePlan;

// The Move panel cannot usefully present a desktop-style 70 Hz sequence.
// Larger batches at 25 Hz keep the same writing speed while cutting transport
// commits, wakeups, and visible micro-flicker by roughly two thirds.
const POINT_BUDGET: usize = 72;
const FRAME_INTERVAL: Duration = Duration::from_millis(40);

pub(super) struct ReplyController;

#[derive(Clone, Copy, Debug)]
pub(super) struct ReplyCompletion {
    pub(super) region: BBox,
    pub(super) linger: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReplyEffects {
    pub(super) damage: Option<DamageRect>,
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
                eprintln!("riddle: reply reached the page bottom; draining hidden stream tail");
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
        surf: &mut Surface,
        now: Instant,
    ) -> ReplyEffects {
        poll_reply_layout(plan);
        if now < *next {
            return ReplyEffects::default();
        }

        let mut dirty = BBox::empty();
        let mut budget = POINT_BUDGET;
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
                surf.brush_line(px, py, x, y, 2, BLACK);
            } else {
                surf.stamp(x, y, 2, BLACK);
            }
            dirty.add(x, y, 4);
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
        let completion = if plan.stroke_i >= plan.strokes.len()
            && !stream_open
            && plan.layout.is_none()
            && plan.queued_text.is_empty()
        {
            let stroke_points: usize = plan.strokes.iter().map(Vec::len).sum();
            let linger =
                Duration::from_millis(4000 + stroke_points as u64 * 2).min(Duration::from_secs(20));
            Some(ReplyCompletion {
                region: plan.region,
                linger,
            })
        } else {
            *next = now + FRAME_INTERVAL;
            None
        };
        ReplyEffects { damage, completion }
    }
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
            strokes: vec![vec![(10, 10), (14, 10)]],
            stroke_i: 0,
            point_i: 0,
            region,
            next_y: 20,
            layout: None,
            queued_text: String::new(),
            layout_font: None,
        }
    }

    #[test]
    fn animation_reports_damage_and_waits_for_stream_close() {
        let (_bytes, mut surface) = surface(32, 32);
        let mut plan = plan();
        let now = Instant::now();
        let mut next = now;
        let effects = ReplyController::tick(&mut plan, &mut next, true, &mut surface, now);
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
        let effects = ReplyController::tick(&mut plan, &mut next, false, &mut surface, now);
        let completion = effects.completion.expect("complete reply");
        assert_eq!(completion.linger, Duration::from_millis(4004));
        assert_eq!(completion.region.rect(), plan.region.rect());
    }

    #[test]
    fn animation_coalesces_work_until_the_next_panel_frame() {
        let (_bytes, mut surface) = surface(32, 32);
        let mut plan = plan();
        let now = Instant::now();
        let mut next = now + FRAME_INTERVAL;
        let effects = ReplyController::tick(&mut plan, &mut next, true, &mut surface, now);
        assert!(effects.damage.is_none());
        assert_eq!(plan.stroke_i, 0);
        assert_eq!(plan.point_i, 0);
    }
}
