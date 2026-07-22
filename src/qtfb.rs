//! Native qtfb client: SOCK_SEQPACKET protocol + shared-memory framebuffer.
//!
//! Wire format (verified against rm-appload src/qtfb/common.h):
//!   ClientMessage  = 24 bytes, type:u8 @0, payload @4
//!   ServerMessage  = 32 bytes, type:u8 @0, payload @8

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::time::Duration;

use crate::platform::{self, DamageRect, PenFrame, PenPhase, PenTool};

pub const MESSAGE_INITIALIZE: u8 = 0;
pub const MESSAGE_UPDATE: u8 = 1;
#[allow(dead_code)]
pub const MESSAGE_CUSTOM_INITIALIZE: u8 = 2;
pub const MESSAGE_TERMINATE: u8 = 3;
pub const MESSAGE_USERINPUT: u8 = 4;
pub const MESSAGE_SET_REFRESH_MODE: u8 = 5;
pub const MESSAGE_REQUEST_FULL_REFRESH: u8 = 6;

pub const UPDATE_ALL: i32 = 0;
pub const UPDATE_PARTIAL: i32 = 1;

/// FBFMT_RMPPM_RGB565: Paper Pro Move native 954x1696 RGB565.
pub const FBFMT_RMPPM_RGB565: u8 = 6;

#[allow(dead_code)]
pub const REFRESH_MODE_UFAST: i32 = 0;
pub const REFRESH_MODE_FAST: i32 = 1;
pub const REFRESH_MODE_CONTENT: i32 = 3;
const REFRESH_MODE_UI: i32 = 4;

// Input event types (server -> client).
pub const INPUT_TOUCH_PRESS: i32 = 0x10;
pub const INPUT_TOUCH_RELEASE: i32 = 0x11;
pub const INPUT_TOUCH_UPDATE: i32 = 0x12;
pub const INPUT_PEN_PRESS: i32 = 0x20;
pub const INPUT_PEN_RELEASE: i32 = 0x21;
#[allow(dead_code)]
pub const INPUT_PEN_UPDATE: i32 = 0x22;
/// Runtime convention: dev_id 0 is the marker tip, 1 is the eraser tip.
/// Upstream AppLoad currently sends 0 for every pen event, so this remains
/// backward compatible while allowing the ReMagic fork to preserve erasers.
pub const PEN_DEVICE_ERASER: i32 = 1;
#[allow(dead_code)]
pub const INPUT_VKB_RELEASE: i32 = 0x41;

const SOCKET_PATH: &str = "/tmp/qtfb.sock";
const MAX_EVENTS_PER_PUMP: usize = 512;

#[derive(Debug, Clone, Copy)]
pub struct InputEvent {
    pub input_type: i32,
    pub dev_id: i32,
    pub x: i32,
    pub y: i32,
    #[allow(dead_code)]
    pub d: i32,
}

impl InputEvent {
    pub fn pen_tool(self) -> crate::pen::Tool {
        if self.dev_id == PEN_DEVICE_ERASER || self.d < 0 {
            crate::pen::Tool::Eraser
        } else {
            crate::pen::Tool::Pen
        }
    }

    pub fn pressure_percent(self) -> i32 {
        self.d.saturating_abs().clamp(0, 100)
    }

    /// Normalize the legacy qtfb v1 payload into the platform pen contract.
    /// qtfb does not carry a kernel timestamp, so receive-time monotonic time
    /// is used and documented by `PenFrame`.
    pub fn to_pen_frame(self, sequence: u64, phase: PenPhase) -> PenFrame {
        PenFrame {
            sequence,
            kernel_time_ns: platform::monotonic_now_ns(),
            phase,
            tool: match self.pen_tool() {
                crate::pen::Tool::Pen => PenTool::Pen,
                crate::pen::Tool::Eraser => PenTool::Eraser,
            },
            x: self.x,
            y: self.y,
            pressure: (self.pressure_percent() * crate::pen::MAX_PRESSURE / 100) as u16,
        }
    }
}

pub struct QtfbClient {
    fd: RawFd,
    shm: *mut u8,
    shm_len: usize,
    pub width: usize,
    pub height: usize,
    applied_refresh_mode: Cell<i32>,
    pending_commit: RefCell<Option<PendingCommit>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingCommit {
    update: PendingUpdate,
    refresh_mode: i32,
}

impl PendingCommit {
    fn merge(self, newer: Self) -> Self {
        Self {
            update: self.update.merge(newer.update),
            // Stable content must dominate transient ink when backpressure
            // coalesces commits that requested different waveforms.
            refresh_mode: self.refresh_mode.max(newer.refresh_mode),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingUpdate {
    All,
    Partial(DamageRect),
}

impl PendingUpdate {
    fn merge(self, newer: Self) -> Self {
        match (self, newer) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::Partial(a), Self::Partial(b)) => {
                let x0 = a.x.min(b.x);
                let y0 = a.y.min(b.y);
                let x1 = a.x.saturating_add(a.width).max(b.x.saturating_add(b.width));
                let y1 =
                    a.y.saturating_add(a.height)
                        .max(b.y.saturating_add(b.height));
                Self::Partial(DamageRect {
                    x: x0,
                    y: y0,
                    width: x1.saturating_sub(x0),
                    height: y1.saturating_sub(y0),
                })
            }
        }
    }
}

// The raw pointer is to a MAP_SHARED region; we are the only writer thread.
unsafe impl Send for QtfbClient {}

impl QtfbClient {
    /// Connect and initialize with the default resolution of `format`.
    pub fn connect(
        key: i32,
        format: u8,
        width: usize,
        height: usize,
        bpp: usize,
    ) -> io::Result<Self> {
        let socket = connect_socket()?;
        let (shm_key, shm_size) = initialize(socket.as_raw_fd(), key, format)?;
        let ptr = map_framebuffer(shm_key, shm_size, width * height * bpp)?;
        set_nonblocking(socket.as_raw_fd())?;

        Ok(Self {
            fd: socket.into_raw_fd(),
            shm: ptr,
            shm_len: shm_size,
            width,
            height,
            applied_refresh_mode: Cell::new(REFRESH_MODE_UI),
            pending_commit: RefCell::new(None),
        })
    }

    /// Block efficiently until the runtime sends input/window state, or the
    /// caller's next animation/stream deadline. This replaces the idle 500 Hz
    /// recv loop without adding latency to pen or touch events.
    pub fn wait_io(&self, timeout: Duration, want_write: bool) -> io::Result<()> {
        let timeout_ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut pollfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN
                | libc::POLLHUP
                | libc::POLLERR
                | if want_write { libc::POLLOUT } else { 0 },
            revents: 0,
        };
        loop {
            let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if rc >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn framebuffer(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.shm, self.shm_len) }
    }

    fn send_msg(&self, msg: &[u8; 24]) -> io::Result<()> {
        send_all(self.fd, msg)
    }

    pub fn update_all(&self, refresh_mode: i32) -> io::Result<()> {
        self.queue_update(PendingUpdate::All, refresh_mode);
        self.flush_pending_update()
    }

    pub fn update_partial(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        refresh_mode: i32,
    ) -> io::Result<()> {
        self.queue_update(
            PendingUpdate::Partial(DamageRect {
                x,
                y,
                width: w,
                height: h,
            }),
            refresh_mode,
        );
        self.flush_pending_update()
    }

    fn queue_update(&self, update: PendingUpdate, refresh_mode: i32) {
        let newer = PendingCommit {
            update,
            refresh_mode,
        };
        let mut pending = self.pending_commit.borrow_mut();
        *pending = Some(match pending.take() {
            Some(existing) => existing.merge(newer),
            None => newer,
        });
    }

    pub fn has_pending_update(&self) -> bool {
        self.pending_commit.borrow().is_some()
    }

    /// Try the current merged update exactly once. WouldBlock leaves it queued
    /// for POLLOUT/the next reactor tick; all other errors remain fatal.
    pub fn flush_pending_update(&self) -> io::Result<()> {
        let Some(commit) = self.pending_commit.borrow_mut().take() else {
            return Ok(());
        };
        if self.applied_refresh_mode.get() != commit.refresh_mode {
            if let Err(error) = self.send_refresh_mode(commit.refresh_mode) {
                self.restore_pending(commit);
                return Err(error);
            }
            self.applied_refresh_mode.set(commit.refresh_mode);
        }
        let mut msg = [0u8; 24];
        msg[0] = MESSAGE_UPDATE;
        match commit.update {
            PendingUpdate::All => msg[4..8].copy_from_slice(&UPDATE_ALL.to_le_bytes()),
            PendingUpdate::Partial(damage) => {
                msg[4..8].copy_from_slice(&UPDATE_PARTIAL.to_le_bytes());
                msg[8..12].copy_from_slice(&damage.x.to_le_bytes());
                msg[12..16].copy_from_slice(&damage.y.to_le_bytes());
                msg[16..20].copy_from_slice(&damage.width.to_le_bytes());
                msg[20..24].copy_from_slice(&damage.height.to_le_bytes());
            }
        }
        if let Err(error) = self.send_msg(&msg) {
            self.restore_pending(commit);
            return Err(error);
        }
        Ok(())
    }

    fn restore_pending(&self, commit: PendingCommit) {
        let mut pending = self.pending_commit.borrow_mut();
        *pending = Some(match pending.take() {
            Some(newer) => commit.merge(newer),
            None => commit,
        });
    }

    /// Cache mode changes because some legacy AppLoad hosts handled this
    /// packet expensively. The managed ReMagic display host applies it without
    /// sleeping, so a real transition does not add a fixed one-second stall.
    fn send_refresh_mode(&self, mode: i32) -> io::Result<()> {
        let mut msg = [0u8; 24];
        msg[0] = MESSAGE_SET_REFRESH_MODE;
        msg[4..8].copy_from_slice(&mode.to_le_bytes());
        self.send_msg(&msg)
    }

    /// NOTE: 1s server-side stall, use only on explicit user request.
    pub fn request_full_refresh(&self) -> io::Result<()> {
        let mut msg = [0u8; 24];
        msg[0] = MESSAGE_REQUEST_FULL_REFRESH;
        self.send_msg(&msg)
    }

    pub fn terminate(&self) {
        let mut msg = [0u8; 24];
        msg[0] = MESSAGE_TERMINATE;
        let _ = self.send_msg(&msg);
    }

    /// Drain pending server messages. Returns input events, or Err on
    /// disconnect (window closed -> we must exit).
    pub fn drain_events(&self) -> io::Result<Vec<InputEvent>> {
        let mut out = Vec::new();
        loop {
            let mut buf = [0u8; 32];
            let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr() as *mut libc::c_void, 32, 0) };
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "qtfb socket closed",
                ));
            }
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    return Ok(out);
                }
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if buf[0] == MESSAGE_USERINPUT && n >= 28 {
                out.push(InputEvent {
                    input_type: i32::from_le_bytes(buf[8..12].try_into().unwrap()),
                    dev_id: i32::from_le_bytes(buf[12..16].try_into().unwrap()),
                    x: i32::from_le_bytes(buf[16..20].try_into().unwrap()),
                    y: i32::from_le_bytes(buf[20..24].try_into().unwrap()),
                    d: i32::from_le_bytes(buf[24..28].try_into().unwrap()),
                });
                if out.len() >= MAX_EVENTS_PER_PUMP {
                    return Ok(out);
                }
            }
        }
    }
}

impl Drop for QtfbClient {
    fn drop(&mut self) {
        self.terminate();
        unsafe {
            libc::munmap(self.shm as *mut libc::c_void, self.shm_len);
            libc::close(self.fd);
        }
    }
}

fn connect_socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (index, byte) in SOCKET_PATH.bytes().enumerate() {
        address.sun_path[index] = byte as libc::c_char;
    }
    let result = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

fn initialize(fd: RawFd, key: i32, format: u8) -> io::Result<(i32, usize)> {
    let mut message = [0_u8; 24];
    message[0] = MESSAGE_INITIALIZE;
    message[4..8].copy_from_slice(&key.to_le_bytes());
    message[8] = format;
    send_all(fd, &message)?;

    let mut reply = [0_u8; 32];
    let received = unsafe { libc::recv(fd, reply.as_mut_ptr().cast(), reply.len(), 0) };
    if received <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "qtfb server rejected init (no reply)",
        ));
    }
    Ok((
        i32::from_le_bytes(reply[8..12].try_into().unwrap()),
        u64::from_le_bytes(reply[16..24].try_into().unwrap()) as usize,
    ))
}

fn map_framebuffer(shm_key: i32, shm_size: usize, required: usize) -> io::Result<*mut u8> {
    let path = format!("/dev/shm/qtfb_{shm_key}\0");
    let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDWR) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let shm = unsafe { OwnedFd::from_raw_fd(fd) };
    let pointer = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            shm_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            shm.as_raw_fd(),
            0,
        )
    };
    if pointer == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    if shm_size < required {
        unsafe { libc::munmap(pointer, shm_size) };
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("shm too small: {shm_size} < {required}"),
        ));
    }
    Ok(pointer.cast())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn send_all(fd: RawFd, buf: &[u8]) -> io::Result<()> {
    send_packet_with(buf, |packet| {
        let written =
            unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), libc::MSG_NOSIGNAL) };
        if written < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(written as usize)
        }
    })
}

/// SOCK_SEQPACKET preserves one update as one packet. Once the fd is
/// nonblocking, backpressure is returned immediately to the runtime's merged
/// damage queue; the UI thread must never sleep while holding the event loop.
fn send_packet_with<F>(buf: &[u8], mut send: F) -> io::Result<()>
where
    F: FnMut(&[u8]) -> io::Result<usize>,
{
    loop {
        match send(buf) {
            Ok(written) if written == buf.len() => return Ok(()),
            Ok(_) => return Err(io::Error::new(io::ErrorKind::WriteZero, "short send")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
