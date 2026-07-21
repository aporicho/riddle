use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::super::lifecycle::{LifecycleCommand, LifecycleStage};
use super::super::oracle_controller::cancel_speculative;
use super::super::state::TurnKind;
use super::{suspend_visible_state, Engine, LifecycleExit};
use crate::fb::BBox;
use crate::platform::RefreshIntent;

impl Engine<'_> {
    pub(super) fn run_loop(&mut self, sigterm: &AtomicBool) {
        loop {
            if !self.poll_lifecycle() || sigterm.load(Ordering::Relaxed) {
                break;
            }
            if !self.lifecycle_foreground {
                if !self.wait_in_background() {
                    break;
                }
                continue;
            }
            if self.touch_requested_quit() || self.handle_power() {
                break;
            }
            self.drain_raw_pen();
            if !self.pump_host_events() {
                break;
            }
            self.open_reader_target();
            if !self.flush_live_ink() {
                break;
            }
            self.tick_state();
            self.stylus_tapped = false;
            self.wait_for_next_tick();
        }
    }

    fn poll_lifecycle(&mut self) -> bool {
        let commands = match self.lifecycle.poll() {
            Ok(commands) => commands,
            Err(error) => {
                self.fail(
                    LifecycleStage::Runtime,
                    format!("lifecycle protocol failed: {error}"),
                );
                return false;
            }
        };
        for command in commands {
            if !self.handle_lifecycle_command(command) {
                return false;
            }
        }
        true
    }

    fn handle_lifecycle_command(&mut self, command: LifecycleCommand) -> bool {
        match command {
            LifecycleCommand::Start => {
                eprintln!("magic-paper: lifecycle start accepted");
                true
            }
            LifecycleCommand::EnterForeground => self.enter_foreground(),
            LifecycleCommand::EnterBackground => self.enter_background(),
            LifecycleCommand::Shutdown { deadline_ms } => {
                eprintln!("magic-paper: lifecycle requested shutdown");
                self.lifecycle_exit = LifecycleExit::Complete {
                    deadline: Duration::from_millis(deadline_ms),
                };
                false
            }
        }
    }

    fn enter_foreground(&mut self) -> bool {
        if self.lifecycle_foreground {
            eprintln!("magic-paper: duplicate foreground command ignored");
            return true;
        }
        if let Err(error) = self.disp.pump() {
            self.fail(
                LifecycleStage::Foreground,
                format!("could not reset foreground input epoch: {error}"),
            );
            return false;
        }
        self.qtfb_pen.reset();
        self.primary_touch = None;
        self.pen_down = false;
        self.stylus_on = false;
        self.stylus_tapped = false;
        let sequence = self.lifecycle_frame_sequence.wrapping_add(1).max(1);
        if let Err(error) =
            self.disp
                .present_all_checked(self.surf.w, self.surf.h, RefreshIntent::Content)
        {
            self.fail(
                LifecycleStage::Foreground,
                format!("foreground frame commit failed: {error}"),
            );
            return false;
        }
        self.lifecycle_frame_sequence = sequence;
        if let Err(error) = self.lifecycle.report_ready_after_frame(sequence) {
            self.fail(
                LifecycleStage::Foreground,
                format!("could not report foreground readiness: {error}"),
            );
            return false;
        }
        self.lifecycle_foreground = true;
        self.input_priority.enter_foreground();
        eprintln!("magic-paper: lifecycle entered foreground after frame {sequence}");
        true
    }

    fn enter_background(&mut self) -> bool {
        if self.lifecycle_foreground {
            self.reset_for_background();
            if let Err(error) = self.disp.pump() {
                self.fail(
                    LifecycleStage::Background,
                    format!("could not drain background input epoch: {error}"),
                );
                return false;
            }
        }
        if let Err(error) = self.lifecycle.report_background_ready() {
            self.fail(
                LifecycleStage::Background,
                format!("could not report background readiness: {error}"),
            );
            return false;
        }
        eprintln!("magic-paper: lifecycle entered background");
        true
    }

    fn reset_for_background(&mut self) {
        self.lifecycle_foreground = false;
        self.input_priority.enter_background();
        self.oracle.invalidate_active_turn();
        cancel_speculative(&mut self.speculative, "application entered background");
        self.user_ink.pen_up();
        suspend_visible_state(&mut self.state, &mut self.surf, &mut self.user_ink);
        self.pen_down = false;
        self.qtfb_pen.reset();
        self.primary_touch = None;
        self.stylus_on = false;
        self.stylus_tapped = false;
        self.ink_dirty = BBox::empty();
        self.ink_flush_urgent = false;
        self.last_flush = Instant::now();
        self.speculative_attempted = false;
        self.reader_target = None;
        self.ui_scheduler_lease = None;
        self.turn_id = 0;
        self.turn_strokes.clear();
        self.turn_reply.clear();
        self.turn_transcript = None;
        self.turn_failed = false;
        self.turn_kind = TurnKind::User;
        self.turn_tasks.clear();
        self.pen_trace.finish("application-entered-background");
    }

    fn wait_in_background(&mut self) -> bool {
        if let Err(error) = self.disp.pump() {
            self.fail(
                LifecycleStage::Runtime,
                format!("display host disconnected in background: {error}"),
            );
            return false;
        }
        self.disp.wait(Duration::from_millis(25), false);
        true
    }
}
