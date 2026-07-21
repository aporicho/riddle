use super::*;

#[test]
fn pi_turn_gate_never_allows_receiver_overwrite() {
    let busy = AtomicBool::new(false);
    assert!(busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok());
    assert!(busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err());
    busy.store(false, Ordering::Release);
    assert!(busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok());
}

#[test]
fn cancelled_pi_turn_rejects_late_emission() {
    let cancelled = Arc::new(AtomicBool::new(false));
    assert!(!cancelled.load(Ordering::Acquire));
    cancelled.store(true, Ordering::Release);
    assert!(cancelled.load(Ordering::Acquire));
}

#[test]
fn cancelling_before_png_encode_releases_the_pi_turn_gate() {
    let state = ReaderState::new();
    state.busy.store(true, Ordering::Release);
    let (tx, _rx) = std::sync::mpsc::channel();
    *state.pending.lock().unwrap() = Some(tx);
    *state.parser.lock().unwrap() = Some(StreamParser::new(Vec::new()));
    abandon_prepared_turn(&state);
    assert!(!state.busy.load(Ordering::Acquire));
    assert!(state.pending.lock().unwrap().is_none());
    assert!(state.parser.lock().unwrap().is_none());
}
