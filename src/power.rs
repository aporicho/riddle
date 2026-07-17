//! Power button, for takeover mode. The device is GRABBED so logind doesn't
//! also act on the press: the diary draws its sleep page first, then triggers
//! the suspend itself. If the grab fails we still see the press and draw, and
//! leave the actual suspend to logind.

use std::io;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

const EV_KEY: u16 = 1;
const KEY_POWER: u16 = 116;
const EVIOCGRAB: libc::c_ulong = 0x40044590;
/// Maximum pause between clicks. A lone/double click resolves to one normal
/// power action after this delay; the third click resolves immediately.
const MULTI_CLICK_GAP: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    None,
    Single,
    Triple,
}

/// Turns raw power-key presses into either one normal click or a triple-click.
/// A double click intentionally falls back to one normal click.
pub struct ClickTracker {
    clicks: usize,
    deadline: Option<Instant>,
}

impl ClickTracker {
    pub fn new() -> Self {
        Self {
            clicks: 0,
            deadline: None,
        }
    }

    pub fn push(&mut self, presses: usize, now: Instant) -> ClickAction {
        if presses == 0 {
            return self.poll(now);
        }
        if self.deadline.is_some_and(|deadline| now > deadline) {
            self.clear();
        }
        self.clicks += presses;
        if self.clicks >= 3 {
            self.clear();
            ClickAction::Triple
        } else {
            self.deadline = Some(now + MULTI_CLICK_GAP);
            ClickAction::None
        }
    }

    pub fn poll(&mut self, now: Instant) -> ClickAction {
        if self.clicks > 0 && self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            ClickAction::Single
        } else {
            ClickAction::None
        }
    }

    pub fn clear(&mut self) {
        self.clicks = 0;
        self.deadline = None;
    }
}

pub struct PowerButton {
    fd: RawFd,
    pub grabbed: bool,
    down: bool,
}

impl PowerButton {
    pub fn open() -> io::Result<Self> {
        Self::open_with_grab(true)
    }

    /// Open without EVIOCGRAB so xochitl keeps its normal single-click power
    /// behavior while the launcher quietly watches for a triple-click.
    pub fn open_listener() -> io::Result<Self> {
        Self::open_with_grab(false)
    }

    fn open_with_grab(should_grab: bool) -> io::Result<Self> {
        for i in 0..8 {
            let name = std::fs::read_to_string(format!("/sys/class/input/event{i}/device/name"))
                .unwrap_or_default()
                .to_lowercase();
            if !name.contains("powerkey")
                && !name.contains("pwrkey")
                && !name.contains("power button")
            {
                continue;
            }
            let cpath = std::ffi::CString::new(format!("/dev/input/event{i}")).unwrap();
            let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let grabbed = should_grab && unsafe { libc::ioctl(fd, EVIOCGRAB, 1i32) } == 0;
            eprintln!(
                "riddle: power button /dev/input/event{i} ({})",
                if grabbed { "grabbed" } else { "shared" }
            );
            return Ok(Self {
                fd,
                grabbed,
                down: false,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no power button device",
        ))
    }

    /// Number of power-key presses (value 1; auto-repeat ignored) since the
    /// last drain.
    pub fn drain_press_count(&mut self) -> usize {
        let mut presses = 0;
        let mut buf = [0u8; 24 * 16];
        loop {
            let n =
                unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            for chunk in buf[..n as usize].chunks_exact(24) {
                let etype = u16::from_le_bytes(chunk[16..18].try_into().unwrap());
                let code = u16::from_le_bytes(chunk[18..20].try_into().unwrap());
                let value = i32::from_le_bytes(chunk[20..24].try_into().unwrap());
                if etype == EV_KEY && code == KEY_POWER {
                    if value == 1 {
                        self.down = true;
                        presses += 1;
                    } else if value == 0 {
                        self.down = false;
                    }
                }
            }
        }
        presses
    }

    pub fn drain_pressed(&mut self) -> bool {
        self.drain_press_count() > 0
    }

    /// Drain through the release belonging to the triggering press. Without
    /// this, xochitl can inherit the final key-up after it is restarted and
    /// immediately put the restored UI back to sleep.
    pub fn wait_for_release(&mut self, timeout: Duration) {
        let start = Instant::now();
        while self.down && start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(10));
            let _ = self.drain_press_count();
        }
        // Also discard any SYN/key bounce already queued behind the release.
        std::thread::sleep(Duration::from_millis(40));
        let _ = self.drain_press_count();
    }
}

impl Drop for PowerButton {
    fn drop(&mut self) {
        unsafe {
            if self.grabbed {
                libc::ioctl(self.fd, EVIOCGRAB, 0i32);
            }
            libc::close(self.fd);
        }
    }
}

/// Background mode used beside xochitl. It never grabs the button, so normal
/// power handling remains untouched; three quick presses launch the diary.
pub fn launcher_loop() -> io::Result<()> {
    let mut button = PowerButton::open_listener()?;
    let mut clicks = ClickTracker::new();
    eprintln!("riddle: power launcher ready (triple-click to open)");
    loop {
        let presses = button.drain_press_count();
        if clicks.push(presses, Instant::now()) == ClickAction::Triple {
            eprintln!("riddle: power launcher triple-click");
            // Keep the system awake while xochitl is stopped and Quill takes
            // ownership of the panel. The takeover restore hook releases it.
            let _ = std::fs::write("/sys/power/wake_lock", b"riddle-takeover\n");
            let status =
                std::process::Command::new("/home/root/apps/riddle/riddle-launch.sh").status();
            if !status.as_ref().is_ok_and(|s| s.success()) {
                eprintln!("riddle: power launcher failed: {status:?}");
                let _ = std::fs::write("/sys/power/wake_unlock", b"riddle-takeover\n");
            }
            // Ignore bounce and any trailing event from the triggering click.
            std::thread::sleep(Duration::from_millis(500));
            let _ = button.drain_press_count();
            clicks.clear();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The kernel's successful-suspend counter — the authoritative "we slept"
/// signal. (Clock heuristics fail here: on this kernel CLOCK_MONOTONIC keeps
/// advancing across deep sleep, verified on-device.)
pub fn suspend_count() -> u64 {
    std::fs::read_to_string("/sys/power/suspend_stats/success")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// After resume, Wi-Fi is often stranded: wpa_supplicant fails a few attempts
/// while the radio settles and marks the network TEMP-DISABLED, and with
/// xochitl stopped nobody clears it. Nudge it back, detached, best-effort.
pub fn wifi_heal() {
    let script = "for i in 1 2 3 4 5 6 7 8 9 10; do \
        state=$(wpa_cli -i wlan0 status 2>/dev/null | grep ^wpa_state | cut -d= -f2); \
        [ \"$state\" = COMPLETED ] && exit 0; \
        wpa_cli -i wlan0 enable_network all >/dev/null 2>&1; \
        wpa_cli -i wlan0 reassociate >/dev/null 2>&1; \
        sleep 3; \
        done";
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_click_fires_immediately() {
        let t = Instant::now();
        let mut clicks = ClickTracker::new();
        assert_eq!(clicks.push(1, t), ClickAction::None);
        assert_eq!(
            clicks.push(1, t + Duration::from_millis(200)),
            ClickAction::None
        );
        assert_eq!(
            clicks.push(1, t + Duration::from_millis(400)),
            ClickAction::Triple
        );
    }

    #[test]
    fn lone_click_becomes_single_after_gap() {
        let t = Instant::now();
        let mut clicks = ClickTracker::new();
        assert_eq!(clicks.push(1, t), ClickAction::None);
        assert_eq!(
            clicks.poll(t + Duration::from_millis(799)),
            ClickAction::None
        );
        assert_eq!(
            clicks.poll(t + Duration::from_millis(800)),
            ClickAction::Single
        );
    }

    #[test]
    fn slow_click_starts_a_new_sequence() {
        let t = Instant::now();
        let mut clicks = ClickTracker::new();
        assert_eq!(clicks.push(1, t), ClickAction::None);
        assert_eq!(
            clicks.push(1, t + Duration::from_secs(1)),
            ClickAction::None
        );
        assert_eq!(
            clicks.poll(t + Duration::from_millis(1800)),
            ClickAction::Single
        );
    }
}
