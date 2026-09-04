//! Peer-credential checks on an accepted Unix-domain connection.
//!
//! Why: filesystem permissions alone are a documented intention. A `0600`
//! socket in a `0700` directory is unreachable to another uid *while those bits
//! hold*, but nothing in the process re-checks them — an operator who loosens
//! the directory, a backup tool that restores it wide, or a socket reached
//! before this crate's hardening ran would all pass unnoticed. Asking the
//! kernel who is on the other end turns the permission bits into an enforced
//! boundary. ADR-0034 §3 ("The trust boundary") requires it for this reason.
//!
//! What: [`peer_uid`] wraps the platform syscall — `SO_PEERCRED` on Linux,
//! `getpeereid` on macOS and the BSDs — [`peer_uid_verdict`] is the pure
//! comparison behind the refusal, and [`ensure_peer_is_self`] joins them.
//! [`peer_pid`] (#6642) answers the adjacent question — WHICH process is on the
//! other end — for a caller that wants to meter it rather than refuse it.
//! Targets with neither syscall fail closed via
//! [`UdsSecurityError::UnsupportedPlatform`] rather than compiling to an
//! unchecked accept.
//!
//! Test: `peer_uid_of_self_connection_is_self` and
//! `bind_hardened_socket_is_connectable_after_hardening` cover the syscall;
//! `peer_uid_verdict_refuses_a_foreign_uid` and
//! `peer_uid_verdict_accepts_the_same_uid` cover the decision without needing a
//! second uid.

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

/// Decide whether a peer uid is acceptable, as a pure function.
///
/// Why: the refusal branch is the security-critical one and cannot be reached
/// from an unprivileged test — connecting as a second uid needs root or a
/// pre-provisioned account. Taking both uids as arguments makes the decision
/// assertable directly, so only the syscall plumbing remains untested rather
/// than the whole policy (#5099 review finding 5).
///
/// What: returns [`UdsSecurityError::ForeignPeer`] unless `peer == own`. Root
/// is refused like any other foreign uid — root can bypass the filesystem check
/// anyway, so admitting it would buy nothing and widen the accepted set.
///
/// Test: `peer_uid_verdict_accepts_the_same_uid`,
/// `peer_uid_verdict_refuses_a_foreign_uid`,
/// `peer_uid_verdict_refuses_root_when_we_are_not_root`.
pub fn peer_uid_verdict(peer: u32, own: u32) -> Result<(), UdsSecurityError> {
    if peer != own {
        return Err(UdsSecurityError::ForeignPeer {
            peer,
            expected: own,
        });
    }
    Ok(())
}

/// Read the uid of the process on the other end of `stream`.
///
/// Why: split from [`ensure_peer_is_self`] so a caller that wants to log or
/// meter the peer identity does not have to duplicate the platform `cfg`s.
/// What: `getsockopt(SO_PEERCRED)` on Linux, `getpeereid(3)` elsewhere on unix.
/// The credentials the kernel reports are those captured at `connect` time, so
/// a peer cannot change uid after connecting to defeat the check.
/// Test: `peer_uid_of_self_connection_is_self`.
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &UnixStream) -> Result<u32, UdsSecurityError> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;

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

/// Read the pid of the process on the other end of `stream` (#6642).
///
/// Why: trusty-console has to answer "how much CPU is trusty-search using", and
/// a UDS daemon publishes no pid anywhere — no pid file, no field in its health
/// response. The kernel already knows, and the console already dials that exact
/// socket to detect the service, so the connection it makes IS the identifier.
/// This is the one implementation; a second consumer with the same question
/// calls it rather than shelling out to `lsof`.
///
/// Why the return type is `Option` and not `Result`: every caller so far treats
/// "cannot identify the peer" as a metric it simply does not have. There is no
/// remediation to report and nothing to fail on, so the platform gap, the
/// syscall error, and a nonsensical pid all collapse into the one answer the
/// caller acts on. Contrast [`peer_uid`], whose failure MUST refuse a
/// connection and therefore owes the caller a reason.
///
/// What: `getsockopt(SO_PEERCRED)`'s `ucred.pid` on Linux,
/// `getsockopt(SOL_LOCAL, LOCAL_PEERPID)` on macOS/iOS. The other BSDs expose
/// no peer-pid option, so they answer `None`. Like [`peer_uid`], the credentials
/// are those captured at `connect` time.
///
/// The pid identifies the process that was listening when the connection was
/// made. It can be reused after that process exits, so a caller holding one
/// across time must re-verify the process still exists — see
/// [`ProcessCpuSampler::refresh`](crate::sys_metrics::ProcessCpuSampler::refresh),
/// which drops a vanished pid for exactly this reason.
/// Test: `peer_pid_of_self_connection_is_this_process`.
#[cfg(target_os = "linux")]
#[must_use]
pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;

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
    if rc != 0 || cred.pid <= 0 {
        return None;
    }
    u32::try_from(cred.pid).ok()
}

/// See the Linux variant for the contract; this is the Darwin syscall.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[must_use]
pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = size_of::<libc::pid_t>() as libc::socklen_t;

    // SAFETY: `stream` owns a live connected socket fd for the duration of the
    // call; `pid` and `len` are a correctly-sized, correctly-typed
    // out-parameter pair for LOCAL_PEERPID on SOL_LOCAL.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    if rc != 0 || pid <= 0 {
        return None;
    }
    u32::try_from(pid).ok()
}

/// No peer-pid option on this target — the caller has no measurement.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
#[must_use]
pub fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
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
/// Why: the accept-side half of the `0600` contract. ADR-0034 §3: "the target
/// verifies peer credentials on accept and refuses any connection whose uid is
/// not its own. This is what makes the permission bits an enforced boundary
/// rather than a documented intention."
///
/// What: reads [`peer_uid`] and applies [`peer_uid_verdict`] against
/// [`self_uid`].
///
/// Test: `bind_hardened_socket_is_connectable_after_hardening` proves the
/// accept path for a same-uid peer; `peer_uid_verdict_refuses_a_foreign_uid`
/// proves the refusal decision without needing a second uid.
pub fn ensure_peer_is_self(stream: &UnixStream) -> Result<(), UdsSecurityError> {
    peer_uid_verdict(peer_uid(stream)?, self_uid())
}
