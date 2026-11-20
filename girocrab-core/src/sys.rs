//! The crate's only `unsafe` code: safe wrappers around the few libc calls std
//! does not offer (#948). The crate denies `unsafe_code` (Cargo.toml `[lints]`) and
//! `scripts/lint-no-obfuscation.sh` lets only this file lift it, so a new
//! system call is a visible addition here, not a block inside a connector.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::io::{AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;

/// The real user ID of this process.
pub(crate) fn getuid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Sleeps in `poll(2)` until `a` or `b` has an event (readable, hung up) or
/// `timeout_ms` passed (-1 = no limit). Returns which of the two has one.
pub(crate) fn poll_readable(
    a: BorrowedFd<'_>,
    b: BorrowedFd<'_>,
    timeout_ms: i32,
) -> io::Result<(bool, bool)> {
    let mut fds = [a, b].map(|fd| libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    // SAFETY: `fds` is a valid array of two pollfd structs.
    if unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0].revents != 0, fds[1].revents != 0))
}

/// The user ID of the process at the other end of a connected Unix socket.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
        // SAFETY: fd is a connected Unix socket owned by `stream`; uid/gid are valid out pointers.
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: fd is a connected Unix socket; `cred`/`len` describe a valid buffer.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "linux"
    )))]
    {
        let _ = fd;
        Err(io::ErrorKind::Unsupported.into())
    }
}
