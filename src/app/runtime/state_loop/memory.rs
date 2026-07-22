use std::time::{Duration, Instant};

use super::super::super::state::{ConjurePlan, State};
use super::super::Engine;
use crate::fb::{screen_h, screen_w, BBox};
use crate::platform::RefreshIntent;
use crate::surface::FADED;

impl Engine<'_> {
    pub(super) fn tick_conjuring(
        &mut self,
        mut plan: ConjurePlan,
        next: Instant,
        saved: Vec<u8>,
    ) -> State {
        if self.stylus_tapped {
            self.surf.paste_rect(0, 0, screen_w(), screen_h(), &saved);
            self.refresh
                .request_full(self.disp, self.surf.w, self.surf.h);
            return State::MemoryShown {
                saved: None,
                until: Instant::now(),
                region: plan.region,
            };
        }
        if Instant::now() < next {
            return State::Conjuring { plan, next, saved };
        }
        let dirty = draw_conjure_batch(&mut self.surf, &mut plan);
        if !dirty.is_empty() {
            let (x, y, width, height) = dirty.rect();
            self.disp
                .present_region(x, y, width, height, RefreshIntent::Ink);
        }
        if plan.stroke_i >= plan.strokes.len() {
            State::MemoryShown {
                saved: Some(saved),
                until: Instant::now() + Duration::from_secs(120),
                region: plan.region,
            }
        } else {
            State::Conjuring {
                plan,
                next: Instant::now() + Duration::from_millis(40),
                saved,
            }
        }
    }

    pub(super) fn tick_memory_shown(
        &mut self,
        saved: Option<Vec<u8>>,
        until: Instant,
        region: BBox,
    ) -> State {
        match saved {
            Some(saved) if self.stylus_tapped || Instant::now() >= until => {
                self.surf.paste_rect(0, 0, screen_w(), screen_h(), &saved);
                self.refresh
                    .request_full(self.disp, self.surf.w, self.surf.h);
                eprintln!("riddle: memory dismissed");
                State::MemoryShown {
                    saved: None,
                    until,
                    region,
                }
            }
            Some(saved) => State::MemoryShown {
                saved: Some(saved),
                until,
                region,
            },
            None if self.stylus_on => State::MemoryShown {
                saved: None,
                until,
                region,
            },
            None => State::Listening { last_pen: None },
        }
    }
}

fn draw_conjure_batch(surface: &mut crate::surface::Surface, plan: &mut ConjurePlan) -> BBox {
    let mut dirty = BBox::empty();
    // Match the reply renderer's e-ink cadence: fewer commits, same effective
    // point throughput, and no 100 Hz wakeup loop during a remembered page.
    let mut budget = 192;
    while budget > 0 && plan.stroke_i < plan.strokes.len() {
        let stroke = &plan.strokes[plan.stroke_i];
        if plan.point_i >= stroke.len() {
            plan.stroke_i += 1;
            plan.point_i = 0;
            continue;
        }
        let (x, y, radius) = stroke[plan.point_i];
        if plan.point_i > 0 {
            let (previous_x, previous_y, previous_radius) = stroke[plan.point_i - 1];
            surface.brush_line(
                previous_x,
                previous_y,
                x,
                y,
                radius.min(previous_radius + 1),
                FADED,
            );
        } else {
            surface.stamp(x, y, radius, FADED);
        }
        dirty.add(x, y, radius + 2);
        plan.point_i += 1;
        budget -= 1;
    }
    dirty
}
