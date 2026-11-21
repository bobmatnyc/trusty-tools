//! Same-user peer authentication for every broker socket.
//!
//! Why: the sockets are 0600 in a 0700 directory, and each accepted
//! connection is still checked against the kernel's record of who connected
//! (spike `broker.py:46-52`, `194-198`). A file-mode slip must not open the
//! broker to another local user.
//! What: Darwin reads `LOCAL_PEERCRED` (`struct xucred`; a non-zero
//! `cr_version` is refused), Linux reads `SO_PEERCRED`; other platforms fail.
//! [`admit`] refuses an unreadable credential and any uid but ours.
//! Test: `same_user_socketpair_passes_peer_auth`, `foreign_or_unreadable_peer_is_refused`
//! in `tests/broker_server.rs`.

// copied from trusty-browser 275d713590 (src/peer.rs), trimmed: `Refusal` -> `PeerRefused`.

use std::io;
use std::os::fd::AsRawFd;

use crate::private;

/// Why a peer was refused; `code` is the reply and audit code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct PeerRefused {
    /// `foreign_uid` or `peer_unverified`.
    pub code: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

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

/// Admit a peer whose credential read gave `peer` when this process is `me`.
///
/// Why: split from the socket so a foreign uid is testable without root.
/// What: an unreadable credential is `peer_unverified`, another uid is
/// `foreign_uid`; only `Ok(me)` passes. Both refusals fail closed.
/// Test: `foreign_or_unreadable_peer_is_refused`.
pub fn admit(peer: io::Result<u32>, me: u32) -> Result<u32, PeerRefused> {
    match peer {
        Ok(uid) if uid == me => Ok(uid),
        Ok(uid) => Err(PeerRefused {
            code: "foreign_uid",
            detail: format!("peer uid {uid} is not {me}"),
        }),
        Err(e) => Err(PeerRefused {
            code: "peer_unverified",
            detail: e.to_string(),
        }),
    }
}

/// [`admit`] for a connected socket against this process's uid.
pub fn verify_peer(sock: &impl AsRawFd) -> Result<u32, PeerRefused> {
    admit(peer_uid(sock), private::uid())
}
