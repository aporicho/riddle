//! Exclusive process ownership for the recurring-task scheduler.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Process-lifetime lease held by the screenless task agent. Its advisory lock
/// lets every UI mode (managed QTFB or legacy takeover) prove that it must not
/// run a second scheduler.
pub struct SchedulerLease(File);

impl Drop for SchedulerLease {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn scheduler_lock_file(dir: &Path) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("scheduler.lock"))
}

pub(super) fn acquire_scheduler_lease_in(dir: &Path) -> io::Result<SchedulerLease> {
    let file = scheduler_lock_file(dir)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            if error.kind() == io::ErrorKind::WouldBlock {
                io::ErrorKind::AlreadyExists
            } else {
                error.kind()
            },
            format!("task scheduler lease unavailable: {error}"),
        ));
    }
    Ok(SchedulerLease(file))
}

pub fn acquire_scheduler_lease() -> io::Result<SchedulerLease> {
    acquire_scheduler_lease_in(&super::task_dir())
}

/// Fail closed: if ownership cannot be checked, the interactive UI must not
/// risk issuing duplicate scheduled API requests.
pub(super) fn external_scheduler_active_in(dir: &Path) -> bool {
    let Ok(file) = scheduler_lock_file(dir) else {
        return true;
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return true;
    }
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
    false
}

pub fn external_scheduler_active() -> bool {
    external_scheduler_active_in(&super::task_dir())
}
