use std::time::{Duration, Instant};

use super::super::super::oracle_controller::OracleTurn;
use super::super::super::state::State;
use super::super::{Engine, DRINK_STAGES, DRINK_STAGE_DELAY};
use crate::fb::BBox;
use crate::ink;

impl Engine<'_> {
    pub(super) fn tick_drinking(
        &mut self,
        stage: u32,
        next: Instant,
        region: BBox,
        rx: OracleTurn,
    ) -> State {
        if Instant::now() < next {
            return State::Drinking {
                stage,
                next,
                region,
                rx,
            };
        }
        let intent = ink::dissolve_frame(&mut self.surf, region, stage, DRINK_STAGES);
        let terminal = stage + 1 >= DRINK_STAGES;
        if terminal {
            self.refresh.present_cleanup(self.disp, region);
        } else {
            let (x, y, width, height) = region.rect();
            self.disp.present_region(x, y, width, height, intent);
        }
        if terminal {
            self.user_ink.clear();
            State::Thinking {
                rx,
                since: Instant::now(),
            }
        } else {
            State::Drinking {
                stage: stage + 1,
                next: Instant::now() + DRINK_STAGE_DELAY,
                region,
                rx,
            }
        }
    }

    pub(super) fn tick_fading(&mut self, stage: u32, next: Instant, region: BBox) -> State {
        const STAGES: u32 = 10;
        if Instant::now() < next {
            return State::FadingReply {
                stage,
                next,
                region,
            };
        }
        let intent = ink::dissolve_frame(&mut self.surf, region, stage, STAGES);
        let terminal = stage + 1 >= STAGES;
        if terminal {
            let full =
                self.refresh
                    .present_reply_cleanup(self.disp, self.surf.w, self.surf.h, region);
            eprintln!(
                "magic-paper: event=answer-cleanup refresh={}",
                if full { "full" } else { "partial" }
            );
        } else {
            let (x, y, width, height) = region.rect();
            self.disp.present_region(x, y, width, height, intent);
        }
        if terminal {
            if self.pen_down {
                State::AwaitingPenUp
            } else {
                State::Listening { last_pen: None }
            }
        } else {
            State::FadingReply {
                stage: stage + 1,
                next: Instant::now() + Duration::from_millis(80),
                region,
            }
        }
    }
}
