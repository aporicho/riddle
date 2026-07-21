//! Lifecycle v2/legacy wire envelopes and incremental frame decoding.

use std::io;

use serde::{Deserialize, Serialize};

use crate::platform::AppToken;

use super::{LifecycleCommand, MAX_FRAME};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum InboundFraming {
    #[default]
    Unknown,
    JsonLines,
    LengthPrefixed,
}

pub(super) fn encode_event(
    token: &AppToken,
    request_id: String,
    event: OutboundEvent<'_>,
    framing: InboundFraming,
) -> io::Result<Vec<u8>> {
    let envelope = OutboundEnvelope {
        protocol: 2,
        request_id,
        body: OutboundBody {
            token: WireTokenRef::from(token),
            event,
        },
    };
    let payload = serde_json::to_vec(&envelope).map_err(io::Error::other)?;
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lifecycle event exceeds protocol limit",
        ));
    }
    let mut bytes = Vec::with_capacity(payload.len() + 4);
    match framing {
        InboundFraming::JsonLines => {
            bytes.extend_from_slice(&payload);
            bytes.push(b'\n');
        }
        InboundFraming::Unknown | InboundFraming::LengthPrefixed => {
            bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&payload);
        }
    }
    Ok(bytes)
}

#[allow(dead_code)] // The complete formal stage vocabulary is serialized here.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleStage {
    Start,
    Foreground,
    Background,
    Save,
    Shutdown,
    Runtime,
}

#[derive(Serialize)]
struct OutboundEnvelope<'a> {
    protocol: u16,
    request_id: String,
    body: OutboundBody<'a>,
}

#[derive(Serialize)]
struct OutboundBody<'a> {
    token: WireTokenRef<'a>,
    #[serde(flatten)]
    event: OutboundEvent<'a>,
}

#[derive(Serialize)]
struct WireTokenRef<'a> {
    app_id: &'a str,
    generation: u64,
    foreground_epoch: u64,
    lease_id: Option<u64>,
}

impl<'a> From<&'a AppToken> for WireTokenRef<'a> {
    fn from(token: &'a AppToken) -> Self {
        Self {
            app_id: &token.app_id,
            generation: token.generation,
            foreground_epoch: token.foreground_epoch,
            lease_id: token.lease_id,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum OutboundEvent<'a> {
    Ready {
        #[serde(skip_serializing_if = "Option::is_none")]
        first_frame_sequence: Option<u64>,
    },
    BackgroundReady {
        title: &'a str,
        subtitle: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        resume_payload: Option<&'a serde_json::Value>,
    },
    StateSaved {
        #[serde(skip_serializing_if = "Option::is_none")]
        resume_payload: Option<&'a serde_json::Value>,
    },
    ShutdownComplete {
        exit_code: i32,
    },
    Failed {
        stage: LifecycleStage,
        message: &'a str,
        retryable: bool,
    },
}

#[derive(Debug)]
pub(super) struct DecodedCommand {
    pub(super) command: LifecycleCommand,
    pub(super) token: Option<AppToken>,
}

#[derive(Deserialize)]
struct V2Envelope {
    protocol: u16,
    request_id: String,
    body: V2Body,
}

#[derive(Deserialize)]
struct V2Body {
    token: WireToken,
    #[serde(flatten)]
    command: V2Command,
}

#[derive(Deserialize)]
struct WireToken {
    app_id: String,
    generation: u64,
    foreground_epoch: u64,
    #[serde(default)]
    lease_id: Option<u64>,
}

impl From<WireToken> for AppToken {
    fn from(token: WireToken) -> Self {
        Self {
            app_id: token.app_id,
            generation: token.generation,
            foreground_epoch: token.foreground_epoch,
            lease_id: token.lease_id,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum V2Command {
    Start,
    EnterForeground,
    EnterBackground,
    OpenPath,
    Shutdown { deadline_ms: u64 },
}

impl V2Command {
    fn app_command(self) -> io::Result<LifecycleCommand> {
        Ok(match self {
            Self::Start => LifecycleCommand::Start,
            Self::EnterForeground | Self::OpenPath => LifecycleCommand::EnterForeground,
            Self::EnterBackground => LifecycleCommand::EnterBackground,
            Self::Shutdown { deadline_ms: 0 } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "shutdown deadline must be non-zero",
                ))
            }
            Self::Shutdown { deadline_ms } => LifecycleCommand::Shutdown { deadline_ms },
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LegacyCommand {
    EnterForeground,
    Resume,
    PreparePark,
    EnterBackground,
    Shutdown,
}

impl LegacyCommand {
    fn app_command(self) -> LifecycleCommand {
        match self {
            Self::EnterForeground | Self::Resume => LifecycleCommand::EnterForeground,
            Self::PreparePark | Self::EnterBackground => LifecycleCommand::EnterBackground,
            Self::Shutdown => LifecycleCommand::Shutdown { deadline_ms: 100 },
        }
    }
}

pub(super) fn decode_frames(
    buffer: &mut Vec<u8>,
    framing: &mut InboundFraming,
    end_of_stream: bool,
) -> io::Result<Vec<DecodedCommand>> {
    while *framing == InboundFraming::Unknown && buffer.first().is_some_and(u8::is_ascii_whitespace)
    {
        buffer.remove(0);
    }
    if *framing == InboundFraming::Unknown {
        match buffer.first() {
            Some(b'{') => *framing = InboundFraming::JsonLines,
            Some(_) if buffer.len() >= 4 => *framing = InboundFraming::LengthPrefixed,
            _ => return Ok(Vec::new()),
        }
    }

    match framing {
        InboundFraming::JsonLines => decode_json_lines(buffer, end_of_stream),
        InboundFraming::LengthPrefixed => decode_length_prefixed(buffer, end_of_stream),
        InboundFraming::Unknown => Ok(Vec::new()),
    }
}

fn decode_json_lines(buffer: &mut Vec<u8>, end_of_stream: bool) -> io::Result<Vec<DecodedCommand>> {
    let mut commands = Vec::new();
    while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut payload = buffer.drain(..=newline).collect::<Vec<_>>();
        payload.pop();
        let payload = trim_ascii(&payload);
        if payload.is_empty() {
            continue;
        }
        if payload.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "lifecycle JSON packet exceeds protocol limit",
            ));
        }
        commands.push(decode_command(payload)?);
    }
    if end_of_stream && !trim_ascii(buffer).is_empty() {
        let payload = std::mem::take(buffer);
        commands.push(decode_command(trim_ascii(&payload))?);
    } else if buffer.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unterminated lifecycle JSON packet exceeds protocol limit",
        ));
    }
    Ok(commands)
}

fn decode_length_prefixed(
    buffer: &mut Vec<u8>,
    end_of_stream: bool,
) -> io::Result<Vec<DecodedCommand>> {
    let mut commands = Vec::new();
    loop {
        if buffer.len() < 4 {
            break;
        }
        let length = u32::from_be_bytes(buffer[..4].try_into().unwrap()) as usize;
        if length == 0 || length > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid lifecycle frame length {length}"),
            ));
        }
        if buffer.len() < 4 + length {
            break;
        }
        let payload = buffer[4..4 + length].to_vec();
        buffer.drain(..4 + length);
        commands.push(decode_command(&payload)?);
    }
    if end_of_stream && !buffer.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated lifecycle length-prefixed frame",
        ));
    }
    Ok(commands)
}

pub(super) fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub(super) fn decode_command(payload: &[u8]) -> io::Result<DecodedCommand> {
    if let Ok(envelope) = serde_json::from_slice::<V2Envelope>(payload) {
        if envelope.protocol != 2 || !valid_request_id(&envelope.request_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid lifecycle v2 envelope",
            ));
        }
        return Ok(DecodedCommand {
            command: envelope.body.command.app_command()?,
            token: Some(envelope.body.token.into()),
        });
    }
    let legacy: LegacyCommand = serde_json::from_slice(payload).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid lifecycle command: {error}"),
        )
    })?;
    Ok(DecodedCommand {
        command: legacy.app_command(),
        token: None,
    })
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}
