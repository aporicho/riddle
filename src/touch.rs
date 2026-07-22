//! Raw multitouch gestures for takeover mode.
//! One-finger swipe pages, two-finger drag scrolls, two-finger tap undoes,
//! three-finger tap redoes, and five fingers exits.

use std::io;
use std::os::fd::RawFd;

use crate::fb::screen_h;

const EV_SYN: u16 = 0;
const SYN_REPORT: u16 = 0;
const EV_ABS: u16 = 3;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;
const EVIOCGRAB: libc::c_ulong = 0x40044590;
const EVIOCGABS_MT_POSITION_Y: libc::c_ulong = 0x80184576;
const MAX_SLOTS: usize = 16;
const FALLBACK_TOUCH_MAX_Y: i32 = 2832;
const TAP_SLOP: i32 = 45;

#[repr(C)]
#[derive(Default)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

fn query_touch_max_y(fd: RawFd) -> i32 {
    let mut info = InputAbsInfo::default();
    let result =
        unsafe { libc::ioctl(fd, EVIOCGABS_MT_POSITION_Y, &mut info as *mut InputAbsInfo) };
    if result != 0 || info.maximum <= 0 {
        eprintln!(
            "magicpaper: warning: touch EVIOCGABS failed ({}), assuming {}",
            io::Error::last_os_error(),
            FALLBACK_TOUCH_MAX_Y
        );
        FALLBACK_TOUCH_MAX_Y
    } else {
        info.maximum
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Quit,
    Undo,
    Redo,
    /// Positive values move down through the document.
    Scroll(i32),
    /// Direction (+1 down, -1 up); caller chooses page size.
    Page(i32),
}

#[derive(Clone, Copy, Default)]
struct Slot {
    active: bool,
    start_y: i32,
    y: i32,
}

pub struct TouchDevice {
    fd: RawFd,
    touch_max_y: i32,
    slots: [Slot; MAX_SLOTS],
    cur: usize,
    max_fingers: usize,
    frame_y: Option<i32>,
    total_motion: i32,
    quit_sent: bool,
}

impl TouchDevice {
    pub fn open() -> io::Result<Self> {
        for i in 0..8 {
            let name_path = format!("/sys/class/input/event{i}/device/name");
            if let Ok(name) = std::fs::read_to_string(&name_path) {
                if name.to_lowercase().contains("touch") {
                    let path = std::ffi::CString::new(format!("/dev/input/event{i}")).unwrap();
                    let fd =
                        unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
                    if fd < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let touch_max_y = query_touch_max_y(fd);
                    eprintln!("magicpaper: touch digitizer Y range {touch_max_y}");
                    unsafe { libc::ioctl(fd, EVIOCGRAB, 1i32) };
                    return Ok(Self {
                        fd,
                        touch_max_y,
                        slots: [Slot::default(); MAX_SLOTS],
                        cur: 0,
                        max_fingers: 0,
                        frame_y: None,
                        total_motion: 0,
                        quit_sent: false,
                    });
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no touch device"))
    }

    /// Compatibility helper for takeover apps that only use five-finger exit.
    pub fn drain_check_quit(&mut self) -> bool {
        self.drain().contains(&Gesture::Quit)
    }

    pub fn drain(&mut self) -> Vec<Gesture> {
        let mut out = Vec::new();
        let mut buf = [0u8; 24 * 64];
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
                if etype == EV_ABS && code == ABS_MT_SLOT {
                    self.cur = (value.max(0) as usize).min(MAX_SLOTS - 1);
                } else if etype == EV_ABS && code == ABS_MT_POSITION_Y {
                    self.slots[self.cur].y = value;
                    if self.slots[self.cur].active && self.slots[self.cur].start_y == i32::MIN {
                        self.slots[self.cur].start_y = value;
                    }
                } else if etype == EV_ABS && code == ABS_MT_TRACKING_ID {
                    if value != -1 {
                        self.slots[self.cur] = Slot {
                            active: true,
                            start_y: i32::MIN,
                            y: self.slots[self.cur].y,
                        };
                    } else {
                        self.slots[self.cur].active = false;
                    }
                } else if etype == EV_SYN && code == SYN_REPORT {
                    self.finish_frame(&mut out);
                }
            }
        }
        out
    }

    fn finish_frame(&mut self, out: &mut Vec<Gesture>) {
        let active: Vec<Slot> = self.slots.iter().copied().filter(|s| s.active).collect();
        let count = active.len();
        self.max_fingers = self.max_fingers.max(count);
        if count >= 5 && !self.quit_sent {
            self.quit_sent = true;
            out.push(Gesture::Quit);
        }

        let average_y = (count > 0).then(|| active.iter().map(|s| s.y).sum::<i32>() / count as i32);
        if let (Some(previous), Some(current)) = (self.frame_y, average_y) {
            let raw_delta = previous - current;
            self.total_motion += raw_delta.abs();
            if count == 2 {
                let pixels = raw_delta * screen_h() as i32 / self.touch_max_y;
                if pixels != 0 {
                    out.push(Gesture::Scroll(pixels));
                }
            }
        }
        self.frame_y = average_y;

        if count == 0 && self.max_fingers > 0 {
            if self.total_motion < TAP_SLOP {
                match self.max_fingers {
                    2 => out.push(Gesture::Undo),
                    3 => out.push(Gesture::Redo),
                    _ => {}
                }
            } else if self.max_fingers == 1 {
                // Released slots retain their coordinates.
                if let Some(slot) = self
                    .slots
                    .iter()
                    .max_by_key(|slot| (slot.start_y - slot.y).abs())
                {
                    let delta = slot.start_y - slot.y;
                    if delta.abs() >= TAP_SLOP {
                        out.push(Gesture::Page(delta.signum()));
                    }
                }
            }
            self.max_fingers = 0;
            self.frame_y = None;
            self.total_motion = 0;
            self.quit_sent = false;
        }
    }
}

impl Drop for TouchDevice {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, EVIOCGRAB, 0i32);
            libc::close(self.fd);
        }
    }
}
