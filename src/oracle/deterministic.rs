//! Offline oracle used only by explicit device automation. It never examines
//! credentials, starts pi, submits OCR, or opens a socket.

use std::sync::mpsc::Sender;

use super::Event;

pub(crate) struct DeterministicOracle;

impl DeterministicOracle {
    pub(super) fn handwriting(tx: Sender<Result<Event, String>>) {
        let _ = tx.send(Ok(Event::Ink("測試回覆".into())));
        let _ = tx.send(Ok(Event::Transcript("測試輸入".into())));
    }

    pub(super) fn scheduled(tx: Sender<Result<Event, String>>) {
        let _ = tx.send(Ok(Event::Ink("測試排程回覆".into())));
    }
}
