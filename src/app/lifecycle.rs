//! Optional ReMagic application-lifecycle command client.
//!
//! Managed launches pass a bidirectional inherited `SOCK_SEQPACKET`; a local
//! UNIX stream remains available for development. The wire codec and transport
//! ownership live in sibling modules so this file only owns lifecycle policy.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::platform::AppToken;

const FD_ENV: &str = "REMAGIC_LIFECYCLE_FD";
const SOCKET_ENV: &str = "REMAGIC_LIFECYCLE_SOCKET";
const MAX_FRAME: usize = 64 * 1024;

mod transport;
mod wire;

use transport::LifecycleTransport;
#[cfg(test)]
use transport::{socket_send, FakeTransport, FdTransport};
pub(super) use wire::LifecycleStage;
use wire::{decode_frames, encode_event, DecodedCommand, InboundFraming, OutboundEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LifecycleCommand {
    Start,
    EnterForeground,
    EnterBackground,
    Shutdown { deadline_ms: u64 },
}

struct OutboundFrame {
    bytes: Vec<u8>,
    written: usize,
}

pub(super) struct LifecycleClient {
    transport: Option<Box<dyn LifecycleTransport>>,
    received: Vec<u8>,
    read_buffer: Box<[u8]>,
    framing: InboundFraming,
    outbound: VecDeque<OutboundFrame>,
    disconnected: bool,
    active_token: Option<AppToken>,
    pending_ready_frame: Option<u64>,
    last_ready_token: Option<AppToken>,
    last_request_stamp: u64,
    require_v2: bool,
}

impl LifecycleClient {
    pub(super) fn discover(required: bool) -> io::Result<Self> {
        match Self::from_environment() {
            Ok(client) if required && !client.is_connected() => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "managed MagicPaper lifecycle channel is missing",
            )),
            Ok(mut client) => {
                client.require_v2 = required;
                Ok(client)
            }
            Err(error) if required => Err(error),
            Err(error) => {
                eprintln!(
                    "magic-paper: lifecycle channel unavailable ({error}); using legacy foreground mode"
                );
                Ok(Self::disabled())
            }
        }
    }

    fn from_environment() -> io::Result<Self> {
        Ok(match transport::discover()? {
            Some(transport) => Self::connected(transport),
            None => Self::disabled(),
        })
    }

    pub(super) fn poll(&mut self) -> io::Result<Vec<LifecycleCommand>> {
        self.flush_outbound()?;
        let Some(transport) = self.transport.as_mut() else {
            return Ok(Vec::new());
        };

        let mut newly_disconnected = false;
        loop {
            match transport.read_nonblocking(&mut self.read_buffer) {
                Ok(0) => {
                    if !self.disconnected {
                        self.disconnected = true;
                        newly_disconnected = true;
                    }
                    break;
                }
                Ok(size) => self.received.extend_from_slice(&self.read_buffer[..size]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }

        let decoded = decode_frames(&mut self.received, &mut self.framing, newly_disconnected)?;
        let mut commands = Vec::with_capacity(decoded.len());
        for frame in decoded {
            if self.accept_frame(&frame)? {
                commands.push(frame.command);
                self.queue_pending_ready()?;
            }
        }
        if !newly_disconnected {
            self.flush_outbound()?;
        }
        if newly_disconnected
            && !commands
                .iter()
                .any(|command| matches!(command, LifecycleCommand::Shutdown { .. }))
        {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "lifecycle channel disconnected without Shutdown",
            ));
        }
        Ok(commands)
    }

    pub(super) fn is_connected(&self) -> bool {
        self.transport.is_some() && !self.disconnected
    }

    pub(super) fn active_token(&self) -> Option<&AppToken> {
        self.active_token.as_ref()
    }

    pub(super) fn report_ready_after_frame(&mut self, frame_sequence: u64) -> io::Result<()> {
        if frame_sequence == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ready frame sequence must be non-zero",
            ));
        }
        if self.transport.is_none() {
            return Ok(());
        }
        self.pending_ready_frame = Some(frame_sequence);
        self.queue_pending_ready()?;
        self.flush_outbound()
    }

    pub(super) fn report_background_ready(&mut self) -> io::Result<()> {
        self.queue_event(OutboundEvent::StateSaved {
            resume_payload: None,
        })?;
        self.queue_event(OutboundEvent::BackgroundReady {
            title: "MagicPaper",
            subtitle: "已暂停，可继续",
            resume_payload: None,
        })?;
        self.flush_outbound()
    }

    pub(super) fn report_shutdown_complete(
        &mut self,
        exit_code: i32,
        deadline: Duration,
    ) -> io::Result<()> {
        self.queue_event(OutboundEvent::StateSaved {
            resume_payload: None,
        })?;
        self.queue_event(OutboundEvent::ShutdownComplete { exit_code })?;
        self.flush_for(deadline)
    }

    pub(super) fn report_failed(
        &mut self,
        stage: LifecycleStage,
        message: &str,
        retryable: bool,
        deadline: Duration,
    ) -> io::Result<()> {
        self.queue_event(OutboundEvent::Failed {
            stage,
            message,
            retryable,
        })?;
        self.flush_for(deadline)
    }

    fn queue_pending_ready(&mut self) -> io::Result<()> {
        let (Some(frame_sequence), Some(token)) =
            (self.pending_ready_frame, self.active_token.clone())
        else {
            return Ok(());
        };
        if self.last_ready_token.as_ref() == Some(&token) {
            self.pending_ready_frame = None;
            return Ok(());
        }
        if self.queue_event(OutboundEvent::Ready {
            first_frame_sequence: Some(frame_sequence),
        })? {
            self.pending_ready_frame = None;
            self.last_ready_token = Some(token);
        }
        Ok(())
    }

    fn queue_event(&mut self, event: OutboundEvent<'_>) -> io::Result<bool> {
        if self.transport.is_none() {
            return Ok(false);
        }
        let Some(token) = self.active_token.clone() else {
            return Ok(false);
        };
        let request_id = self.next_request_id(token.generation);
        let bytes = encode_event(&token, request_id, event, self.framing)?;
        self.outbound.push_back(OutboundFrame { bytes, written: 0 });
        Ok(true)
    }

    fn next_request_id(&mut self, generation: u64) -> String {
        let now = crate::platform::monotonic_now_ns();
        let stamp = now.max(self.last_request_stamp.saturating_add(1));
        self.last_request_stamp = stamp;
        format!("mp-{generation}-{stamp}")
    }

    fn flush_outbound(&mut self) -> io::Result<()> {
        let Some(transport) = self.transport.as_mut() else {
            self.outbound.clear();
            return Ok(());
        };
        while let Some(frame) = self.outbound.front_mut() {
            let remaining = &frame.bytes[frame.written..];
            match transport.write_nonblocking(remaining) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "lifecycle event write returned zero",
                    ));
                }
                Ok(written) => {
                    if transport.message_oriented() && written != remaining.len() {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "short lifecycle packet write",
                        ));
                    }
                    frame.written += written;
                    if frame.written == frame.bytes.len() {
                        self.outbound.pop_front();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn flush_for(&mut self, deadline: Duration) -> io::Result<()> {
        let until = Instant::now() + deadline.min(Duration::from_secs(30));
        loop {
            self.flush_outbound()?;
            if self.outbound.is_empty() {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "lifecycle event channel stayed backpressured",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn connected(transport: Box<dyn LifecycleTransport>) -> Self {
        Self {
            transport: Some(transport),
            received: Vec::new(),
            read_buffer: vec![0; MAX_FRAME + 4].into_boxed_slice(),
            framing: InboundFraming::Unknown,
            outbound: VecDeque::new(),
            disconnected: false,
            active_token: None,
            pending_ready_frame: None,
            last_ready_token: None,
            last_request_stamp: 0,
            require_v2: false,
        }
    }

    fn disabled() -> Self {
        Self {
            transport: None,
            received: Vec::new(),
            read_buffer: Vec::new().into_boxed_slice(),
            framing: InboundFraming::Unknown,
            outbound: VecDeque::new(),
            disconnected: false,
            active_token: None,
            pending_ready_frame: None,
            last_ready_token: None,
            last_request_stamp: 0,
            require_v2: false,
        }
    }

    #[cfg(test)]
    fn fake(chunks: Vec<Vec<u8>>) -> Self {
        Self::fake_with_output(chunks, false, None).0
    }

    #[cfg(test)]
    fn fake_with_output(
        chunks: Vec<Vec<u8>>,
        message_oriented: bool,
        max_write: Option<usize>,
    ) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self::connected(Box::new(FakeTransport {
                chunks: chunks.into(),
                writes: std::sync::Arc::clone(&writes),
                message_oriented,
                max_write,
            })),
            writes,
        )
    }

    fn accept_frame(&mut self, frame: &DecodedCommand) -> io::Result<bool> {
        let Some(token) = frame.token.as_ref() else {
            if self.require_v2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "managed MagicPaper requires lifecycle v2 tokens",
                ));
            }
            return Ok(true);
        };
        if token.app_id != "magicpaper" || token.generation == 0 || token.lease_id == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid MagicPaper lifecycle token",
            ));
        }
        let Some(active) = self.active_token.as_ref() else {
            if self.require_v2
                && (!matches!(frame.command, LifecycleCommand::Start)
                    || token.foreground_epoch == 0
                    || token.lease_id.is_none())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "managed MagicPaper must start with a foreground lifecycle token",
                ));
            }
            self.active_token = Some(token.clone());
            return Ok(true);
        };
        if token.generation != active.generation || token.foreground_epoch < active.foreground_epoch
        {
            eprintln!("magic-paper: stale lifecycle generation/epoch ignored");
            return Ok(false);
        }
        let accepted = match frame.command {
            LifecycleCommand::Start => token == active,
            LifecycleCommand::EnterForeground => {
                (token.foreground_epoch > active.foreground_epoch && token.lease_id.is_some())
                    || (token.foreground_epoch == active.foreground_epoch
                        && token.lease_id == active.lease_id)
            }
            LifecycleCommand::EnterBackground | LifecycleCommand::Shutdown { .. } => {
                token.foreground_epoch == active.foreground_epoch
                    && (token.lease_id == active.lease_id || token.lease_id.is_none())
            }
        };
        if !accepted {
            eprintln!("magic-paper: stale/mismatched lifecycle token ignored");
            return Ok(false);
        }
        self.active_token = Some(token.clone());
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
