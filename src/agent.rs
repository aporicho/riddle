//! Screenless recurring-task executor.
//!
//! This process never opens display or input devices. It is the sole owner of
//! scheduled execution, regardless of which application is foreground, and
//! leaves resulting ink in a local queue. The UI only consumes that queue when
//! Master's page is idle; it never issues a competing heartbeat request.

use crate::{memory, oracle, tasks, todos};
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_REPLY_BYTES: usize = 32 * 1024;

fn queue_dir() -> PathBuf {
    crate::runtime_env::persistent_path(
        "MAGICPAPER_AGENT_QUEUE_DIR",
        "agent",
        "/home/root/.local/share/magicpaper/agent",
    )
}

fn queue_path() -> PathBuf {
    queue_dir().join("pending.tsv")
}

pub fn run() -> io::Result<()> {
    let _scheduler_lease = tasks::acquire_scheduler_lease()?;
    eprintln!("magic-paper-agent: sole scheduled-task owner ready");
    loop {
        let Some(mut task_store) = tasks::TaskStore::open() else {
            wait_for_task_change(Some(Duration::from_secs(5 * 60)));
            continue;
        };
        let now = unix_now();
        let due = task_store.due(now);
        if due.is_empty() {
            let wait = task_store
                .next_due()
                .map(|due| Duration::from_secs(due.saturating_sub(now).max(1)));
            wait_for_task_change(wait);
            continue;
        }

        let _work_lease = match WorkLease::begin("scheduled MagicPaper task", 180_000) {
            Ok(lease) => lease,
            Err(error) => {
                eprintln!("magic-paper-agent: power lease unavailable: {error}");
                wait_for_task_change(Some(Duration::from_secs(5 * 60)));
                continue;
            }
        };

        let oracle = match oracle::Oracle::spawn(true) {
            Ok(oracle) => oracle,
            Err(error) => {
                eprintln!("magic-paper-agent: oracle unavailable: {error}");
                wait_for_task_change(Some(Duration::from_secs(5 * 60)));
                continue;
            }
        };

        let context = build_context(&task_store);
        let prompt = tasks::heartbeat_prompt(&due);
        let (tx, rx) = mpsc::channel();
        let request = oracle.ask_text(&prompt, &context, tx);
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
                    request.cancel();
                    failed = true;
                    break;
                }
            }
        }
        if failed || reply.trim().is_empty() {
            wait_for_task_change(Some(Duration::from_secs(5 * 60)));
            continue;
        }
        if !task_store.complete_due_if_unchanged(&due, unix_now())? {
            eprintln!(
                "magic-paper-agent: due task changed while its answer was running; stale result discarded"
            );
            continue;
        }
        queue_reply(reply.trim())?;
        eprintln!(
            "magic-paper-agent: queued {} scheduled result(s)",
            due.len()
        );
    }
}

struct WorkLease {
    id: u64,
}

impl WorkLease {
    fn begin(reason: &str, requested_ms: u64) -> io::Result<Self> {
        let response = runtime_request(serde_json::json!({
            "version": 2,
            "request_id": format!("agent-work-{}", std::process::id()),
            "command": "begin_work",
            "class": "agent_turn",
            "reason": reason,
            "requested_ms": requested_ms,
        }))?;
        let id = response
            .get("lease")
            .and_then(|lease| lease.get("id"))
            .and_then(serde_json::Value::as_u64)
            .filter(|id| *id != 0)
            .ok_or_else(|| io::Error::other("manager did not return a work lease"))?;
        Ok(Self { id })
    }
}

impl Drop for WorkLease {
    fn drop(&mut self) {
        let _ = runtime_request(serde_json::json!({
            "version": 2,
            "request_id": format!("agent-finish-{}", std::process::id()),
            "command": "finish_work",
            "lease_id": self.id,
            "visible_result": false,
        }));
    }
}

fn runtime_request(request: serde_json::Value) -> io::Result<serde_json::Value> {
    let socket = std::env::var("REMAGIC_RUNTIME_SOCKET")
        .unwrap_or_else(|_| "/run/remagic/runtime-app.sock".into());
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(&mut stream, &request).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(64 * 1024)
        .read_line(&mut response)?;
    let value: serde_json::Value = serde_json::from_str(&response).map_err(io::Error::other)?;
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(value)
    } else {
        Err(io::Error::other(
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("manager rejected work lease"),
        ))
    }
}

/// Wait on the task directory because task persistence uses atomic rename.
/// With no active task this blocks forever and therefore produces zero timer
/// wakeups; the exact next due time is the only timeout for active tasks.
fn wait_for_task_change(timeout: Option<Duration>) {
    let directory = tasks::watch_dir();
    if fs::create_dir_all(&directory).is_err() {
        std::thread::sleep(timeout.unwrap_or(Duration::from_secs(60 * 60)));
        return;
    }
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if fd < 0 {
        std::thread::sleep(timeout.unwrap_or(Duration::from_secs(60 * 60)));
        return;
    }
    let path = match CString::new(directory.as_os_str().as_encoded_bytes()) {
        Ok(path) => path,
        Err(_) => {
            unsafe { libc::close(fd) };
            return;
        }
    };
    let mask = libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_TO
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_ATTRIB;
    if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } < 0 {
        unsafe { libc::close(fd) };
        std::thread::sleep(timeout.unwrap_or(Duration::from_secs(60 * 60)));
        return;
    }
    let timeout_ms = timeout.map_or(-1, |duration| {
        duration.as_millis().clamp(1, i32::MAX as u128) as i32
    });
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
    unsafe { libc::close(fd) };
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

fn queue_reply(reply: &str) -> io::Result<()> {
    fs::create_dir_all(queue_dir())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .truncate(false)
        .open(queue_path())?;
    lock(&file)?;
    writeln!(file, "{}\t{}", unix_now(), escape(reply))?;
    file.sync_all()?;
    unlock(&file);
    Ok(())
}

pub(crate) fn take_pending() -> io::Result<Option<String>> {
    fs::create_dir_all(queue_dir())?;
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(queue_path())?;
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
