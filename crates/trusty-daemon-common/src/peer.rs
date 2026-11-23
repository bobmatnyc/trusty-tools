//! The kernel's record of which uid connected to a Unix socket.
//!
//! Why: broker sockets are 0600 in a 0700 directory, and each accepted
//! connection is still checked against the kernel's credential, so a
//! file-mode slip cannot open a broker to another local user.
//! What: Darwin reads `LOCAL_PEERCRED` (`struct xucred`; a non-zero
//! `cr_version` is refused), Linux reads `SO_PEERCRED`; any other platform
//! answers `Unsupported`. Each consumer maps the uid to its own refusal type.
//! Test: `same_user_socketpair_reports_our_uid`.

use std::io;
use std::os::fd::AsRawFd;

/// The connecting process's uid as the kernel recorded it.
pub fn peer_uid(sock: &impl AsRawFd) -> io::Result<u32> {
    peer_uid_fd(sock.as_raw_fd())
}

#[cfg(target_os = "macos")]
fn peer_uid_fd(fd: i32) -> io::Result<u32> {
    // SAFETY: xucred is plain old data; zeroed is a valid value.
    let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
    // SAFETY: the buffer and length describe `cred`, which outlives the call.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // Python unpacks `=II` (version, uid) and refuses any version but 0.
    if (len as usize) < 8 || cred.cr_version != 0 {
        return Err(io::Error::other("unexpected peer credential version"));
    }
    Ok(cred.cr_uid)
}

#[cfg(target_os = "linux")]
fn peer_uid_fd(fd: i32) -> io::Result<u32> {
    // SAFETY: ucred is plain old data; zeroed is a valid value.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer and length describe `cred`, which outlives the call.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_uid_fd(_fd: i32) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer credentials",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn same_user_socketpair_reports_our_uid() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        assert_eq!(peer_uid(&a).ok(), Some(crate::private::uid()));
        assert_eq!(peer_uid(&b).ok(), Some(crate::private::uid()));
    }
}
