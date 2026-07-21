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

const DEFAULT_SOCKET: &str = "/run/remagic/runtime-app.sock";
const MAX_ACK_BYTES: usize = 8 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Ask the Remagic runtime to foreground KOReader at `path`.
pub fn open_reader(path: &Path) -> io::Result<()> {
    if !external_reader_allowed(crate::runtime_env::test_mode()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "external reader requests are disabled in RIDDLE_TEST_MODE",
        ));
    }
    let socket = std::env::var("REMAGIC_RUNTIME_SOCKET")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SOCKET.to_owned());
    let request_id = format!(
        "magicpaper-{}-{}",
        std::process::id(),
        REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let path = path.to_string_lossy();
    let request = open_reader_request(&request_id, &path);
    request_acknowledged(&socket, request.as_bytes())
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

fn request_acknowledged(socket: &str, request: &[u8]) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("connect to runtime control socket {socket}: {error}"),
        )
    })?;
    stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
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
    if acknowledgement_ok(&ack) {
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

fn acknowledgement_ok(json: &str) -> bool {
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
        acknowledgement_ok, external_reader_allowed, json_escape, json_string_field,
        open_reader_request,
    };

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
        assert!(acknowledgement_ok(r#"{"ok": true}"#));
        assert!(acknowledgement_ok(r#"{"status":"accepted"}"#));
        assert!(acknowledgement_ok(r#"{"accepted":true}"#));
        assert!(!acknowledgement_ok(r#"{"ok":false,"error":"busy"}"#));
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
}
