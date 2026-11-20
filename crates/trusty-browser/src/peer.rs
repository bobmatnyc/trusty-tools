//! Same-user peer authentication for every broker socket.
//!
//! Why: the sockets are mode 0600 in a 0700 directory, and each accepted
//! connection is still checked against the kernel's record of who connected
//! (`protocol.py` `verify_peer`). A file-mode slip must not open the broker
//! to another local user.
//! What: Darwin reads `LOCAL_PEERCRED` (`struct xucred`; a non-zero
//! `cr_version` is refused, as Python does), Linux reads `SO_PEERCRED`; any
//! other platform answers `UnsupportedPeerCredentials`. The peer uid must equal
//! this process's uid.
//! Test: `same_user_socketpair_passes_peer_auth` (below), and every replay in
//! `tests/parity_broker.rs` runs through it.

use std::io;
use std::os::fd::AsRawFd;

use crate::private;
use crate::refusal::Refusal;

/// The connecting process's uid as the kernel recorded it.
///
/// Why: the one primitive the uid check and its tests share.
/// What: an error when the option cannot be read or has an unexpected shape.
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

/// `verify_peer`: refuse unless the peer is this user.
///
/// Why: the broker's first check on every connection, before any byte is read.
/// What: `UnsupportedPeerCredentials` off Darwin/Linux; `Unauthorized` when the
/// credential cannot be read or names another uid. The caller adds
/// `reason: peer_uid` and the audit line.
/// Test: `same_user_socketpair_passes_peer_auth`, `foreign_uid_is_refused`.
pub fn verify_peer(sock: &impl AsRawFd) -> Result<(), Refusal> {
    if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
        return Err(Refusal::new("UnsupportedPeerCredentials"));
    }
    match peer_uid(sock) {
        Ok(uid) => uid_matches(uid, private::uid()),
        Err(_) => Err(Refusal::new("Unauthorized")),
    }
}

/// The comparison itself, split out so a foreign uid can be tested without root.
pub fn uid_matches(peer: u32, me: u32) -> Result<(), Refusal> {
    if peer == me {
        Ok(())
    } else {
        Err(Refusal::new("Unauthorized"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn same_user_socketpair_passes_peer_auth() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        assert_eq!(peer_uid(&a).ok(), Some(private::uid()));
        assert!(verify_peer(&b).is_ok());
    }

    #[test]
    fn foreign_uid_is_refused() {
        let me = private::uid();
        let err = uid_matches(me.wrapping_add(1), me).err();
        assert_eq!(err.map(|r| r.code).as_deref(), Some("Unauthorized"));
        assert!(uid_matches(me, me).is_ok());
    }
}
