//! Exact due-time scheduling and failure retry timing.

use std::time::{Duration, Instant};

use crate::tasks;

const HEARTBEAT_RETRY_DEFAULT: Duration = Duration::from_secs(30);

pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn heartbeat_retry_interval() -> Duration {
    let secs = std::env::var("MAGICPAPER_HEARTBEAT_RETRY_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(HEARTBEAT_RETRY_DEFAULT.as_secs())
        .max(5);
    Duration::from_secs(secs)
}

/// Smart heartbeat: sleep logically until the nearest active task is due.
/// No active task means no heartbeat and, crucially, no oracle/API request.
pub(super) fn heartbeat_deadline(task_store: &Option<tasks::TaskStore>) -> Option<Instant> {
    let due = task_store.as_ref()?.next_due()?;
    let wait = due.saturating_sub(unix_now());
    eprintln!("magic-paper: next task check in {wait}s");
    Some(Instant::now() + Duration::from_secs(wait))
}
