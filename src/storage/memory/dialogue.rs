//! Durable conversation boundaries and bounded Pi context extraction.

use super::{MemoryStore, PathBuf};
use crate::storage::persistence::{atomic_write, read_optional_utf8};
use std::io;

pub(super) const MAX_DIALOGUE_FIELD_BYTES: usize = 8 * 1024;
const MAX_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_DIALOGUE_FLOOR_BYTES: usize = 64;

/// Highest memory id excluded from automatic conversational context. The
/// catalog and history page deliberately ignore this boundary so old pages can
/// still be recalled explicitly after starting a fresh conversation.
const FLOOR_FILE: &str = "dialogue-floor";

impl MemoryStore {
    pub(super) fn dialogue_floor_path(&self) -> PathBuf {
        self.dir.join(FLOOR_FILE)
    }

    /// The last `n` turns as (transcript, reply) pairs, oldest first — the
    /// conversational memory that rides along with each request.
    pub fn recent_dialogue(&self, n: usize) -> Vec<(String, String)> {
        let floor = self.dialogue_floor();
        let mut bytes = 0_usize;
        let mut recent = Vec::new();
        for entry in self
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.id > floor && !entry.transcript.is_empty())
            .take(n)
        {
            let transcript = truncate_utf8(&entry.transcript, MAX_DIALOGUE_FIELD_BYTES);
            let reply = truncate_utf8(&entry.reply, MAX_DIALOGUE_FIELD_BYTES);
            let turn_bytes = transcript.len().saturating_add(reply.len());
            if bytes.saturating_add(turn_bytes) > MAX_CONTEXT_BYTES {
                break;
            }
            bytes += turn_bytes;
            recent.push((transcript.to_owned(), reply.to_owned()));
        }
        recent.reverse();
        recent
    }

    /// Start a durable dialogue epoch without deleting any remembered page.
    /// ReMagic can then recreate Pi at any later time without rehydrating the
    /// pages that were intentionally placed before this boundary.
    pub fn begin_dialogue_session(&mut self) -> io::Result<u64> {
        let _lock = self.lock_exclusive()?;
        self.load_unlocked()?;
        let floor = self.entries.iter().map(|entry| entry.id).max().unwrap_or(0);
        atomic_write(&self.dialogue_floor_path(), format!("{floor}\n").as_bytes())?;
        Ok(floor)
    }

    fn dialogue_floor(&self) -> u64 {
        match read_optional_utf8(&self.dialogue_floor_path(), MAX_DIALOGUE_FLOOR_BYTES) {
            Ok(None) => 0,
            Ok(Some(value)) => value.trim().parse().unwrap_or_else(|_| {
                eprintln!("magicpaper: invalid dialogue boundary; withholding old context");
                u64::MAX
            }),
            Err(error) => {
                eprintln!("magicpaper: could not read dialogue boundary: {error}");
                u64::MAX
            }
        }
    }
}

fn truncate_utf8(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
