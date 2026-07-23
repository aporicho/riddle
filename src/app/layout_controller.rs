//! Background reply layout. Font rasterization and stroke tracing are CPU
//! heavy; the UI only polls immutable results and keeps servicing pen/display
//! events while a chunk is prepared.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use crate::fb::BBox;
use crate::fonts;

use super::reply::plan_reply;

pub(super) struct LayoutResult {
    pub(super) strokes: Vec<Vec<(f32, f32)>>,
    pub(super) region: BBox,
    pub(super) next_y: i32,
    pub(super) visible_graphemes: usize,
    pub(super) truncated: bool,
}

pub(super) enum LayoutPoll {
    Pending,
    Ready(LayoutResult),
    Failed,
}

pub(super) struct LayoutJob {
    rx: mpsc::Receiver<LayoutResult>,
    cancelled: Arc<AtomicBool>,
}

impl LayoutJob {
    pub(super) fn spawn(font: fonts::FontBook, text: String, y_start: Option<i32>) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            if worker_cancelled.load(Ordering::Acquire) {
                return;
            }
            let plan = plan_reply(&font, &text, y_start);
            if worker_cancelled.load(Ordering::Acquire) {
                return;
            }
            let strokes = plan.strokes.len();
            let points = plan.strokes.iter().map(Vec::len).sum::<usize>();
            let truncated = plan.truncated;
            let delivered = tx
                .send(LayoutResult {
                    strokes: plan.strokes,
                    region: plan.region,
                    next_y: plan.next_y,
                    visible_graphemes: plan.visible_graphemes,
                    truncated,
                })
                .is_ok();
            eprintln!(
                "magic-paper: event=reply-layout-ready latency_ms={} strokes={} points={} truncated={} delivered={}",
                started.elapsed().as_millis(),
                strokes,
                points,
                truncated,
                delivered,
            );
        });
        Self { rx, cancelled }
    }

    pub(super) fn poll(&self) -> LayoutPoll {
        match self.rx.try_recv() {
            Ok(result) => LayoutPoll::Ready(result),
            Err(mpsc::TryRecvError::Empty) => LayoutPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => LayoutPoll::Failed,
        }
    }

    #[cfg(test)]
    fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

impl Drop for LayoutJob {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::FontRef;

    fn font() -> fonts::FontBook {
        let font = FontRef::try_from_slice(include_bytes!("../../fonts/ChenYuluoyan-2.0-Thin.ttf"))
            .unwrap();
        fonts::FontBook::for_test(font, None)
    }

    #[test]
    fn dropping_layout_job_sets_its_cancellation_barrier() {
        crate::fb::test_init_screen();
        let job = LayoutJob::spawn(font(), "測試".into(), None);
        let flag = job.cancel_flag();
        drop(job);
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn layout_worker_returns_pretraced_strokes() {
        crate::fb::test_init_screen();
        let job = LayoutJob::spawn(font(), "測試".into(), None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match job.poll() {
                LayoutPoll::Ready(result) => {
                    assert!(!result.strokes.is_empty());
                    break;
                }
                LayoutPoll::Pending if std::time::Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                LayoutPoll::Pending | LayoutPoll::Failed => panic!("layout worker failed"),
            }
        }
    }
}
