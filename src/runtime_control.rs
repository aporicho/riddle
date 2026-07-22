//! Small client for application-to-runtime lifecycle requests.
//!
//! The protocol is deliberately one request and one acknowledgement per Unix
//! stream connection.  A newline terminates each compact JSON object, which
//! makes the boundary explicit without adding a JSON dependency to the device
//! binary.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::platform::{AppToken, InputMode};

const DEFAULT_SOCKET: &str = "/run/remagic/runtime-app.sock";
const MAX_ACK_BYTES: usize = 8 * 1024;
const OPEN_APP_TIMEOUT: Duration = Duration::from_secs(40);
const INPUT_MODE_TIMEOUT: Duration = Duration::from_secs(8);
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Ask the Remagic runtime to foreground KOReader at `path`.
pub fn open_reader(path: &Path) -> io::Result<()> {
    if !external_reader_allowed(crate::runtime_env::test_mode()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "external reader requests are disabled in RIDDLE_TEST_MODE",
        ));
    }
    let socket = runtime_socket();
    let request_id = next_request_id();
    let path = path.to_string_lossy();
    let request = open_reader_request(&request_id, &path);
    request_acknowledged(
        &socket,
        request.as_bytes(),
        OPEN_APP_TIMEOUT,
        legacy_acknowledgement_ok,
    )
}

/// Switch the manager-owned input overlay and require a correlated ACK.
///
/// This is intentionally stricter than the legacy `open_app` acknowledgement:
/// accepting an uncorrelated or mismatched mode could leave host live ink on
/// while MagicPaper believes an animation is protected.
pub fn set_input_mode(token: &AppToken, mode: InputMode) -> io::Result<()> {
    validate_input_token(token)?;
    let socket = runtime_socket();
    let request_id = next_request_id();
    let request = input_mode_request(&request_id, token, mode);
    request_acknowledged(&socket, request.as_bytes(), INPUT_MODE_TIMEOUT, |ack| {
        input_mode_acknowledgement_ok(ack, &request_id, token, mode)
    })
}

fn external_reader_allowed(test_mode: bool) -> bool {
    !test_mode
}

fn open_reader_request(request_id: &str, path: &str) -> String {
    format!(
        "{{\"version\":1,\"request_id\":\"{}\",\"command\":\"open_app\",\"app\":\"koreader\",\"open_path\":\"{}\"}}\n",
        json_escape(request_id),
        json_escape(path),
    )
}

fn input_mode_request(request_id: &str, token: &AppToken, mode: InputMode) -> String {
    let lease_id = token.lease_id.unwrap_or_default();
    format!(
        "{{\"version\":2,\"request_id\":\"{}\",\"command\":\"set_input_mode\",\"token\":{{\"app_id\":\"{}\",\"generation\":{},\"foreground_epoch\":{},\"lease_id\":{}}},\"mode\":\"{}\"}}\n",
        json_escape(request_id),
        json_escape(&token.app_id),
        token.generation,
        token.foreground_epoch,
        lease_id,
        mode.as_str(),
    )
}

fn validate_input_token(token: &AppToken) -> io::Result<()> {
    if token.app_id != "magicpaper"
        || token.generation == 0
        || token.foreground_epoch == 0
        || token.lease_id.is_none_or(|lease_id| lease_id == 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input-mode request requires the exact MagicPaper foreground token",
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
    let mut stream = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("connect to runtime control socket {socket}: {error}"),
        )
    })?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(request)?;
    stream.flush()?;

    let mut ack = Vec::new();
    let mut byte = [0u8; 1];
    while ack.len() < MAX_ACK_BYTES {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => ack.push(byte[0]),
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

fn legacy_acknowledgement_ok(json: &str) -> bool {
    let compact: String = json
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    compact.contains("\"ok\":true")
        || compact.contains("\"accepted\":true")
        || matches!(
            json_string_field(json, "status").as_deref(),
            Some("ok" | "accepted" | "starting" | "foreground")
        )
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

fn json_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character < ' ' => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output
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
    use super::{
        external_reader_allowed, input_mode_acknowledgement_ok, input_mode_request, json_escape,
        json_string_field, legacy_acknowledgement_ok, open_reader_request, validate_input_token,
    };
    use crate::platform::{AppToken, InputMode};

    fn token() -> AppToken {
        AppToken {
            app_id: "magicpaper".into(),
            generation: 7,
            foreground_epoch: 11,
            lease_id: Some(13),
        }
    }

    #[test]
    fn deterministic_test_mode_blocks_reader_ipc() {
        assert!(!external_reader_allowed(true));
        assert!(external_reader_allowed(false));
    }

    #[test]
    fn request_values_are_json_escaped() {
        assert_eq!(json_escape("书\n\\\"名"), "书\\n\\\\\\\"名");
    }

    #[test]
    fn runtime_acknowledgements_accept_supported_shapes() {
        assert!(legacy_acknowledgement_ok(r#"{"ok": true}"#));
        assert!(legacy_acknowledgement_ok(r#"{"status":"accepted"}"#));
        assert!(legacy_acknowledgement_ok(r#"{"accepted":true}"#));
        assert!(!legacy_acknowledgement_ok(r#"{"ok":false,"error":"busy"}"#));
        assert_eq!(
            json_string_field(r#"{"error":"reader is busy"}"#, "error").as_deref(),
            Some("reader is busy")
        );
    }

    #[test]
    fn control_request_is_one_json_line_with_explicit_fields() {
        let request = open_reader_request("magicpaper-7-2", "/books/书\"名.epub");
        assert_eq!(request.lines().count(), 1);
        assert!(request.ends_with('\n'));
        assert!(request.contains(r#""version":1"#));
        assert!(request.contains(r#""request_id":"magicpaper-7-2""#));
        assert!(request.contains(r#""command":"open_app""#));
        assert!(request.contains(r#""app":"koreader""#));
        assert!(request.contains(r#""open_path":"/books/书\"名.epub""#));
    }

    #[test]
    fn input_mode_request_has_a_correlatable_explicit_contract() {
        let request = input_mode_request("magicpaper-7-3", &token(), InputMode::AnimationLocked);
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
            assert!(validate_input_token(&invalid).is_err());
        }
        assert!(validate_input_token(&token()).is_ok());
    }
}
