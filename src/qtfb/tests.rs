use super::*;

fn pen_event(dev_id: i32, pressure: i32) -> InputEvent {
    InputEvent {
        input_type: INPUT_PEN_UPDATE,
        dev_id,
        x: 10,
        y: 20,
        d: pressure,
    }
}

#[test]
fn forwarded_pen_keeps_pressure_and_eraser_semantics() {
    assert_eq!(pen_event(0, 73).pen_tool(), crate::pen::Tool::Pen);
    assert_eq!(pen_event(0, 73).pressure_percent(), 73);
    assert_eq!(
        pen_event(PEN_DEVICE_ERASER, 40).pen_tool(),
        crate::pen::Tool::Eraser
    );
    assert_eq!(pen_event(0, -120).pen_tool(), crate::pen::Tool::Eraser);
    assert_eq!(pen_event(0, -120).pressure_percent(), 100);

    let frame = pen_event(PEN_DEVICE_ERASER, 50).to_pen_frame(9, PenPhase::Down);
    assert_eq!(frame.sequence, 9);
    assert_eq!(frame.phase, PenPhase::Down);
    assert_eq!(frame.tool, PenTool::Eraser);
    assert_eq!(frame.pressure, 2048);
}

#[test]
fn backpressure_is_returned_without_retrying_or_sleeping() {
    let mut attempts = 0;
    let error = send_packet_with(&[1, 2, 3], |_| {
        attempts += 1;
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(attempts, 1);
}

#[test]
fn interrupted_send_is_retried_but_never_shortened() {
    let mut attempts = 0;
    send_packet_with(&[1, 2, 3], |packet| {
        attempts += 1;
        if attempts == 1 {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(packet.len())
        }
    })
    .unwrap();
    assert_eq!(attempts, 2);
}

#[test]
fn pending_damage_merges_and_full_update_dominates() {
    let first = PendingUpdate::Partial(DamageRect {
        x: 10,
        y: 20,
        width: 5,
        height: 6,
    });
    let second = PendingUpdate::Partial(DamageRect {
        x: 4,
        y: 24,
        width: 20,
        height: 10,
    });
    assert_eq!(
        first.merge(second),
        PendingUpdate::Partial(DamageRect {
            x: 4,
            y: 20,
            width: 20,
            height: 14,
        })
    );
    assert_eq!(first.merge(PendingUpdate::All), PendingUpdate::All);

    let transient = PendingCommit {
        update: first,
        refresh_mode: REFRESH_MODE_UFAST,
    };
    let stable = PendingCommit {
        update: second,
        refresh_mode: REFRESH_MODE_CONTENT,
    };
    let merged = transient.merge(stable);
    assert_eq!(merged.refresh_mode, REFRESH_MODE_CONTENT);
    assert_eq!(
        merged.update,
        PendingUpdate::Partial(DamageRect {
            x: 4,
            y: 20,
            width: 20,
            height: 14,
        })
    );
}
