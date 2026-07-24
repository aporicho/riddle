//! Asynchronous `open_app` client used for MagicPaper → KOReader handoff.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde::Serialize;

use super::{next_request_id, request_acknowledged, runtime_socket, validate_app_token};
use crate::platform::AppToken;

/// Runtime App v2 acknowledges queue admission, not target foreground.
pub(super) const OPEN_APP_TIMEOUT: Duration = Duration::from_secs(2);

pub enum OpenReaderPoll {
    Pending,
    Accepted,
    Failed(io::Error),
}

/// Dropping the receiver is the lifecycle fence: an ACK that arrives after
/// background can no longer mutate the foreground state machine.
pub struct OpenReaderRequest {
    result: mpsc::Receiver<io::Result<()>>,
    cancelled: Arc<AtomicBool>,
    token: AppToken,
    request_id: String,
}

impl OpenReaderRequest {
    pub fn poll(&self) -> OpenReaderPoll {
        match self.result.try_recv() {
            Ok(Ok(())) => OpenReaderPoll::Accepted,
            Ok(Err(error)) => OpenReaderPoll::Failed(error),
            Err(mpsc::TryRecvError::Empty) => OpenReaderPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => OpenReaderPoll::Failed(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "runtime request worker stopped without a result",
            )),
        }
    }

    pub fn token(&self) -> &AppToken {
        &self.token
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[cfg(test)]
    pub(super) fn cancellation_probe(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    #[cfg(test)]
    pub(crate) fn pending_for_test(token: AppToken) -> (Self, Arc<AtomicBool>) {
        let (_tx, result) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        (
            Self {
                result,
                cancelled: Arc::clone(&cancelled),
                token,
                request_id: "test-pending".into(),
            },
            cancelled,
        )
    }
}

impl Drop for OpenReaderRequest {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

pub fn open_reader(path: &Path, token: &AppToken) -> io::Result<OpenReaderRequest> {
    if !external_reader_allowed(
        crate::runtime_env::test_mode(),
        test_event_file_configured(),
    ) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "external reader requests are disabled in MAGICPAPER_TEST_MODE",
        ));
    }
    validate_app_token(token)?;
    start_open_reader_at(path, token, runtime_socket(), OPEN_APP_TIMEOUT)
}

pub(super) fn start_open_reader_at(
    path: &Path,
    token: &AppToken,
    socket: String,
    timeout: Duration,
) -> io::Result<OpenReaderRequest> {
    let path = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "KOReader path is not valid UTF-8",
        )
    })?;
    let request_id = next_request_id();
    let request = open_reader_request(&request_id, path, token)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (tx, result) = mpsc::sync_channel(1);
    let worker_request_id = request_id.clone();
    std::thread::Builder::new()
        .name("magicpaper-open-reader".into())
        .spawn(move || {
            let outcome = request_acknowledged(&socket, &request, timeout, |ack| {
                open_app_acknowledgement_ok(ack, &worker_request_id)
            });
            if !worker_cancelled.load(Ordering::Acquire) {
                let _ = tx.send(outcome);
            }
        })?;
    Ok(OpenReaderRequest {
        result,
        cancelled,
        token: token.clone(),
        request_id,
    })
}

fn external_reader_allowed(test_mode: bool, automation_configured: bool) -> bool {
    !test_mode || automation_configured
}

fn test_event_file_configured() -> bool {
    std::env::var_os("MAGICPAPER_TEST_EVENT_FILE").is_some_and(|path| !path.is_empty())
}

#[derive(Serialize)]
struct OpenAppRequest<'a> {
    version: u8,
    request_id: &'a str,
    command: &'static str,
    app: &'static str,
    open_path: &'a str,
    token: OpenAppToken<'a>,
}

#[derive(Serialize)]
struct OpenAppToken<'a> {
    app_id: &'a str,
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
}

pub(super) fn open_reader_request(
    request_id: &str,
    path: &str,
    token: &AppToken,
) -> io::Result<Vec<u8>> {
    super::json_line(&OpenAppRequest {
        version: 2,
        request_id,
        command: "open_app",
        app: "koreader",
        open_path: path,
        token: OpenAppToken {
            app_id: &token.app_id,
            generation: token.generation,
            foreground_epoch: token.foreground_epoch,
            lease_id: token.lease_id.unwrap_or_default(),
        },
    })
}

pub(super) fn open_app_acknowledgement_ok(json: &str, request_id: &str) -> bool {
    let Ok(ack) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    ack.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
        && ack.get("status").and_then(serde_json::Value::as_str) == Some("accepted")
        && ack.get("request_id").and_then(serde_json::Value::as_str) == Some(request_id)
}

#[cfg(test)]
pub(super) fn external_reader_allowed_for_test(
    test_mode: bool,
    automation_configured: bool,
) -> bool {
    external_reader_allowed(test_mode, automation_configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn dropping_an_awaiting_request_fences_a_late_acknowledgement() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!("magicpaper-cancelled-{nonce}.sock"));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request);
            std::thread::sleep(Duration::from_millis(20));
            let _ = stream
                .write_all(b"{\"ok\":true,\"status\":\"accepted\",\"request_id\":\"late\"}\n");
        });
        let token = AppToken {
            app_id: "magicpaper".into(),
            generation: 7,
            foreground_epoch: 11,
            lease_id: Some(13),
        };
        let request = start_open_reader_at(
            Path::new("/books/late.epub"),
            &token,
            socket.to_string_lossy().into_owned(),
            Duration::from_millis(100),
        )
        .unwrap();
        let cancelled = request.cancellation_probe();
        drop(request);
        assert!(cancelled.load(Ordering::Acquire));
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
