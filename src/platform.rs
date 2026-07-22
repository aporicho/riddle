//! Platform-facing contracts shared by the current device adapters and the
//! future Remagic host protocol.
//!
//! These types intentionally do not mention qtfb, Quill, Qt, or framebuffer
//! memory.  Backends translate them at the platform boundary.

#![allow(dead_code)] // Contract surface is introduced ahead of the v2 host.

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AppToken {
    /// Stable manifest identifier, for example `magicpaper`.
    pub(crate) app_id: String,
    pub(crate) generation: u64,
    pub(crate) foreground_epoch: u64,
    pub(crate) lease_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PenPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PenTool {
    Pen,
    Eraser,
}

/// Application-level input ownership negotiated with the Remagic runtime.
///
/// `Writing` is the only mode in which the host may draw its low-latency ink
/// overlay. The two locked modes still forward normalized input events so the
/// application can dismiss an answer or interact with a modal paper page, but
/// the host must never render those contacts as page ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputMode {
    Writing,
    AnimationLocked,
    Modal,
}

impl InputMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Writing => "writing",
            Self::AnimationLocked => "animation_locked",
            Self::Modal => "modal",
        }
    }

    pub(crate) const fn ink_enabled(self) -> bool {
        matches!(self, Self::Writing)
    }
}

/// One normalized pen frame. Pressure is always in the inclusive 0..=4096
/// range. `kernel_time_ns` is the kernel's CLOCK_MONOTONIC event timestamp
/// when available, or CLOCK_MONOTONIC receive time when a transport/driver
/// cannot provide one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PenFrame {
    pub(crate) sequence: u64,
    pub(crate) kernel_time_ns: u64,
    pub(crate) phase: PenPhase,
    pub(crate) tool: PenTool,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) pressure: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DamageRect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshIntent {
    Ink,
    /// High-quality monochrome partial update for settling erased ink without
    /// invoking a color or full-panel waveform.
    MonoQuality,
    Ui,
    Content,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FrameCommit {
    pub(crate) token: AppToken,
    pub(crate) frame_sequence: u64,
    pub(crate) damage: Vec<DamageRect>,
    pub(crate) intent: RefreshIntent,
}

/// Stable display contract. Implementations may be in-process today or an IPC
/// client once the Remagic display host owns the panel.
pub(crate) trait DisplayBackend {
    type Error;

    fn commit(&self, frame: &FrameCommit) -> Result<(), Self::Error>;
    fn request_full_refresh(&self, token: &AppToken, bounds: DamageRect)
        -> Result<(), Self::Error>;
}

/// Optional low-latency overlay. The formal page remains application-owned;
/// this backend owns only transient ink between begin/end and settle.
pub(crate) trait LiveInkBackend {
    type Error;

    fn begin(&mut self, token: &AppToken, frame: PenFrame) -> Result<(), Self::Error>;
    fn update(&mut self, token: &AppToken, frame: PenFrame) -> Result<(), Self::Error>;
    fn end(&mut self, token: &AppToken, frame: PenFrame) -> Result<(), Self::Error>;
    fn cancel(&mut self, token: &AppToken, sequence: u64) -> Result<(), Self::Error>;
}

pub(crate) fn monotonic_now_ns() -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) };
    if result != 0 {
        return 0;
    }
    (value.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(value.tv_nsec.max(0) as u64)
}
