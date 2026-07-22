//! MagicPaper's memory. Every finished turn is kept — the writer's actual pen
//! strokes, a transcription of their words, and MP's reply — so a later
//! incantation ("show me what I wrote about the garden") can conjure the page
//! back in the writer's own hand.
//!
//! Everything lives on the tablet, in plain files under
//! `/home/root/.local/share/magicpaper/memories` (override: `MAGICPAPER_MEMORY_DIR`):
//!
//!   index.tsv        one line per memory: id \t transcript \t reply
//!                    (tabs/newlines/backslashes escaped)
//!   <id>.strokes     the pen strokes: one line per stroke, "x,y,r;x,y,r;…"
//!
//! Delete the directory and the diary forgets. `MAGICPAPER_MEMORY=off` disables
//! remembering entirely (no storage, no context sent with requests).

use crate::storage::persistence::{
    atomic_write, invalid_line, lock_exclusive, read_optional_utf8, unescape_field, StoreLock,
};
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

/// Newest memories the diary keeps. Older pages are forgotten (pruned).
const MAX_MEMORIES: usize = 400;
/// Decimation: drop replay points closer than this (px) to the last kept one.
/// Handwriting stays faithful; files shrink several-fold.
const MIN_POINT_DIST2: i64 = 9;

pub type Strokes = Vec<Vec<(i32, i32, i32)>>;

#[derive(Clone)]
pub struct Entry {
    /// Unix seconds when the page was committed. Also the strokes filename.
    pub id: u64,
    pub transcript: String,
    pub reply: String,
}

pub struct MemoryStore {
    dir: PathBuf,
    pub entries: Vec<Entry>,
}

impl MemoryStore {
    /// Allocate a filename-safe Unix-second id without overwriting a turn that
    /// completed during the same second.
    pub fn next_id(&self, now: u64) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.id)
            .max()
            .map(|id| now.max(id.saturating_add(1)))
            .unwrap_or(now)
    }

    /// Open (or start) the diary's memory. Returns None when memory is off.
    pub fn open() -> Option<Self> {
        match std::env::var("MAGICPAPER_MEMORY").as_deref() {
            Ok("off") | Ok("0") | Ok("no") | Ok("false") => return None,
            _ => {}
        }
        let dir = crate::runtime_env::persistent_path(
            "MAGICPAPER_MEMORY_DIR",
            "memories",
            "/home/root/.local/share/magicpaper/memories",
        );
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("magicpaper: memory disabled ({}: {e})", dir.display());
            return None;
        }
        let mut store = Self {
            dir,
            entries: Vec::new(),
        };
        match store.load() {
            Ok(()) => Some(store),
            Err(error) => {
                eprintln!(
                    "magicpaper: memory disabled because {} could not be loaded: {error}",
                    store.index_path().display()
                );
                None
            }
        }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.tsv")
    }

    fn strokes_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.strokes"))
    }

    fn lock_exclusive(&self) -> io::Result<StoreLock> {
        lock_exclusive(&self.dir)
    }

    fn persist_index(&self, entries: &[Entry]) -> io::Result<()> {
        let mut out = String::new();
        for entry in entries {
            out.push_str(&format!(
                "{}\t{}\t{}\n",
                entry.id,
                escape(&entry.transcript),
                escape(&entry.reply)
            ));
        }
        atomic_write(&self.index_path(), out.as_bytes())
    }

    fn load(&mut self) -> io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.load_unlocked()
    }

    fn load_unlocked(&mut self) -> io::Result<()> {
        let path = self.index_path();
        let Some(contents) = read_optional_utf8(&path)? else {
            self.entries.clear();
            return Ok(());
        };
        let mut entries = Vec::new();
        let mut ids = HashSet::new();
        for (line_index, line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            if line.is_empty() {
                return Err(invalid_line(&path, line_number, "empty memory record"));
            }
            let columns = line.split('\t').collect::<Vec<_>>();
            if columns.len() != 3 {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "memory record must contain exactly three columns",
                ));
            }
            let id = columns[0]
                .parse::<u64>()
                .map_err(|_| invalid_line(&path, line_number, "invalid memory id"))?;
            if !ids.insert(id) {
                return Err(invalid_line(&path, line_number, "duplicate memory id"));
            }
            let transcript = unescape_field(columns[1])
                .map_err(|error| invalid_line(&path, line_number, &error.to_string()))?;
            let reply = unescape_field(columns[2])
                .map_err(|error| invalid_line(&path, line_number, &error.to_string()))?;
            entries.push(Entry {
                id,
                transcript,
                reply,
            });
            if entries.len() > MAX_MEMORIES {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "memory store exceeds its maximum size",
                ));
            }
        }
        self.entries = entries;
        Ok(())
    }

    /// Remember a finished turn. Strokes are decimated before writing.
    pub fn append(&mut self, id: u64, transcript: &str, reply: &str, strokes: &Strokes) {
        if let Err(error) = self.try_append(id, transcript, reply, strokes) {
            eprintln!("magicpaper: memory not kept: {error}");
        }
    }

    fn try_append(
        &mut self,
        requested_id: u64,
        transcript: &str,
        reply: &str,
        strokes: &Strokes,
    ) -> io::Result<u64> {
        let _lock = self.lock_exclusive()?;
        self.load_unlocked()?;
        let id = match self.entries.iter().map(|entry| entry.id).max() {
            Some(previous) if requested_id <= previous => {
                previous.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "memory id space is exhausted")
                })?
            }
            _ => requested_id,
        };
        let thin = decimate(strokes);
        let mut lines = String::new();
        for s in &thin {
            let mut first = true;
            for &(x, y, r) in s {
                if !first {
                    lines.push(';');
                }
                lines.push_str(&format!("{x},{y},{r}"));
                first = false;
            }
            lines.push('\n');
        }
        let strokes_path = self.strokes_path(id);
        if let Err(error) = atomic_write(&strokes_path, lines.as_bytes()) {
            // No index entry can refer to this id yet. If the replacement was
            // published but its final directory sync failed, remove that
            // otherwise-orphaned stroke file before reporting failure.
            let _ = std::fs::remove_file(&strokes_path);
            return Err(error);
        }
        let entry = Entry {
            id,
            transcript: transcript.to_string(),
            reply: reply.to_string(),
        };
        let mut updated = self.entries.clone();
        updated.push(entry);
        let drop_count = updated.len().saturating_sub(MAX_MEMORIES);
        let dropped = updated[..drop_count]
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let retained = updated.split_off(drop_count);
        if let Err(error) = self.persist_index(&retained) {
            let committed =
                self.load_unlocked().is_ok() && self.entries.iter().any(|entry| entry.id == id);
            if !committed {
                let _ = std::fs::remove_file(&strokes_path);
            }
            return Err(error);
        }
        self.entries = retained;
        for dropped_id in dropped {
            if let Err(error) = std::fs::remove_file(self.strokes_path(dropped_id)) {
                if error.kind() != io::ErrorKind::NotFound {
                    eprintln!("magicpaper: pruned memory index but not strokes: {error}");
                }
            }
        }
        Ok(id)
    }

    /// Load the pen strokes of one remembered page.
    pub fn strokes(&self, id: u64) -> Option<Strokes> {
        let text = std::fs::read_to_string(self.strokes_path(id)).ok()?;
        let mut strokes = Vec::new();
        for line in text.lines() {
            let mut stroke = Vec::new();
            for pt in line.split(';') {
                let mut n = pt.split(',');
                let (Some(x), Some(y), Some(r)) = (n.next(), n.next(), n.next()) else {
                    continue;
                };
                if let (Ok(x), Ok(y), Ok(r)) = (x.parse(), y.parse(), r.parse()) {
                    stroke.push((x, y, r));
                }
            }
            if !stroke.is_empty() {
                strokes.push(stroke);
            }
        }
        Some(strokes)
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The last `n` turns as (transcript, reply) pairs, oldest first — the
    /// conversational memory that rides along with each request.
    pub fn recent_dialogue(&self, n: usize) -> Vec<(String, String)> {
        self.entries
            .iter()
            .rev()
            .take(n)
            .filter(|e| !e.transcript.is_empty())
            .map(|e| (e.transcript.clone(), e.reply.clone()))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// The catalog shown to the oracle so it can pick a page to conjure:
    /// numbered newest-first. Returns (lines, ids) where ids[i] belongs to
    /// catalog number i+1.
    pub fn catalog(&self, max: usize) -> (Vec<String>, Vec<u64>) {
        let mut lines = Vec::new();
        let mut ids = Vec::new();
        for (i, e) in self.entries.iter().rev().take(max).enumerate() {
            let gist = if e.transcript.trim().is_empty() {
                format!("(reply: {})", one_line(&e.reply, 70))
            } else {
                one_line(&e.transcript, 70)
            };
            // One entry per line: the catalog is a numbered list the model
            // reads back, so a gist must never carry its own newline.
            lines.push(format!("{}. {} — {}", i + 1, spoken_date(e.id), gist));
            ids.push(e.id);
        }
        (lines, ids)
    }

    /// A compact newest-first local history page. Its visible numbering is
    /// also accepted by `delete_number`, independent of the model catalog.
    pub fn panel_lines(&self, max: usize) -> Vec<String> {
        self.entries
            .iter()
            .rev()
            .take(max)
            .enumerate()
            .map(|(index, entry)| {
                format!(
                    "{}  你：{}  MP：{}",
                    index + 1,
                    one_line(&entry.transcript, 36),
                    one_line(&entry.reply, 44)
                )
            })
            .collect()
    }

    /// Delete one of the newest `max` visible history rows and its strokes.
    pub fn delete_number(&mut self, number: usize, max: usize) -> Result<Entry, String> {
        let _lock = self
            .lock_exclusive()
            .map_err(|error| format!("lock memory store: {error}"))?;
        self.load_unlocked()
            .map_err(|error| format!("load memory store: {error}"))?;
        let visible = self.entries.len().min(max);
        if number == 0 || number > visible {
            return Err(format!(
                "history {number} does not exist (there are {visible} visible)"
            ));
        }
        let index = self.entries.len() - number;
        let entry = self.entries[index].clone();
        let mut updated = self.entries.clone();
        updated.remove(index);
        if let Err(error) = self.persist_index(&updated) {
            let fallback = self.entries.clone();
            if self.load_unlocked().is_err() {
                self.entries = fallback;
            }
            return Err(format!("save history deletion: {error}"));
        }
        self.entries = updated;
        if let Err(error) = std::fs::remove_file(self.strokes_path(entry.id)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("magicpaper: deleted history index but not strokes: {error}");
            }
        }
        Ok(entry)
    }
}

/// Collapse whitespace (incl. newlines) to single spaces and cap at `max`
/// chars, so a multi-line transcript stays one catalog line.
fn one_line(s: &str, max: usize) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

fn decimate(strokes: &Strokes) -> Strokes {
    strokes
        .iter()
        .map(|s| {
            let mut out: Vec<(i32, i32, i32)> = Vec::new();
            for (i, &(x, y, r)) in s.iter().enumerate() {
                let keep = match out.last() {
                    None => true,
                    Some(&(lx, ly, _)) => {
                        let (dx, dy) = ((x - lx) as i64, (y - ly) as i64);
                        dx * dx + dy * dy >= MIN_POINT_DIST2 || i == s.len() - 1
                    }
                };
                if keep {
                    out.push((x, y, r));
                }
            }
            out
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

/// "the 6th of July, in the evening" — how the diary speaks of a moment.
/// Local time via libc so the device's timezone is respected; the writer can
/// nudge it with MAGICPAPER_TZ_OFFSET (hours) if the tablet clock runs on UTC.
pub fn spoken_date(id: u64) -> String {
    let offset: i64 = std::env::var("MAGICPAPER_TZ_OFFSET")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|h| (h * 3600.0) as i64)
        .unwrap_or(0);
    let t = id as i64 + offset;
    let (y, mo, d, h) = civil(t);
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let suffix = match d {
        11..=13 => "th",
        _ => match d % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    let tod = match h {
        0..=4 => "in the small hours",
        5..=11 => "in the morning",
        12..=17 => "in the afternoon",
        18..=21 => "in the evening",
        _ => "late at night",
    };
    let _ = y;
    format!("the {d}{suffix} of {}, {tod}", MONTHS[(mo - 1) as usize])
}

/// Days-since-epoch to civil date (Howard Hinnant's algorithm) + hour of day.
fn civil(secs: i64) -> (i64, i64, i64, i64) {
    let days = secs.div_euclid(86400);
    let hour = secs.rem_euclid(86400) / 3600;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d, hour)
}

#[cfg(test)]
mod tests;
