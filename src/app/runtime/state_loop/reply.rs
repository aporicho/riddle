use std::time::Instant;

use super::super::super::lists::{accept_transcript, apply_local_command};
use super::super::super::oracle_controller::{OracleTurn, StreamPoll};
use super::super::super::reply::{append_overflow_marker, recenter_undrawn_reply};
use super::super::super::reply_controller::{
    answer_visible_duration, ReplyCompletion, ReplyController,
};
use super::super::super::state::{State, TurnKind, WritePlan};
use super::super::super::timing::{heartbeat_deadline, unix_now};
use super::super::Engine;
use crate::oracle::Event;
use crate::platform::RefreshIntent;
use crate::{reader, runtime_control};

const MAX_READY_STREAM_EVENTS_PER_TICK: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrainOutcome {
    Pending,
    Boundary,
    Saturated,
}

/// Consume a bounded batch without confusing "exactly hit the event budget"
/// with "the producer is still open". A saturated initial batch gets one more
/// UI tick to observe Pending/Closed before committing its vertical anchor.
fn drain_ready_stream(
    mut poll: impl FnMut() -> StreamPoll,
    mut consume: impl FnMut(StreamPoll) -> bool,
) -> DrainOutcome {
    for _ in 0..MAX_READY_STREAM_EVENTS_PER_TICK {
        let item = poll();
        let pending = matches!(&item, StreamPoll::Pending);
        if consume(item) {
            return DrainOutcome::Boundary;
        }
        if pending {
            return DrainOutcome::Pending;
        }
    }
    DrainOutcome::Saturated
}

impl Engine<'_> {
    pub(super) fn tick_replying(
        &mut self,
        mut plan: WritePlan,
        next: Instant,
        mut rx: Option<OracleTurn>,
        mut page_full: bool,
    ) -> State {
        // Drain the small semantic-event queue before deciding initial layout.
        // A stream that completed just after its first sentence can therefore
        // be centered as one block without paying one UI tick per chunk.
        let drain = match rx.as_ref() {
            Some(turn) => drain_ready_stream(
                || turn.poll_stream(),
                |poll| self.consume_stream_poll(poll, &mut plan, &mut page_full),
            ),
            None => DrainOutcome::Pending,
        };
        if drain == DrainOutcome::Boundary {
            rx = None;
            if recenter_undrawn_reply(&self.font, &mut plan, &self.turn_reply) {
                // A provisional top-safe fit may have rejected a tail that the
                // complete centered fit can retain at one uniform smaller size.
                page_full = false;
            }
        } else if drain == DrainOutcome::Saturated && plan.initial_buffering {
            // The 32nd item may have been the final queued Event; defer layout
            // for one reactor tick so the following Closed can be observed.
            return State::Replying {
                plan,
                next,
                rx,
                page_full,
            };
        }
        let mut next = next;
        let effects = ReplyController::tick(
            &mut plan,
            &mut next,
            rx.is_some(),
            &mut page_full,
            &mut self.surf,
            Instant::now(),
        );
        if let Some(damage) = effects.damage {
            self.disp.present_region(
                damage.x,
                damage.y,
                damage.width,
                damage.height,
                RefreshIntent::Ink,
            );
            if let Some(latency_ms) = effects.first_damage_latency_ms {
                eprintln!("magic-paper: event=reply-first-display-submit latency_ms={latency_ms}");
            }
        }
        if let Some(completion) = effects.completion {
            self.complete_reply(completion, page_full)
        } else {
            State::Replying {
                plan,
                next,
                rx,
                page_full,
            }
        }
    }

    fn consume_stream_poll(
        &mut self,
        poll: StreamPoll,
        plan: &mut WritePlan,
        page_full: &mut bool,
    ) -> bool {
        match poll {
            StreamPoll::Event(event) => self.consume_reply_event(event, plan, page_full),
            StreamPoll::Error { request_id, error } => {
                eprintln!(
                    "magic-paper: event=turn-error request={} stage=mid-reply error={:?}",
                    request_id,
                    error.lines().next().unwrap_or("unknown error")
                );
                eprintln!("magicpaper: oracle failed mid-reply: {error}");
                self.turn_failed = true;
                true
            }
            StreamPoll::Closed { request_id } => {
                eprintln!("magic-paper: event=turn-stream-closed request={request_id} phase=reply");
                true
            }
            StreamPoll::Stale { request_id } => {
                eprintln!(
                    "magic-paper: event=turn-discarded request={request_id} reason=stale-generation"
                );
                true
            }
            StreamPoll::Pending => false,
        }
    }

    fn consume_reply_event(
        &mut self,
        event: Event,
        plan: &mut WritePlan,
        page_full: &mut bool,
    ) -> bool {
        match event {
            Event::Ink(more) => {
                push_reply(&mut self.turn_reply, &more);
                let was_page_full = *page_full;
                ReplyController::append_text(&self.font, plan, page_full, &more);
                if !was_page_full && *page_full {
                    append_overflow_marker(&self.font, plan);
                }
            }
            Event::LocalCommand(command) => {
                self.turn_transcript = Some(command.clone());
                let (reply, tasks_changed) =
                    apply_local_command(&command, &mut self.task_store, &mut self.todo_store);
                if tasks_changed {
                    self.next_heartbeat = heartbeat_deadline(&self.task_store);
                }
                push_reply(&mut self.turn_reply, &reply);
                let was_page_full = *page_full;
                ReplyController::append_text(&self.font, plan, page_full, &reply);
                if !was_page_full && *page_full {
                    append_overflow_marker(&self.font, plan);
                }
            }
            Event::Reader(query) => self.open_delayed_reader(query.as_deref()),
            Event::FullRefresh => self
                .refresh
                .request_full(self.disp, self.surf.w, self.surf.h),
            Event::Transcript(transcript) => {
                if accept_transcript(
                    &mut self.turn_transcript,
                    transcript,
                    self.turn_kind,
                    &mut self.task_store,
                    &mut self.todo_store,
                ) {
                    self.next_heartbeat = heartbeat_deadline(&self.task_store);
                }
            }
            Event::Show(_)
            | Event::TaskList
            | Event::TodoList
            | Event::FontList
            | Event::Settings
            | Event::HistoryList
            | Event::Help => {
                eprintln!("magicpaper: modal directive arrived after visible prose");
            }
        }
        false
    }

    fn open_delayed_reader(&self, query: Option<&str>) {
        match reader::Catalog::open().map(|catalog| catalog.lookup(query)) {
            Ok(reader::Lookup::Open(path)) => {
                let _ = runtime_control::open_reader(&path).map_err(|error| {
                    eprintln!("magic-paper: delayed KOReader request failed: {error}")
                });
            }
            Ok(reader::Lookup::Choose(_) | reader::Lookup::Missing) => {
                eprintln!("magic-paper: delayed reader directive was ambiguous");
            }
            Err(error) => eprintln!("magic-paper: delayed reader catalog failed: {error}"),
        }
    }

    fn complete_reply(&mut self, completion: ReplyCompletion, page_full: bool) -> State {
        if !self.turn_failed && !self.turn_reply.is_empty() {
            match self.turn_kind {
                TurnKind::User => self.persist_user_turn(),
                TurnKind::Heartbeat => self.complete_heartbeat(),
            }
        }
        if self.turn_kind == TurnKind::Heartbeat {
            self.ui_scheduler_lease = None;
        }
        eprintln!(
            "magic-paper: event=turn-render-complete kind={} reply_chars={} transcript_chars={} page_full={} memory_enabled={} reply_elapsed_ms={}",
            if self.turn_kind == TurnKind::User { "user" } else { "heartbeat" },
            self.turn_reply.chars().count(),
            self.turn_transcript.as_deref().map(str::chars).map(Iterator::count).unwrap_or(0),
            page_full,
            self.store.is_some(),
            completion.reply_elapsed_ms,
        );
        self.turn_strokes.clear();
        self.turn_tasks.clear();
        State::AnswerVisible {
            until: Instant::now()
                + answer_visible_duration(
                    completion.visible_graphemes,
                    self.refresh.values().answer_dwell_percent,
                ),
            region: completion.region,
        }
    }

    fn persist_user_turn(&mut self) {
        if let Some(store) = self.store.as_mut() {
            store.append(
                self.turn_id,
                self.turn_transcript.as_deref().unwrap_or(""),
                self.turn_reply.trim(),
                &self.turn_strokes,
            );
        }
    }

    fn complete_heartbeat(&mut self) {
        let Some(store) = self.task_store.as_mut() else {
            return;
        };
        match store.complete_due_if_unchanged(&self.turn_tasks, unix_now()) {
            Err(error) => eprintln!("magic-paper: could not advance tasks: {error}"),
            Ok(false) => {
                eprintln!("magic-paper: heartbeat result discarded because its task changed")
            }
            Ok(true) => {
                self.next_heartbeat = heartbeat_deadline(&self.task_store);
                eprintln!(
                    "magic-paper: completed {} heartbeat task(s)",
                    self.turn_tasks.len()
                );
            }
        }
    }
}

fn push_reply(target: &mut String, text: &str) {
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(text);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{drain_ready_stream, DrainOutcome, MAX_READY_STREAM_EVENTS_PER_TICK};
    use crate::app::oracle_controller::StreamPoll;
    use crate::oracle::Event;

    #[test]
    fn saturated_batch_defers_layout_until_closed_can_be_observed() {
        let mut polls = (0..MAX_READY_STREAM_EVENTS_PER_TICK)
            .map(|index| StreamPoll::Event(Event::Ink(format!("chunk-{index}"))))
            .chain(std::iter::once(StreamPoll::Closed { request_id: 7 }))
            .collect::<VecDeque<_>>();
        let mut consumed = 0;
        let first = drain_ready_stream(
            || polls.pop_front().expect("poll fixture exhausted"),
            |poll| {
                consumed += 1;
                matches!(poll, StreamPoll::Closed { .. })
            },
        );
        assert_eq!(first, DrainOutcome::Saturated);
        assert_eq!(consumed, MAX_READY_STREAM_EVENTS_PER_TICK);
        assert_eq!(polls.len(), 1);

        let second = drain_ready_stream(
            || polls.pop_front().expect("closed fixture missing"),
            |poll| {
                consumed += 1;
                matches!(poll, StreamPoll::Closed { .. })
            },
        );
        assert_eq!(second, DrainOutcome::Boundary);
        assert_eq!(consumed, MAX_READY_STREAM_EVENTS_PER_TICK + 1);
        assert!(polls.is_empty());
    }

    #[test]
    fn ready_batch_stops_immediately_at_pending() {
        let mut polls = VecDeque::from([
            StreamPoll::Event(Event::Ink("ready".into())),
            StreamPoll::Pending,
            StreamPoll::Event(Event::Ink("later".into())),
        ]);
        let outcome = drain_ready_stream(
            || polls.pop_front().expect("poll fixture exhausted"),
            |_| false,
        );
        assert_eq!(outcome, DrainOutcome::Pending);
        assert_eq!(polls.len(), 1);
    }
}
