//! Bounded, cancellable framing for the private Agent Unix stream.

use serde_json::Value;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};

// Must match ReMagic agent protocol's AGENT_MAX_FRAME.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub(super) fn valid_event(value: &Value, request_id: &str, app_id: &str) -> bool {
    value.get("protocol").and_then(Value::as_u64) == Some(1)
        && value.get("request_id").and_then(Value::as_str) == Some(request_id)
        && value.get("app_id").and_then(Value::as_str) == Some(app_id)
}

pub(super) fn write_frame(stream: &mut UnixStream, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Agent frame is too large",
        ));
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

pub(super) fn read_frame(
    stream: &mut UnixStream,
    cancelled: &AtomicBool,
) -> io::Result<Option<Value>> {
    let mut header = [0_u8; 4];
    if !read_exact_cancellable(stream, &mut header, cancelled)? {
        return Ok(None);
    }
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Agent frame size",
        ));
    }
    let mut body = vec![0_u8; size];
    if !read_exact_cancellable(stream, &mut body, cancelled)? {
        return Ok(None);
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_exact_cancellable(
    stream: &mut UnixStream,
    bytes: &mut [u8],
    cancelled: &AtomicBool,
) -> io::Result<bool> {
    let mut offset = 0;
    while offset < bytes.len() {
        if cancelled.load(Ordering::Acquire) {
            return Ok(false);
        }
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Agent closed stream",
                ))
            }
            Ok(read) => offset += read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}
