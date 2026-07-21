use super::worker_pool::take_worker_permit;
use super::*;

#[test]
fn worker_gate_never_queues_more_network_workers() {
    let gate = Arc::new(AtomicBool::new(false));
    let first = take_worker_permit(&gate).expect("first request owns the worker");
    assert!(take_worker_permit(&gate).is_none());
    drop(first);
    assert!(take_worker_permit(&gate).is_some());
}

#[test]
fn speculative_and_committed_worker_lanes_are_independent() {
    let speculative = Arc::new(AtomicBool::new(false));
    let committed = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let _stuck_preask = take_worker_permit(&speculative).unwrap();
    assert!(take_worker_permit(&speculative).is_none());

    let first = committed.iter().find_map(take_worker_permit).unwrap();
    let second = committed.iter().find_map(take_worker_permit).unwrap();
    assert!(committed.iter().find_map(take_worker_permit).is_none());
    drop(first);
    assert!(committed.iter().find_map(take_worker_permit).is_some());
    drop(second);
}
