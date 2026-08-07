//! Peer-credential checks on an accepted Unix-domain connection.
//!
//! Why: filesystem permissions alone are a documented intention. A `0600`
//! socket in a `0700` directory is unreachable to another uid *while those
//! bits hold*, but nothing in the process re-checks them — an operator who
//! loosens the directory, a backup tool that restores it wide, or a socket
//! reached before this crate's hardening ran would all pass unnoticed. Asking
//! the kernel who is on the other end turns the permission bits into an
//! enforced boundary. ADR-0034 requires it for exactly this reason.
//!
//! What: [`peer_uid`] wraps the platform syscall — `SO_PEERCRED` on Linux,
//! `getpeereid` on macOS and the BSDs — and [`ensure_peer_is_self`] refuses any
//! peer whose uid differs from this process's own. Targets with neither
//! syscall fail closed via [`UdsSecurityError::UnsupportedPlatform`] rather
//! than compiling to an unchecked accept.
//!
//! Test: `peer_uid_of_self_connection_is_self` and
//! `ensure_peer_is_self_accepts_same_uid` in `tests.rs`; the cross-uid refusal
//! is `#[ignore]`d because it needs a second uid.

use std::io;
use std::os::fd::AsRawFd;

use tokio::net::UnixStream;

use super::UdsSecurityError;

/// The uid this process runs as.
///
/// Why: the comparison target for every peer check, and the key
/// [`super::scratch_socket_dir`] uses to give each user its own directory.
/// What: `getuid(2)`. Cannot fail per POSIX.
/// Test: exercised by every peer test; equality with `id -u` is not asserted
/// because the test process has no independent source of truth for it.
pub fn self_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, touches no caller memory, and is
    // documented as always succeeding.
    unsafe { libc::getuid() }
}

/// Read the uid of the process on the other end of `stream`.
///
/// Why: split from [`ensure_peer_is_self`] so a caller that wants to log or
/// meter the peer identity does not have to duplicate the platform `cfg`s.
/// What: `getsockopt(SO_PEERCRED)` on Linux, `getpeereid(3)` elsewhere on
/// unix. The credentials the kernel reports are those captured at `connect`
/// time, so a peer cannot change uid after connecting to defeat the check.
/// Test: `peer_uid_of_self_connection_is_self`.
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &UnixStream) -> Result<u32, UdsSecurityError> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    // SAFETY: `stream` owns a live connected socket fd for the duration of the
    // call; `cred` and `len` are a correctly-sized, correctly-typed
    // out-parameter pair for SO_PEERCRED on SOL_SOCKET.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(UdsSecurityError::PeerCred {
            source: io::Error::last_os_error(),
        });
    }
    Ok(cred.uid)
}

/// See the Linux variant for the contract; this is the BSD/macOS syscall.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub fn peer_uid(stream: &UnixStream) -> Result<u32, UdsSecurityError> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;

    // SAFETY: `stream` owns a live connected socket fd for the duration of the
    // call; both out-parameters are valid, correctly-typed stack slots.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if rc != 0 {
        return Err(UdsSecurityError::PeerCred {
            source: io::Error::last_os_error(),
        });
    }
    Ok(uid)
}

/// Fail closed on a unix target with neither `SO_PEERCRED` nor `getpeereid`.
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
pub fn peer_uid(_stream: &UnixStream) -> Result<u32, UdsSecurityError> {
    Err(UdsSecurityError::UnsupportedPlatform)
}

/// Refuse a connection whose peer is not this same uid.
///
/// Why: the accept-side half of the `0600` contract. ADR-0034: "the target
/// verifies peer credentials on accept and refuses any connection whose uid is
/// not its own. This is what makes the permission bits an enforced boundary
/// rather than a documented intention."
///
/// What: compares [`peer_uid`] against [`self_uid`] and returns
/// [`UdsSecurityError::ForeignPeer`] on mismatch. Root is refused like any
/// other foreign uid — root can bypass the filesystem check anyway, so
/// admitting it would buy nothing and widen the accepted set.
///
/// Test: `ensure_peer_is_self_accepts_same_uid`; the refusal path is
/// `ensure_peer_is_self_refuses_foreign_uid`, `#[ignore]`d because an
/// unprivileged CI runner cannot connect as a second uid.
pub fn ensure_peer_is_self(stream: &UnixStream) -> Result<(), UdsSecurityError> {
    let peer = peer_uid(stream)?;
    let expected = self_uid();
    if peer != expected {
        return Err(UdsSecurityError::ForeignPeer { peer, expected });
    }
    Ok(())
}
