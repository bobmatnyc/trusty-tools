//! Sizing `SO_SNDBUF` / `SO_RCVBUF` on every Unix socket this module owns
//! (#6896).
//!
//! Why: macOS ships `net.local.stream.sendspace` and `.recvspace` at 8192
//! bytes, and nothing in this workspace ever raised them. Every frame a daemon
//! exchanges is multi-KiB at least and multi-MiB at the budget, so it moves
//! through an 8 KiB pipe in hundreds or thousands of write-then-drain round
//! trips, each one a task park and unpark. Measured off the #6876 fix on this
//! host, the two frame-budget exchanges cost ~235 ms and ~204 ms unloaded and
//! inflated about 45x, to ~11 s and ~9 s, under 480 concurrent processes, while
//! connection setup stayed at ~1 ms. The cost is the round trips, not the bytes.
//!
//! What: two entry points, differing only in what they do when the socket's
//! peer has already hung up. Both set both buffers to [`SOCKET_BUFFER_BYTES`]
//! and read back what the kernel granted, because the kernel clamps rather than
//! refuses.
//!   - [`tune_listener_buffers`] is for a socket that has no peer and never
//!     will: any failure is a failure. [`crate::uds::bind_hardened`] calls it.
//!   - [`tune_connected_buffers`] is for a socket that has a peer, which may
//!     have gone away. [`crate::uds::connect_hardened`] calls it for the
//!     dialling end and [`crate::uds::accept_sized`] for the accepted end.
//!
//! 🔴 **Why the split is not a stylistic one (#6896 review).** The benign
//! classification rests on `getpeername` failing, and `getpeername` ALWAYS
//! fails on a listening socket — it has no peer to name. A single forgiving
//! entry point therefore degrades, on the listener, to trusting the errno
//! alone: any `EINVAL` from `setsockopt` would be recorded as a hung-up peer,
//! and `bind_hardened` would return a listener silently left at the platform
//! default. The strict form has no benign outcome to reach, in the type as well
//! as in the code.
//!
//! 🔴 **Both the listener AND each accepted socket are sized, because only
//! macOS inherits (#6896 follow-up).** macOS builds the server-side socket in
//! `sonewconn`, which copies the listener's `sb_hiwat` onto it, so sizing the
//! listener there covers everything `accept` returns. Linux does not: AF_UNIX
//! builds the server-side socket from scratch in `unix_stream_connect`, so it
//! comes back at `net.core.wmem_default` / `rmem_default` however the listener
//! was sized. Sizing only the listener therefore left every server-side socket
//! on Linux at the platform default. [`crate::uds::accept_sized`] closes that,
//! through the forgiving entry point: an accepted socket can have lost its peer
//! before the server touches it — a liveness probe connects and closes, which
//! is exactly what [`crate::uds::probe::socket_is_serving`] does — and that
//! peer-hangup is what [`SocketBufferOutcome::PeerHungUp`] absorbs rather than
//! turning every probe into a connection failure.
//!
//! **A failure is propagated, never defaulted — with one named exception.** A
//! socket left at the platform default still works, at the cost this module
//! exists to remove and silently, so a real `setsockopt` failure is an error.
//! The exception is a CONNECTED socket whose peer has already hung up: it is
//! reported as [`SocketBufferOutcome::PeerHungUp`] rather than sized, because
//! it will carry no frame at all and the caller's next read or write is what
//! tells it so.
//!
//! **Linux is unaffected or better.** `net.core.wmem_default` is 212992 there,
//! 26x the macOS figure, a request above `wmem_max` is clamped silently rather
//! than refused, and `setsockopt` does not consult the connection state — so
//! this raises the buffer where the host allows it and changes nothing where it
//! does not.
//!
//! 🟡 **Nothing here compares a read-back against what was requested.** Linux
//! stores twice the accepted value and `getsockopt` returns that doubled
//! figure, so a request of 1 MiB reads back as 2 MiB — or as 425984 on a host
//! whose `wmem_max` is 212992, the clamp applied first and the doubling after.
//! The read-back is logged and returned for observation, never asserted
//! against [`SOCKET_BUFFER_BYTES`]. A caller comparing two sockets on the same
//! host is comparing like with like; a caller comparing either against the
//! request is not.
//!
//! Test: `tune_connected_buffers_raises_both_buffers_on_a_socketpair`,
//! `tune_connected_buffers_reports_a_peer_that_already_hung_up`,
//! `a_listener_can_never_classify_a_failure_as_a_hung_up_peer`,
//! `tune_listener_buffers_refuses_where_the_connected_form_tolerates`,
//! `accept_sized_raises_the_accepted_socket_to_the_listeners_sizing`,
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
/// each of those is memory the machine does not have to spare, for a saving
/// that has already been taken — 1 MiB is 128x the macOS default and drops an
/// 8 MiB frame from 1024 round trips to 8. Neither platform refuses a larger
/// request (macOS clamps to `kern.ipc.maxsockbuf`, Linux to
/// `net.core.wmem_max`), so this is a memory decision, not a compatibility one.
///
/// 🔴 **One size for every consumer, and it is not proportional to any
/// consumer's own frame budget (#6896 review).** A service that raises its
/// budget above [`MAX_FRAME_BYTES`] gets this same 1 MiB: `trusty-memory`
/// serves 32 MiB frames, and a 32 MiB frame here still costs about 32 round
/// trips rather than the 4 a proportional buffer would give. That is the
/// deliberate trade — 32 round trips against the ~4,100 the 8 KiB default
/// charged — not an oversight, and not something a per-consumer parameter
/// should be added to address.
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
/// Test: `tune_connected_buffers_raises_both_buffers_on_a_socketpair`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SocketBufferSizes {
    /// Effective `SO_SNDBUF`, as the kernel reports it.
    pub send: usize,
    /// Effective `SO_RCVBUF`, as the kernel reports it.
    pub recv: usize,
}

/// Whether [`tune_connected_buffers`] had a live socket to size.
///
/// Why this is not folded into an error: a socket whose peer has hung up is not
/// a failure to report — the caller's next read or write reports it, in the
/// shape the caller already handles (`UdsRpcError::NoResponse`,
/// `Served::LivenessProbe`). Naming the case keeps it distinguishable from a
/// socket that was genuinely sized, without turning a routine liveness probe
/// into a connection error.
///
/// [`tune_listener_buffers`] does not return this type at all — see the module
/// docs for why a listener must not be able to reach this outcome.
///
/// Test: `tune_connected_buffers_reports_a_peer_that_already_hung_up`.
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
/// 🔴 Always false for a LISTENING socket, which has no peer to name — which is
/// why [`hangup_is_benign`] takes `connected` rather than relying on this alone.
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

/// Whether a `setsockopt` failure on `fd` may be recorded as a hung-up peer
/// rather than reported.
///
/// Why a named function: this is the whole fail-closed decision, and it is the
/// one the #6896 review found could not be made from the errno and
/// `getpeername` alone. `connected` is the caller's statement about what kind
/// of socket it holds; a listener passes `false` and can never reach the benign
/// arm, whatever the errno and whatever `getpeername` says.
///
/// Test: `a_listener_can_never_classify_a_failure_as_a_hung_up_peer`.
pub(crate) fn hangup_is_benign(connected: bool, error: &UdsSecurityError, fd: i32) -> bool {
    connected && error.is_disconnected_socket() && !still_connected(fd)
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
        // #6896 review: its OWN variant. Reporting a read-back failure through
        // `SocketBuffer` printed "size SO_SNDBUF to 1048576 bytes", naming an
        // operation this branch never attempted.
        return Err(UdsSecurityError::SocketBufferRead {
            option: name,
            source: io::Error::last_os_error(),
        });
    }
    Ok(usize::try_from(value).unwrap_or(0))
}

/// Read a socket's effective buffer sizes without changing them.
///
/// Why: the only way to observe what a socket carries. What the kernel granted
/// is not what was asked for — Linux doubles it and clamps it first — so a
/// caller checking that a socket really is sized has to read it back rather
/// than assume (#6896).
///
/// # Errors
///
/// [`UdsSecurityError::SocketBufferRead`] when either option cannot be read.
///
/// Test: `accept_sized_raises_the_accepted_socket_to_the_listeners_sizing`.
pub fn socket_buffer_sizes<S: AsRawFd + ?Sized>(
    socket: &S,
) -> Result<SocketBufferSizes, UdsSecurityError> {
    let fd = socket.as_raw_fd();
    Ok(SocketBufferSizes {
        send: read_buffer(fd, libc::SO_SNDBUF, "SO_SNDBUF")?,
        recv: read_buffer(fd, libc::SO_RCVBUF, "SO_RCVBUF")?,
    })
}

/// Set both buffers, tolerating a hung-up peer only when `connected` says the
/// socket has one.
fn tune(fd: i32, connected: bool) -> Result<Option<SocketBufferSizes>, UdsSecurityError> {
    for (option, name) in [
        (libc::SO_SNDBUF, "SO_SNDBUF"),
        (libc::SO_RCVBUF, "SO_RCVBUF"),
    ] {
        if let Err(e) = set_buffer(fd, option, name, SOCKET_BUFFER_BYTES) {
            if hangup_is_benign(connected, &e, fd) {
                tracing::debug!("uds socket buffers left unsized; the peer had already hung up");
                return Ok(None);
            }
            return Err(e);
        }
    }
    let sizes = SocketBufferSizes {
        send: read_buffer(fd, libc::SO_SNDBUF, "SO_SNDBUF")?,
        recv: read_buffer(fd, libc::SO_RCVBUF, "SO_RCVBUF")?,
    };
    tracing::debug!(
        requested = SOCKET_BUFFER_BYTES,
        send = sizes.send,
        recv = sizes.recv,
        "sized uds socket buffers"
    );
    Ok(Some(sizes))
}

/// Size the buffers of a socket that has no peer, failing on any error.
///
/// Why: a listener is the one socket for which the hung-up-peer classification
/// cannot be evaluated — see the module docs. There is no benign outcome in the
/// return type, so a caller cannot proceed on an unsized listener without
/// seeing an error (#6896 review).
///
/// What: `setsockopt` for both options, then `getsockopt` for both, at debug
/// level. macOS copies what this sets onto every socket `accept` returns; Linux
/// does not, which is why [`crate::uds::accept_sized`] sizes the accepted end
/// as well.
///
/// # Errors
///
/// [`UdsSecurityError::SocketBuffer`] when either option cannot be set,
/// [`UdsSecurityError::SocketBufferRead`] when either cannot be read back.
///
/// Test: `a_listener_can_never_classify_a_failure_as_a_hung_up_peer`,
/// `tune_listener_buffers_refuses_where_the_connected_form_tolerates`,
/// `accept_sized_raises_the_accepted_socket_to_the_listeners_sizing`.
pub fn tune_listener_buffers<S: AsRawFd + ?Sized>(
    socket: &S,
) -> Result<SocketBufferSizes, UdsSecurityError> {
    match tune(socket.as_raw_fd(), false)? {
        Some(sizes) => Ok(sizes),
        // Unreachable by construction: `hangup_is_benign` returns false for
        // every error when `connected` is false, so `tune` cannot answer `None`.
        // Reported rather than asserted, because a panic in a bind path is a
        // worse failure than an error the caller already handles.
        None => Err(UdsSecurityError::SocketBuffer {
            option: "SO_SNDBUF",
            requested: SOCKET_BUFFER_BYTES,
            source: io::Error::other("a listener cannot have a hung-up peer"),
        }),
    }
}

/// Size the buffers of a connected socket, reporting a peer that has already
/// hung up rather than failing on it.
///
/// Why: the dialling end of a pair the server may have dropped between
/// `connect` returning and this call — the #6601 shutdown drain does exactly
/// that. Such a socket will carry no frame, so there is nothing to size and
/// nothing to report as broken.
///
/// What: as [`tune_listener_buffers`], except that a `setsockopt` failure whose
/// errno says the socket buffer is torn down AND whose `getpeername` confirms
/// there is no peer returns [`SocketBufferOutcome::PeerHungUp`]. Every other
/// failure is an error.
///
/// # Errors
///
/// [`UdsSecurityError::SocketBuffer`] when either option cannot be set on a
/// still-connected socket, [`UdsSecurityError::SocketBufferRead`] when either
/// cannot be read back.
///
/// Test: `tune_connected_buffers_raises_both_buffers_on_a_socketpair`,
/// `tune_connected_buffers_reports_a_peer_that_already_hung_up`,
/// `tune_listener_buffers_refuses_where_the_connected_form_tolerates`.
pub fn tune_connected_buffers<S: AsRawFd + ?Sized>(
    socket: &S,
) -> Result<SocketBufferOutcome, UdsSecurityError> {
    Ok(match tune(socket.as_raw_fd(), true)? {
        Some(sizes) => SocketBufferOutcome::Sized(sizes),
        None => SocketBufferOutcome::PeerHungUp,
    })
}
