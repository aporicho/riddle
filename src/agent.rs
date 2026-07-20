//! Screenless recurring-task executor.
//!
//! This process never opens display or input devices. It runs due tasks while
//! another foreground domain is active and leaves the resulting ink in a
//! local queue. The UI consumes that queue only when Master's page is idle.

use crate::{memory, oracle, tasks, todos};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FOREGROUND_MARKER: &str = "/run/remagic/foreground-app";
const QUEUE_DIR: &str = "/home/root/riddle-data/agent";
const QUEUE_PATH: &str = "/home/root/riddle-data/agent/pending.tsv";
const MAX_REPLY_BYTES: usize = 32 * 1024;

pub fn run() -> io::Result<()> {
    eprintln!("magic-paper-agent: screenless task worker ready");
    let oracle = loop {
        match oracle::Oracle::spawn(true) {
            Ok(oracle) => break oracle,
            Err(error) => {
                eprintln!("magic-paper-agent: oracle unavailable: {error}");
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    };

    loop {
        if magicpaper_is_foreground() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        let Some(mut task_store) = tasks::TaskStore::open() else {
            std::thread::sleep(Duration::from_secs(30));
            continue;
        };
        let now = unix_now();
        let due = task_store.due(now);
        if due.is_empty() {
            let wait = task_store
                .next_due()
                .map(|due| due.saturating_sub(now).clamp(1, 30))
                .unwrap_or(30);
            std::thread::sleep(Duration::from_secs(wait));
            continue;
        }

        let ids: Vec<u64> = due.iter().map(|task| task.id).collect();
        let context = build_context(&task_store);
        let prompt = tasks::heartbeat_prompt(&due);
        let (tx, rx) = mpsc::channel();
        oracle.ask_text(&prompt, &context, tx);
        let mut reply = String::new();
        let mut failed = false;
        loop {
            match rx.recv_timeout(Duration::from_secs(180)) {
                Ok(Ok(oracle::Event::Ink(chunk))) => {
                    if reply.len() + chunk.len() <= MAX_REPLY_BYTES {
                        reply.push_str(&chunk);
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    eprintln!("magic-paper-agent: task request failed: {error}");
                    failed = true;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    eprintln!("magic-paper-agent: task request timed out");
                    failed = true;
                    break;
                }
            }
        }
        if failed || reply.trim().is_empty() {
            std::thread::sleep(Duration::from_secs(30));
            continue;
        }
        queue_reply(reply.trim())?;
        task_store.mark_ran(&ids, unix_now())?;
        eprintln!(
            "magic-paper-agent: queued {} scheduled result(s)",
            ids.len()
        );
    }
}

fn build_context(task_store: &tasks::TaskStore) -> oracle::TurnContext {
    let memory_store = memory::MemoryStore::open();
    let todo_store = todos::TodoStore::open();
    let (history, catalog_lines, catalog_ids) = match memory_store {
        Some(store) => {
            let (lines, ids) = store.catalog(40);
            (store.recent_dialogue(20), lines, ids)
        }
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    oracle::TurnContext {
        history,
        catalog_lines,
        catalog_ids,
        task_lines: task_store.catalog_lines(),
        todo_lines: todo_store
            .as_ref()
            .map(|store| store.catalog_lines())
            .unwrap_or_default(),
    }
}

fn magicpaper_is_foreground() -> bool {
    fs::read_to_string(FOREGROUND_MARKER).is_ok_and(|value| value.trim() == "magicpaper")
}

fn queue_reply(reply: &str) -> io::Result<()> {
    fs::create_dir_all(QUEUE_DIR)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(QUEUE_PATH)?;
    lock(&file)?;
    writeln!(file, "{}\t{}", unix_now(), escape(reply))?;
    file.sync_all()?;
    unlock(&file);
    Ok(())
}

pub(crate) fn take_pending() -> io::Result<Option<String>> {
    fs::create_dir_all(QUEUE_DIR)?;
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(QUEUE_PATH)?;
    lock(&file)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let mut lines = contents.lines();
    let first = lines.next().and_then(|line| line.split_once('\t'));
    let reply = first.map(|(_, encoded)| unescape(encoded));
    if reply.is_some() {
        let remaining: String = lines.map(|line| format!("{line}\n")).collect();
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(remaining.as_bytes())?;
        file.sync_all()?;
    }
    unlock(&file);
    Ok(reply)
}

fn lock(file: &fs::File) -> io::Result<()> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unlock(file: &fs::File) {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn unescape(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') => output.push('\\'),
            Some(other) => output.push(other),
            None => output.push('\\'),
        }
    }
    output
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_escaping_round_trips() {
        let original = "第一行\nsecond\tline\\end";
        assert_eq!(unescape(&escape(original)), original);
    }
}
