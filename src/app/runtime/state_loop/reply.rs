use std::time::Instant;

use super::super::super::lists::{accept_transcript, apply_local_command};
use super::super::super::oracle_controller::{OracleTurn, StreamPoll};
use super::super::super::reply_controller::{ReplyCompletion, ReplyController};
use super::super::super::state::{State, TurnKind, WritePlan};
use super::super::super::timing::{heartbeat_deadline, unix_now};
use super::super::Engine;
use crate::oracle::Event;
use crate::platform::RefreshIntent;
use crate::{reader, runtime_control};

impl Engine<'_> {
    pub(super) fn tick_replying(
        &mut self,
        mut plan: WritePlan,
        next: Instant,
        mut rx: Option<OracleTurn>,
        mut page_full: bool,
    ) -> State {
        if let Some(turn) = rx.as_ref() {
            let close = self.consume_stream_poll(turn.poll_stream(), &mut plan, &mut page_full);
            if close {
                rx = None;
            }
        }
        let mut next = next;
        let effects = ReplyController::tick(
            &mut plan,
            &mut next,
            rx.is_some(),
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
                eprintln!("riddle: oracle failed mid-reply: {error}");
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
                ReplyController::append_text(&self.font, plan, page_full, &more);
            }
            Event::LocalCommand(command) => {
                self.turn_transcript = Some(command.clone());
                let (reply, tasks_changed) =
                    apply_local_command(&command, &mut self.task_store, &mut self.todo_store);
                if tasks_changed {
                    self.next_heartbeat = heartbeat_deadline(&self.task_store);
                }
                push_reply(&mut self.turn_reply, &reply);
                ReplyController::append_text(&self.font, plan, page_full, &reply);
            }
            Event::Reader(query) => self.open_delayed_reader(query.as_deref()),
            Event::FullRefresh => self.disp.request_refresh(self.surf.w, self.surf.h),
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
            | Event::HistoryList
            | Event::Help => {
                eprintln!("riddle: modal directive arrived after visible prose");
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
            "magic-paper: event=turn-render-complete kind={} reply_chars={} transcript_chars={} page_full={} memory_enabled={}",
            if self.turn_kind == TurnKind::User { "user" } else { "heartbeat" },
            self.turn_reply.chars().count(),
            self.turn_transcript.as_deref().map(str::chars).map(Iterator::count).unwrap_or(0),
            page_full,
            self.store.is_some(),
        );
        self.turn_strokes.clear();
        self.turn_tasks.clear();
        State::Lingering {
            until: Instant::now() + completion.linger,
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
