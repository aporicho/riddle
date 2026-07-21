use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;

use super::{FD_ENV, SOCKET_ENV};

pub(super) trait LifecycleTransport: Send {
    fn read_nonblocking(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
    fn write_nonblocking(&mut self, buffer: &[u8]) -> io::Result<usize>;
    fn message_oriented(&self) -> bool;
}

impl LifecycleTransport for UnixStream {
    fn read_nonblocking(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.read(buffer)
    }

    fn write_nonblocking(&mut self, buffer: &[u8]) -> io::Result<usize> {
        socket_send(self.as_raw_fd(), buffer)
    }

    fn message_oriented(&self) -> bool {
        false
    }
}

pub(super) struct FdTransport {
    pub(super) file: std::fs::File,
    pub(super) message_oriented: bool,
}

impl LifecycleTransport for FdTransport {
    fn read_nonblocking(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }

    fn write_nonblocking(&mut self, buffer: &[u8]) -> io::Result<usize> {
        socket_send(self.file.as_raw_fd(), buffer)
    }

    fn message_oriented(&self) -> bool {
        self.message_oriented
    }
}

pub(super) fn discover() -> io::Result<Option<Box<dyn LifecycleTransport>>> {
    if let Some(value) = std::env::var_os(FD_ENV) {
        let value = value.to_string_lossy();
        let fd: RawFd = value
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid lifecycle fd"))?;
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative lifecycle fd",
            ));
        }
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { std::fs::File::from_raw_fd(duplicate) };
        set_nonblocking(file.as_raw_fd())?;
        let message_oriented = socket_type(file.as_raw_fd())? == libc::SOCK_SEQPACKET;
        eprintln!("magic-paper: lifecycle commands use inherited fd {fd}");
        return Ok(Some(Box::new(FdTransport {
            file,
            message_oriented,
        })));
    }

    if let Some(path) = std::env::var_os(SOCKET_ENV) {
        let stream = UnixStream::connect(path)?;
        stream.set_nonblocking(true)?;
        eprintln!("magic-paper: lifecycle commands use local UNIX socket");
        return Ok(Some(Box::new(stream)));
    }
    Ok(None)
}

pub(super) fn socket_send(fd: RawFd, buffer: &[u8]) -> io::Result<usize> {
    let written =
        unsafe { libc::send(fd, buffer.as_ptr().cast(), buffer.len(), libc::MSG_NOSIGNAL) };
    if written < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(written as usize)
    }
}

fn socket_type(fd: RawFd) -> io::Result<i32> {
    let mut kind: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(kind)
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
pub(super) struct FakeTransport {
    pub(super) chunks: std::collections::VecDeque<Vec<u8>>,
    pub(super) writes: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    pub(super) message_oriented: bool,
    pub(super) max_write: Option<usize>,
}

#[cfg(test)]
impl LifecycleTransport for FakeTransport {
    fn read_nonblocking(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(mut chunk) = self.chunks.pop_front() else {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        };
        let size = chunk.len().min(buffer.len());
        buffer[..size].copy_from_slice(&chunk[..size]);
        if size < chunk.len() {
            chunk.drain(..size);
            self.chunks.push_front(chunk);
        }
        Ok(size)
    }

    fn write_nonblocking(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let size = self.max_write.unwrap_or(buffer.len()).min(buffer.len());
        self.writes.lock().unwrap().push(buffer[..size].to_vec());
        Ok(size)
    }

    fn message_oriented(&self) -> bool {
        self.message_oriented
    }
}
