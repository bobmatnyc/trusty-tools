//! Sizing `SO_SNDBUF` / `SO_RCVBUF` on every Unix socket this module owns
//! (#6896).
//!
//! Why: macOS ships `net.local.stream.sendspace` and `.recvspace` at 8192
//! bytes, and nothing in this workspace ever raised them. A daemon frame is
//! budgeted at [`crate::uds::MAX_FRAME_BYTES`] (8 MiB), and `trusty-memory`
//! raises its own to 32 MiB — so a multi-MiB frame moves through an 8 KiB pipe
//! in roughly a thousand write-then-drain round trips, each one a task park and
//! unpark. Measured off the #6876 fix on this host, the two frame-budget
//! exchanges cost ~235 ms and ~204 ms unloaded and inflated about 45x, to ~11 s
//! and ~9 s, under 480 concurrent processes, while connection setup stayed at
//! ~1 ms. The cost is the round trips, not the bytes.
//!
//! What: [`tune_socket_buffers`] sets both buffers to [`SOCKET_BUFFER_BYTES`]
//! and reads back what the kernel granted, because the kernel clamps rather
//! than refuses. Two call sites cover both ends of every socket:
//!   - [`crate::uds::bind_hardened`] sizes the LISTENER. Both platforms copy a
//!     listener's buffer sizes onto every socket `accept` returns, so this is
//!     the whole server side — including a service such as `trusty-embedderd`
//!     that binds through here and then drives its own accept loop.
//!   - [`crate::uds::connect_hardened`] sizes the dialling end.
//!
//! 🔴 **The listener is sized, not each accepted socket, and that is the fix
//! rather than a shortcut.** macOS refuses `setsockopt(SO_SNDBUF)` with EINVAL
//! once a socket's peer has hung up — `getpeername` on the same fd fails the
//! same way, which is what identifies the condition. An accepted socket can
//! already be in that state before the server touches it: a liveness probe
//! connects and closes, which is exactly what
//! [`crate::uds::probe::socket_is_serving`] does. Sizing per accepted socket
//! therefore turned every probe into a connection failure. Inheriting from the
//! listener happens inside `accept`, before any peer can go away.
//!
//! **A failure is propagated, never defaulted — with one named exception.** A
//! socket left at the platform default still works, at the cost this module
//! exists to remove and silently, so a real `setsockopt` failure is an error.
//! The exception is a socket whose peer has ALREADY hung up: it is reported as
//! [`SocketBufferOutcome::PeerHungUp`] rather than sized, because it will carry
//! no frame at all and the caller's next read or write is what tells it so.
//! That case is proven with `getpeername`, not inferred from the errno.
//!
//! **Linux is unaffected or better.** `net.core.wmem_default` is 212992 there,
//! 26x the macOS figure, a request above `wmem_max` is clamped silently rather
//! than refused, and `setsockopt` does not consult the connection state — so
//! this raises the buffer where the host allows it and changes nothing where it
//! does not.
//!
//! Test: `tune_socket_buffers_raises_both_buffers_on_a_socketpair`,
//! `tune_socket_buffers_reports_a_peer_that_already_hung_up`,
//! `an_accepted_socket_inherits_the_listeners_sizing`,
//! `hardened_sockets_hold_far_more_in_flight_than_the_platform_default`.

use std::io;
use std::os::fd::AsRawFd;

use super::UdsSecurityError;
use super::rpc::MAX_FRAME_BYTES;

/// Bytes requested for `SO_SNDBUF` and `SO_RCVBUF` on every socket this module
/// creates or accepts.
///
/// Why an eighth of [`MAX_FRAME_BYTES`] rather than the whole frame: the buffer
/// is charged to the kernel per socket, and the load this issue was measured
/// under is 480 concurrent processes. A full 8 MiB frame budget on both ends of
/// each of those is memory the machine does not have to spare for a saving that
/// has already been taken — 1 MiB is 128x the macOS default and drops an 8 MiB
/// frame from 1024 round trips to 8, where the whole frame budget would drop it
/// to 1. Neither platform refuses a larger request (macOS clamps to
/// `kern.ipc.maxsockbuf`, Linux to `net.core.wmem_max`), so this is a memory
/// decision, not a compatibility one.
///
/// Test: `socket_buffer_request_stays_within_the_frame_budget`.
pub const SOCKET_BUFFER_BYTES: usize = (MAX_FRAME_BYTES / 8) as usize;

/// What the kernel actually granted, after its own clamping.
///
/// Why read back at all: `setsockopt` succeeds while granting less than was
/// asked for — Linux clamps to `net.core.wmem_max`, macOS to
/// `kern.ipc.maxsockbuf`, and both round. A caller that assumed the request was
/// honoured would report a buffer size the socket does not have.
///
/// The two figures are not comparable across platforms: Linux doubles the value
/// it reports to account for its own bookkeeping, macOS does not.
///
/// Test: `tune_socket_buffers_raises_both_buffers_on_a_socketpair`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SocketBufferSizes {
    /// Effective `SO_SNDBUF`, as the kernel reports it.
    pub send: usize,
    /// Effective `SO_RCVBUF`, as the kernel reports it.
    pub recv: usize,
}

/// Whether [`tune_socket_buffers`] had a live socket to size.
///
/// Why this is not folded into an error: a socket whose peer has hung up is not
/// a failure to report — the caller's next read or write reports it, in the
/// shape the caller already handles (`UdsRpcError::NoResponse`,
/// `Served::LivenessProbe`). Naming the case keeps it distinguishable from a
/// socket that was genuinely sized, without turning a routine liveness probe
/// into a connection error. See the module docs.
///
/// Test: `tune_socket_buffers_reports_a_peer_that_already_hung_up`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SocketBufferOutcome {
    /// Both buffers were set; these are the sizes the kernel granted.
    Sized(SocketBufferSizes),
    /// The peer had already hung up, so nothing was sized.
    PeerHungUp,
}

/// Whether `fd` still has a peer.
///
/// macOS answers EINVAL from both `getpeername` and `setsockopt` once the peer
/// is gone, so this is the positive check that tells the two apart rather than
/// reading a meaning into the errno.
fn still_connected(fd: i32) -> bool {
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: `fd` is borrowed from a live fd for the duration of the call, and
    // the buffer is a stack `sockaddr_un` whose length is passed in.
    let rc = unsafe {
        libc::getpeername(
            fd,
            std::ptr::from_mut(&mut addr).cast::<libc::sockaddr>(),
            &raw mut len,
        )
    };
    rc == 0
}

/// Ask for `bytes` on one buffer option.
fn set_buffer(
    fd: i32,
    option: i32,
    name: &'static str,
    bytes: usize,
) -> Result<(), UdsSecurityError> {
    let refuse = |source| UdsSecurityError::SocketBuffer {
        option: name,
        requested: bytes,
        source,
    };
    let requested = libc::c_int::try_from(bytes).map_err(|_| {
        refuse(io::Error::new(
            io::ErrorKind::InvalidInput,
            "buffer size does not fit a C int",
        ))
    })?;
    // SAFETY: `fd` is borrowed from a live socket for the duration of the call,
    // and the value pointer is a stack `c_int` matching the declared length.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            std::ptr::from_ref(&requested).cast::<libc::c_void>(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(refuse(io::Error::last_os_error()));
    }
    Ok(())
}

/// Read one buffer option back.
fn read_buffer(fd: i32, option: i32, name: &'static str) -> Result<usize, UdsSecurityError> {
    let mut value: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: as `set_buffer` — a live fd, and a stack `c_int` whose length is
    // passed in and updated in place.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            std::ptr::from_mut(&mut value).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(UdsSecurityError::SocketBuffer {
            option: name,
            requested: SOCKET_BUFFER_BYTES,
            source: io::Error::last_os_error(),
        });
    }
    Ok(usize::try_from(value).unwrap_or(0))
}

/// Read a socket's effective buffer sizes without changing them.
///
/// Why: the only way to observe what a socket carries — an accepted socket's
/// sizing comes from the listener, so asking it is the one check that proves
/// the inheritance this module relies on (#6896).
///
/// # Errors
///
/// [`UdsSecurityError::SocketBuffer`] when either option cannot be read.
///
/// Test: `an_accepted_socket_inherits_the_listeners_sizing`.
pub fn socket_buffer_sizes<S: AsRawFd + ?Sized>(
    socket: &S,
) -> Result<SocketBufferSizes, UdsSecurityError> {
    let fd = socket.as_raw_fd();
    Ok(SocketBufferSizes {
        send: read_buffer(fd, libc::SO_SNDBUF, "SO_SNDBUF")?,
        recv: read_buffer(fd, libc::SO_RCVBUF, "SO_RCVBUF")?,
    })
}

/// Size both socket buffers to [`SOCKET_BUFFER_BYTES`] and report what stuck.
///
/// Why: the one place in this workspace that sets these options, so the size
/// and the read-back cannot drift between the bind and connect paths (#6896,
/// CLAUDE.md "common entry point").
///
/// What: `setsockopt` for `SO_SNDBUF` then `SO_RCVBUF`, then `getsockopt` for
/// both, logged at debug level — the kernel clamps silently, so the granted
/// size is the only figure worth reporting. A `setsockopt` failure on a socket
/// `getpeername` says has no peer left is reported as
/// [`SocketBufferOutcome::PeerHungUp`].
///
/// # Errors
///
/// [`UdsSecurityError::SocketBuffer`] when either option cannot be set on a
/// still-connected socket, or cannot be read back.
///
/// Test: `tune_socket_buffers_raises_both_buffers_on_a_socketpair`,
/// `tune_socket_buffers_reports_a_peer_that_already_hung_up`.
pub fn tune_socket_buffers<S: AsRawFd + ?Sized>(
    socket: &S,
) -> Result<SocketBufferOutcome, UdsSecurityError> {
    let fd = socket.as_raw_fd();
    for (option, name) in [
        (libc::SO_SNDBUF, "SO_SNDBUF"),
        (libc::SO_RCVBUF, "SO_RCVBUF"),
    ] {
        if let Err(e) = set_buffer(fd, option, name, SOCKET_BUFFER_BYTES) {
            // #6896: a socket that lost its peer will carry no frame, so there
            // is nothing to size and nothing to report as broken. BOTH signals
            // are required — the errno macOS uses for a torn-down socket buffer,
            // and `getpeername` agreeing there is no peer. A listener has no
            // peer either, so the errno alone would let a genuine failure on one
            // pass as this case.
            if e.is_disconnected_socket() && !still_connected(fd) {
                tracing::debug!("uds socket buffers left unsized; the peer had already hung up");
                return Ok(SocketBufferOutcome::PeerHungUp);
            }
            return Err(e);
        }
    }

    let sizes = socket_buffer_sizes(socket)?;
    tracing::debug!(
        requested = SOCKET_BUFFER_BYTES,
        send = sizes.send,
        recv = sizes.recv,
        "sized uds socket buffers"
    );
    Ok(SocketBufferOutcome::Sized(sizes))
}
