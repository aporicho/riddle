//! Small client for application-to-runtime lifecycle requests.
//!
//! The protocol is deliberately one request and one acknowledgement per Unix
//! stream connection.  A newline terminates each compact JSON object, which
//! makes the boundary explicit without adding a JSON dependency to the device
//! binary.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::platform::{AppToken, InputMode};

const DEFAULT_SOCKET: &str = "/run/remagic/runtime-app.sock";
const MAX_ACK_BYTES: usize = 8 * 1024;
const INPUT_MODE_TIMEOUT: Duration = Duration::from_secs(8);
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

mod handoff;
pub use handoff::{open_reader, OpenReaderPoll, OpenReaderRequest};

/// Switch the manager-owned input overlay and require a correlated ACK.
///
/// This is intentionally stricter than the legacy `open_app` acknowledgement:
/// accepting an uncorrelated or mismatched mode could leave host live ink on
/// while MagicPaper believes an animation is protected.
pub fn set_input_mode(token: &AppToken, mode: InputMode) -> io::Result<()> {
    validate_app_token(token)?;
    let socket = runtime_socket();
    let request_id = next_request_id();
    let request = input_mode_request(&request_id, token, mode)?;
    request_acknowledged(&socket, &request, INPUT_MODE_TIMEOUT, |ack| {
        input_mode_acknowledgement_ok(ack, &request_id, token, mode)
    })
}

#[derive(Serialize)]
struct WireToken<'a> {
    app_id: &'a str,
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
}

impl<'a> From<&'a AppToken> for WireToken<'a> {
    fn from(token: &'a AppToken) -> Self {
        Self {
            app_id: &token.app_id,
            generation: token.generation,
            foreground_epoch: token.foreground_epoch,
            lease_id: token.lease_id.unwrap_or_default(),
        }
    }
}

#[derive(Serialize)]
struct InputModeRequest<'a> {
    version: u8,
    request_id: &'a str,
    command: &'static str,
    token: WireToken<'a>,
    mode: &'static str,
}

fn input_mode_request(request_id: &str, token: &AppToken, mode: InputMode) -> io::Result<Vec<u8>> {
    json_line(&InputModeRequest {
        version: 2,
        request_id,
        command: "set_input_mode",
        token: token.into(),
        mode: mode.as_str(),
    })
}

fn json_line(value: &impl Serialize) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_app_token(token: &AppToken) -> io::Result<()> {
    if token.app_id != "magicpaper"
        || token.generation == 0
        || token.foreground_epoch == 0
        || token.lease_id.is_none_or(|lease_id| lease_id == 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime request requires the exact MagicPaper foreground token",
        ));
    }
    Ok(())
}

fn runtime_socket() -> String {
    std::env::var("REMAGIC_RUNTIME_SOCKET")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SOCKET.to_owned())
}

fn next_request_id() -> String {
    format!(
        "magicpaper-{}-{}",
        std::process::id(),
        REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn request_acknowledged(
    socket: &str,
    request: &[u8],
    timeout: Duration,
    validate: impl FnOnce(&str) -> bool,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut stream = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("connect to runtime control socket {socket}: {error}"),
        )
    })?;
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    stream.write_all(request)?;
    stream.flush()?;

    let mut ack = Vec::new();
    let mut chunk = [0u8; 512];
    while ack.len() < MAX_ACK_BYTES {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(size) => {
                let end = chunk[..size]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(size);
                let available = MAX_ACK_BYTES.saturating_sub(ack.len());
                ack.extend_from_slice(&chunk[..end.min(available)]);
                if end < size {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "runtime acknowledgement deadline elapsed",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    if ack.len() == MAX_ACK_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime acknowledgement exceeds 8 KiB",
        ));
    }
    let ack = String::from_utf8(ack)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "runtime sent non-UTF-8 JSON"))?;
    if validate(&ack) {
        Ok(())
    } else {
        let detail = json_string_field(&ack, "error")
            .or_else(|| json_string_field(&ack, "message"))
            .unwrap_or_else(|| ack.trim().to_owned());
        Err(io::Error::other(if detail.is_empty() {
            "runtime closed without accepting the request".to_owned()
        } else {
            format!("runtime rejected request: {detail}")
        }))
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "runtime acknowledgement deadline elapsed",
            )
        })
}

fn input_mode_acknowledgement_ok(
    json: &str,
    request_id: &str,
    token: &AppToken,
    mode: InputMode,
) -> bool {
    let Ok(ack) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    ack.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
        && ack.get("status").and_then(serde_json::Value::as_str) == Some("accepted")
        && ack.get("request_id").and_then(serde_json::Value::as_str) == Some(request_id)
        && ack.get("mode").and_then(serde_json::Value::as_str) == Some(mode.as_str())
        && acknowledgement_token_matches(&ack["token"], token)
        && ack.get("ink_enabled").and_then(serde_json::Value::as_bool) == Some(mode.ink_enabled())
}

fn acknowledgement_token_matches(value: &serde_json::Value, token: &AppToken) -> bool {
    value.get("app_id").and_then(serde_json::Value::as_str) == Some(token.app_id.as_str())
        && value.get("generation").and_then(serde_json::Value::as_u64) == Some(token.generation)
        && value
            .get("foreground_epoch")
            .and_then(serde_json::Value::as_u64)
            == Some(token.foreground_epoch)
        && value.get("lease_id").and_then(serde_json::Value::as_u64) == token.lease_id
}

fn json_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut rest = json.split_once(&needle)?.1.trim_start();
    rest = rest.strip_prefix(':')?.trim_start();
    rest = rest.strip_prefix('"')?;
    let mut output = String::new();
    let mut escaped = false;
    for character in rest.chars() {
        if escaped {
            output.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(output);
        } else {
            output.push(character);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::handoff::{
        external_reader_allowed_for_test, open_app_acknowledgement_ok, open_reader_request,
        start_open_reader_at, OpenReaderPoll, OPEN_APP_TIMEOUT,
    };
    use super::{
        input_mode_acknowledgement_ok, input_mode_request, json_string_field, validate_app_token,
    };
    use crate::platform::{AppToken, InputMode};
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn request_text(request: std::io::Result<Vec<u8>>) -> String {
        String::from_utf8(request.unwrap()).unwrap()
    }

    fn token() -> AppToken {
        AppToken {
            app_id: "magicpaper".into(),
            generation: 7,
            foreground_epoch: 11,
            lease_id: Some(13),
        }
    }

    fn socket_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("magicpaper-runtime-{name}-{nonce}.sock"))
    }

    #[test]
    fn deterministic_test_mode_blocks_reader_ipc() {
        assert!(!external_reader_allowed_for_test(true, false));
        assert!(external_reader_allowed_for_test(true, true));
        assert!(external_reader_allowed_for_test(false, false));
    }

    #[test]
    fn open_app_wait_is_bounded_to_a_short_queue_acknowledgement() {
        assert_eq!(OPEN_APP_TIMEOUT, std::time::Duration::from_secs(2));
    }

    #[test]
    fn open_app_ack_requires_a_correlated_v2_acceptance() {
        assert!(open_app_acknowledgement_ok(
            r#"{"ok":true,"status":"accepted","request_id":"mp-4"}"#,
            "mp-4"
        ));
        for rejected in [
            r#"{"ok":true,"status":"accepted","request_id":"old"}"#,
            r#"{"ok":true,"status":"starting","request_id":"mp-4"}"#,
            r#"{"ok":false,"status":"accepted","request_id":"mp-4"}"#,
            r#"{"status":"accepted","request_id":"mp-4"}"#,
        ] {
            assert!(!open_app_acknowledgement_ok(rejected, "mp-4"));
        }
        assert_eq!(
            json_string_field(r#"{"error":"reader is busy"}"#, "error").as_deref(),
            Some("reader is busy")
        );
    }

    #[test]
    fn control_request_is_one_json_line_with_explicit_fields() {
        let request = request_text(open_reader_request(
            "magicpaper-7-2",
            "/books/书\"名.epub",
            &token(),
        ));
        assert_eq!(request.lines().count(), 1);
        assert!(request.ends_with('\n'));
        assert!(request.contains(r#""version":2"#));
        assert!(request.contains(r#""request_id":"magicpaper-7-2""#));
        assert!(request.contains(r#""command":"open_app""#));
        assert!(request.contains(r#""app":"koreader""#));
        assert!(request.contains(r#""open_path":"/books/书\"名.epub""#));
        assert!(request.contains(r#""token":{"app_id":"magicpaper""#));
        assert!(request.contains(r#""generation":7"#));
        assert!(request.contains(r#""foreground_epoch":11"#));
        assert!(request.contains(r#""lease_id":13"#));
    }

    #[test]
    fn input_mode_request_has_a_correlatable_explicit_contract() {
        let request = request_text(input_mode_request(
            "magicpaper-7-3",
            &token(),
            InputMode::AnimationLocked,
        ));
        assert_eq!(request.lines().count(), 1);
        assert!(request.ends_with('\n'));
        assert!(request.contains(r#""version":2"#));
        assert!(request.contains(r#""request_id":"magicpaper-7-3""#));
        assert!(request.contains(r#""command":"set_input_mode""#));
        assert!(request.contains(r#""app_id":"magicpaper""#));
        assert!(request.contains(r#""generation":7"#));
        assert!(request.contains(r#""foreground_epoch":11"#));
        assert!(request.contains(r#""lease_id":13"#));
        assert!(request.contains(r#""mode":"animation_locked""#));
    }

    #[test]
    fn input_mode_ack_requires_matching_id_mode_and_ink_policy() {
        let token = token();
        let accepted = r#"{"ok":true,"status":"accepted","request_id":"mp-4","mode":"writing","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":true}"#;
        assert!(input_mode_acknowledgement_ok(
            accepted,
            "mp-4",
            &token,
            InputMode::Writing
        ));
        for rejected in [
            r#"{"ok":true,"status":"accepted","request_id":"old","mode":"writing","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":true}"#,
            r#"{"ok":true,"status":"accepted","request_id":"mp-4","mode":"modal","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":true}"#,
            r#"{"ok":true,"status":"accepted","request_id":"mp-4","mode":"writing","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":false}"#,
            r#"{"ok":true,"status":"accepted","request_id":"mp-4","mode":"writing","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":12,"lease_id":13},"ink_enabled":true}"#,
            r#"{"ok":true,"request_id":"mp-4","mode":"writing","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":true}"#,
            r#"{"status":"accepted","request_id":"mp-4","mode":"writing","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":true}"#,
        ] {
            assert!(!input_mode_acknowledgement_ok(
                rejected,
                "mp-4",
                &token,
                InputMode::Writing
            ));
        }
        let locked = r#"{"ok":true,"status":"accepted","request_id":"mp-5","mode":"animation_locked","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":false}"#;
        assert!(input_mode_acknowledgement_ok(
            locked,
            "mp-5",
            &token,
            InputMode::AnimationLocked
        ));
    }

    #[test]
    fn open_reader_worker_accepts_without_blocking_the_caller() {
        let socket = socket_path("accepted");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
                request.push(byte[0]);
            }
            let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
            let request_id = request["request_id"].as_str().unwrap();
            writeln!(
                stream,
                "{{\"ok\":true,\"status\":\"accepted\",\"request_id\":{}}}",
                serde_json::to_string(request_id).unwrap()
            )
            .unwrap();
        });
        let request = start_open_reader_at(
            std::path::Path::new("/books/道德经.epub"),
            &token(),
            socket.to_string_lossy().into_owned(),
            Duration::from_secs(1),
        )
        .unwrap();
        let started = Instant::now();
        loop {
            match request.poll() {
                OpenReaderPoll::Pending if started.elapsed() < Duration::from_secs(1) => {
                    std::thread::yield_now();
                }
                OpenReaderPoll::Accepted => break,
                OpenReaderPoll::Failed(error) => panic!("unexpected failure: {error}"),
                OpenReaderPoll::Pending => panic!("worker acknowledgement timed out"),
            }
        }
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn acknowledgement_deadline_is_total_even_for_slow_fragments() {
        let socket = socket_path("slow");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request);
            for byte in br#"{"ok":true,"status":"accepted","request_id":"slow"}\n"# {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let started = Instant::now();
        let error = super::request_acknowledged(
            &socket.to_string_lossy(),
            b"{}\n",
            Duration::from_millis(45),
            |_| true,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(200));
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn non_utf8_reader_paths_are_rejected_before_a_worker_starts() {
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
        let error = start_open_reader_at(
            &path,
            &token(),
            "unused.sock".into(),
            Duration::from_secs(1),
        )
        .err()
        .expect("non-UTF-8 path must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn incomplete_or_wrong_app_token_cannot_be_sent() {
        for invalid in [
            AppToken::default(),
            AppToken {
                app_id: "magicpaper".into(),
                generation: 7,
                foreground_epoch: 0,
                lease_id: Some(13),
            },
            AppToken {
                app_id: "koreader".into(),
                generation: 7,
                foreground_epoch: 11,
                lease_id: Some(13),
            },
        ] {
            assert!(validate_app_token(&invalid).is_err());
        }
        assert!(validate_app_token(&token()).is_ok());
    }
}
