//! Display backends: qtfb (windowed, inside xochitl) and quill (takeover,
//! vendor engine, xochitl stopped). The hosted entry is fail-closed: it never
//! falls through to takeover merely because QTFB_KEY is missing.

use crate::platform::{
    AppToken, DamageRect, DisplayBackend, FrameCommit, LiveInkBackend, PenFrame, RefreshIntent,
};
use crate::surface::{PixFmt, Surface};
use std::io;
use std::time::Duration;

pub enum Display {
    Qtfb(crate::qtfb::QtfbClient),
    #[allow(dead_code)]
    Quill,
}

// C ABI from libquill.so (linked when built with --features takeover).
#[cfg(feature = "takeover")]
mod quill_ffi {
    extern "C" {
        pub fn quill_init() -> i32;
        pub fn quill_width() -> i32;
        pub fn quill_height() -> i32;
        pub fn quill_stride() -> i32;
        pub fn quill_buffer() -> *mut u8;
        pub fn quill_swap(x: i32, y: i32, w: i32, h: i32, mode: i32, full: i32) -> u64;
        pub fn quill_process_events();
    }
}

impl Display {
    pub fn open(allow_legacy_takeover: bool) -> io::Result<(Self, Surface)> {
        if !allow_legacy_takeover {
            let key = std::env::var("QTFB_KEY").map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "QTFB_KEY is required (use --legacy-takeover for the explicit raw-device path)",
                )
            })?;
            let key: i32 = key.parse().map_err(io::Error::other)?;
            let mut client = crate::qtfb::QtfbClient::connect(
                key,
                crate::qtfb::FBFMT_RMPPM_RGB565,
                954,
                1696,
                2,
            )?;
            let buf = client.framebuffer();
            let (ptr, len) = (buf.as_mut_ptr(), buf.len());
            let surface = Surface::new(ptr, len, 954, 1696, 954 * 2, PixFmt::Rgb565);
            return Ok((Display::Qtfb(client), surface));
        }

        #[cfg(feature = "takeover")]
        {
            unsafe {
                if quill_ffi::quill_init() != 0 {
                    return Err(io::Error::other("quill_init failed"));
                }
                let w = quill_ffi::quill_width() as usize;
                let h = quill_ffi::quill_height() as usize;
                let stride = quill_ffi::quill_stride() as usize;
                let ptr = quill_ffi::quill_buffer();
                if ptr.is_null() {
                    return Err(io::Error::other("quill buffer null"));
                }
                let surface = Surface::new(ptr, stride * h, w, h, stride, PixFmt::Rgb32);
                Ok((Display::Quill, surface))
            }
        }
        #[cfg(not(feature = "takeover"))]
        Err(io::Error::other(
            "legacy takeover requested but this build has no takeover backend",
        ))
    }

    /// Submit a semantic refresh intent. Only this backend maps the intent to
    /// qtfb or vendor waveform details; application code never chooses modes.
    pub fn present_region(&self, x: i32, y: i32, w: i32, h: i32, intent: RefreshIntent) {
        if w <= 0 || h <= 0 {
            return;
        }
        let _ = self.present_damage(
            DamageRect {
                x,
                y,
                width: w,
                height: h,
            },
            intent,
        );
    }

    pub fn present_all(&self, w: usize, h: usize, intent: RefreshIntent) {
        let _ = self.present_all_checked(w, h, intent);
    }

    /// Present a complete application surface and preserve the transport
    /// result for lifecycle readiness. Other best-effort UI updates keep the
    /// historical fire-and-forget API above.
    pub fn present_all_checked(&self, w: usize, h: usize, intent: RefreshIntent) -> io::Result<()> {
        if w == 0 || h == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot present an empty application surface",
            ));
        }
        self.present_damage(
            DamageRect {
                x: 0,
                y: 0,
                width: w as i32,
                height: h as i32,
            },
            intent,
        )
    }

    pub fn request_refresh(&self, w: usize, h: usize) {
        let _ = DisplayBackend::request_full_refresh(
            self,
            &legacy_token(),
            DamageRect {
                x: 0,
                y: 0,
                width: w as i32,
                height: h as i32,
            },
        );
    }

    fn present_damage(&self, damage: DamageRect, intent: RefreshIntent) -> io::Result<()> {
        match self {
            Display::Qtfb(c) => {
                let refresh_mode = qtfb_refresh_mode(intent, crate::runtime_env::is_managed());
                if damage.x == 0
                    && damage.y == 0
                    && damage.width as usize >= c.width
                    && damage.height as usize >= c.height
                {
                    c.update_all(refresh_mode)
                } else {
                    c.update_partial(
                        damage.x,
                        damage.y,
                        damage.width,
                        damage.height,
                        refresh_mode,
                    )
                }
            }
            #[allow(unused_variables, unreachable_code)]
            Display::Quill => {
                #[cfg(feature = "takeover")]
                unsafe {
                    let (mode, full) = vendor_refresh(intent);
                    quill_ffi::quill_swap(
                        damage.x,
                        damage.y,
                        damage.width,
                        damage.height,
                        mode,
                        full,
                    );
                    quill_ffi::quill_process_events();
                    return Ok(());
                }
                Ok(())
            }
        }
    }

    /// Drain window-system events. For qtfb this also detects window close
    /// (returns Err); the takeover backend has no window to lose.
    pub fn pump(&self) -> io::Result<Vec<crate::qtfb::InputEvent>> {
        match self {
            Display::Qtfb(c) => {
                match c.flush_pending_update() {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
                c.drain_events()
            }
            Display::Quill => {
                #[cfg(feature = "takeover")]
                unsafe {
                    quill_ffi::quill_process_events();
                }
                Ok(Vec::new())
            }
        }
    }

    /// Wait for hosted input without polling the QTFB socket in a hot loop.
    /// Takeover has additional raw fds that are not represented here, so keep
    /// its historical short sleep until it moves to a shared poll set.
    pub fn wait(&self, timeout: Duration, want_write: bool) {
        match self {
            Display::Qtfb(client) => {
                let _ = client.wait_io(timeout, want_write || client.has_pending_update());
            }
            Display::Quill => std::thread::sleep(timeout.min(Duration::from_millis(2))),
        }
    }

    pub fn terminate(&self) {
        if let Display::Qtfb(c) = self {
            c.terminate();
        }
    }
}

impl DisplayBackend for Display {
    type Error = io::Error;

    fn commit(&self, frame: &FrameCommit) -> Result<(), Self::Error> {
        if frame.intent == RefreshIntent::Full {
            let bounds = frame.damage.first().copied().unwrap_or_default();
            return self.request_full_refresh(&frame.token, bounds);
        }
        for damage in frame.damage.iter().copied() {
            if damage.width > 0 && damage.height > 0 {
                self.present_damage(damage, frame.intent)?;
            }
        }
        Ok(())
    }

    fn request_full_refresh(
        &self,
        _token: &AppToken,
        bounds: DamageRect,
    ) -> Result<(), Self::Error> {
        match self {
            Display::Qtfb(client) => client.request_full_refresh(),
            Display::Quill => self.present_damage(bounds, RefreshIntent::Full),
        }
    }
}

/// Compatibility bridge for today's shared-memory page. It gives the runtime
/// a real `LiveInkBackend` now while preserving the current qtfb/Quill surface
/// until Surface v2 supplies a transient host-owned overlay.
pub struct LegacyLiveInkAdapter<'a> {
    display: &'a Display,
    frame: FrameCommit,
}

impl<'a> LegacyLiveInkAdapter<'a> {
    pub fn new(display: &'a Display) -> Self {
        Self {
            display,
            frame: FrameCommit {
                token: legacy_token(),
                frame_sequence: 0,
                damage: Vec::with_capacity(1),
                intent: RefreshIntent::Ink,
            },
        }
    }

    pub fn present_damage(&mut self, damage: DamageRect) -> io::Result<()> {
        self.frame.frame_sequence = self.frame.frame_sequence.wrapping_add(1).max(1);
        self.frame.damage.clear();
        self.frame.damage.push(damage);
        self.display.commit(&self.frame)
    }

    fn point_damage(frame: PenFrame) -> DamageRect {
        const PAD: i32 = 8;
        DamageRect {
            x: frame.x - PAD,
            y: frame.y - PAD,
            width: PAD * 2 + 1,
            height: PAD * 2 + 1,
        }
    }
}

impl LiveInkBackend for LegacyLiveInkAdapter<'_> {
    type Error = io::Error;

    fn begin(&mut self, _token: &AppToken, frame: PenFrame) -> Result<(), Self::Error> {
        self.present_damage(Self::point_damage(frame))
    }

    fn update(&mut self, _token: &AppToken, frame: PenFrame) -> Result<(), Self::Error> {
        self.present_damage(Self::point_damage(frame))
    }

    fn end(&mut self, _token: &AppToken, _frame: PenFrame) -> Result<(), Self::Error> {
        Ok(())
    }

    fn cancel(&mut self, _token: &AppToken, _sequence: u64) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn legacy_token() -> AppToken {
    AppToken {
        app_id: "magicpaper".into(),
        generation: 0,
        foreground_epoch: 0,
        lease_id: None,
    }
}

fn qtfb_refresh_mode(intent: RefreshIntent, managed: bool) -> i32 {
    if !managed {
        return crate::qtfb::REFRESH_MODE_UFAST;
    }
    match intent {
        RefreshIntent::Ink => crate::qtfb::REFRESH_MODE_UFAST,
        RefreshIntent::CleanPartial => crate::qtfb::REFRESH_MODE_CONTENT,
        // MagicPaper is deliberately monochrome. Stable paper, menus and
        // erased regions use the quality mono waveform; the color/content
        // waveform adds latency and visible flashing without useful output.
        RefreshIntent::MonoQuality | RefreshIntent::Ui | RefreshIntent::Content => {
            crate::qtfb::REFRESH_MODE_FAST
        }
        RefreshIntent::Full => crate::qtfb::REFRESH_MODE_CONTENT,
    }
}

#[cfg(any(feature = "takeover", test))]
fn vendor_refresh(intent: RefreshIntent) -> (i32, i32) {
    match intent {
        RefreshIntent::Ink => (0, 0),
        RefreshIntent::CleanPartial => (4, 0),
        RefreshIntent::MonoQuality | RefreshIntent::Ui | RefreshIntent::Content => (3, 0),
        RefreshIntent::Full => (4, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_intents_are_mapped_only_at_the_vendor_boundary() {
        assert_eq!(vendor_refresh(RefreshIntent::Ink), (0, 0));
        assert_eq!(vendor_refresh(RefreshIntent::MonoQuality), (3, 0));
        assert_eq!(vendor_refresh(RefreshIntent::CleanPartial), (4, 0));
        assert_eq!(vendor_refresh(RefreshIntent::Ui), (3, 0));
        assert_eq!(vendor_refresh(RefreshIntent::Content), (3, 0));
        assert_eq!(vendor_refresh(RefreshIntent::Full), (4, 1));
    }

    #[test]
    fn managed_qtfb_preserves_semantic_refresh_intents() {
        assert_eq!(
            qtfb_refresh_mode(RefreshIntent::Ink, true),
            crate::qtfb::REFRESH_MODE_UFAST
        );
        assert_eq!(
            qtfb_refresh_mode(RefreshIntent::MonoQuality, true),
            crate::qtfb::REFRESH_MODE_FAST
        );
        assert_eq!(
            qtfb_refresh_mode(RefreshIntent::CleanPartial, true),
            crate::qtfb::REFRESH_MODE_CONTENT
        );
        assert_eq!(
            qtfb_refresh_mode(RefreshIntent::Ui, true),
            crate::qtfb::REFRESH_MODE_FAST
        );
        assert_eq!(
            qtfb_refresh_mode(RefreshIntent::Content, true),
            crate::qtfb::REFRESH_MODE_FAST
        );
        assert_eq!(
            qtfb_refresh_mode(RefreshIntent::Content, false),
            crate::qtfb::REFRESH_MODE_UFAST
        );
    }
}
