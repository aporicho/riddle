//! Offline oracle used only by explicit device automation. It never examines
//! credentials, starts pi, submits OCR, or opens a socket.

use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::mpsc::Sender;

use super::Event;

pub(crate) struct DeterministicOracle;

impl DeterministicOracle {
    pub(super) fn handwriting(tx: Sender<Result<Event, String>>) {
        if consume_test_event().as_deref() == Some("read") {
            eprintln!("magic-paper: event=test-event-consumed command=read");
            let _ = tx.send(Ok(Event::Reader(None)));
            return;
        }
        let _ = tx.send(Ok(Event::Ink("測試回覆".into())));
        let _ = tx.send(Ok(Event::Transcript("測試輸入".into())));
    }

    pub(super) fn scheduled(tx: Sender<Result<Event, String>>) {
        let _ = tx.send(Ok(Event::Ink("測試排程回覆".into())));
    }
}

fn consume_test_event() -> Option<String> {
    let path = std::env::var_os("MAGICPAPER_TEST_EVENT_FILE")?;
    if path.is_empty() {
        return None;
    }
    consume_test_event_at(std::path::Path::new(&path))
}

fn consume_test_event_at(path: &std::path::Path) -> Option<String> {
    const MAX_EVENT_BYTES: u64 = 64;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || opened.len() > MAX_EVENT_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(MAX_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes != b"read\n" {
        return None;
    }
    let current = std::fs::symlink_metadata(path).ok()?;
    if current.dev() != opened.dev() || current.ino() != opened.ino() {
        return None;
    }
    std::fs::remove_file(path).ok()?;
    Some("read".into())
}

#[cfg(test)]
mod tests {
    use super::consume_test_event_at;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn marker(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("magicpaper-event-{name}-{nonce}"))
    }

    #[test]
    fn exact_read_marker_is_consumed_once() {
        let path = marker("read");
        std::fs::write(&path, b"read\n").unwrap();
        assert_eq!(consume_test_event_at(&path).as_deref(), Some("read"));
        assert!(!path.exists());
        assert_eq!(consume_test_event_at(&path), None);
    }

    #[test]
    fn non_exact_or_oversized_marker_is_never_consumed() {
        for bytes in [b"READ\n".as_slice(), &[b'x'; 65]] {
            let path = marker("invalid");
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(consume_test_event_at(&path), None);
            assert!(path.exists());
            std::fs::remove_file(path).unwrap();
        }
    }
}
