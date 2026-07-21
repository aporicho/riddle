//! Bounded, priority-separated ownership of blocking HTTP workers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(super) struct WorkerPermit(Arc<AtomicBool>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerLane {
    /// The one-second OCR pre-request. A cancelled call may remain blocked in
    /// the transport, but it must never occupy a committed-user slot.
    Speculative,
    /// Foreground turns get a bounded overflow slot so a cancelled blocking
    /// transport cannot reject the writer's immediate correction.
    Interactive,
    /// Scheduled work is isolated from foreground handwriting entirely.
    Scheduled,
}

#[derive(Clone)]
pub(super) struct WorkerPools {
    speculative: Arc<AtomicBool>,
    interactive: [Arc<AtomicBool>; 2],
    scheduled: Arc<AtomicBool>,
}

impl Default for WorkerPools {
    fn default() -> Self {
        Self {
            speculative: Arc::new(AtomicBool::new(false)),
            interactive: [
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ],
            scheduled: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl WorkerPools {
    pub(super) fn acquire(&self, lane: WorkerLane) -> Option<WorkerPermit> {
        match lane {
            WorkerLane::Speculative => take_worker_permit(&self.speculative),
            WorkerLane::Interactive => self.interactive.iter().find_map(take_worker_permit),
            WorkerLane::Scheduled => take_worker_permit(&self.scheduled),
        }
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn take_worker_permit(gate: &Arc<AtomicBool>) -> Option<WorkerPermit> {
    gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| WorkerPermit(Arc::clone(gate)))
}
