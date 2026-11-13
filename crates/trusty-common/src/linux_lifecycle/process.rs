//! Process primitives for the lifecycle: a pidfd-pinned process, its
//! executable name, and the socket dial that names the listener.
//!
//! Why: a pid is a number the kernel reuses. Signalling by number after a wait
//! can hit an unrelated process that took the number. A pidfd refers to one
//! process for its whole life: `pidfd_send_signal` on an exited process fails
//! with `ESRCH` instead of reaching a newcomer, and the fd turns readable the
//! moment the process exits (a zombie counts as exited), which is the
//! condition `stop` waits on.
//! What: [`PinnedProcess`], [`Signal`], `exe_name`, `dial`.
//! Test: `exe_name_strips_the_deleted_suffix`; the pidfd paths are exercised
//! by every real-process test in `tests/linux_lifecycle.rs`.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::net::UnixStream;

use super::{LifecycleError, PROBE_TIMEOUT};

/// A signal `stop` delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Signal {
    /// SIGTERM, the graceful request.
    Term,
    /// SIGKILL, after the stop grace.
    Kill,
}

impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// One process, held by a pidfd so its pid cannot be confused with a reuse.
///
/// Why: see the module docs — every signal and every exit wait in this module
/// goes through the pidfd, never through the bare pid number.
/// What: [`PinnedProcess::pid`], [`PinnedProcess::send_signal`], and the
/// crate-internal exit checks. It is only built by this module, from a pid it
/// identified.
/// Test: `stop_sends_sigkill_to_a_process_that_ignores_sigterm`,
/// `stop_reports_a_survivor_when_sigkill_does_not_take`.
#[derive(Debug)]
pub struct PinnedProcess {
    pid: u32,
    fd: AsyncFd<OwnedFd>,
}

impl PinnedProcess {
    /// `pidfd_open(pid)`. `Ok(None)` when no such process exists.
    pub(crate) fn open(pid: u32) -> Result<Option<Self>, LifecycleError> {
        let raw_pid = libc::pid_t::try_from(pid).map_err(|_| LifecycleError::Io {
            context: format!("pid {pid} out of range"),
            source: io::Error::from(io::ErrorKind::InvalidInput),
        })?;
        // SAFETY: pidfd_open takes a pid and a flags word and returns a new fd
        // or -1; it touches no caller memory.
        let rc = unsafe { libc::syscall(libc::SYS_pidfd_open, raw_pid, 0) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(LifecycleError::Io {
                context: format!("pidfd_open({pid})"),
                source: err,
            });
        }
        let raw_fd = i32::try_from(rc).map_err(|_| LifecycleError::Io {
            context: format!("pidfd_open({pid}) returned {rc}"),
            source: io::Error::from(io::ErrorKind::InvalidData),
        })?;
        // SAFETY: `raw_fd` was just returned by pidfd_open and is owned by no
        // one else; OwnedFd takes sole ownership and closes it on drop.
        let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let fd = AsyncFd::with_interest(owned, Interest::READABLE).map_err(|source| {
            LifecycleError::Io {
                context: format!("register pidfd of {pid}"),
                source,
            }
        })?;
        Ok(Some(Self { pid, fd }))
    }

    /// The pinned pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Deliver `signal` through the pidfd.
    ///
    /// Why: `kill(pid)` after a wait could reach a process that reused the
    /// number; the pidfd cannot.
    /// What: `pidfd_send_signal`. An exited process yields `ESRCH`, which the
    /// caller reads as "already gone", never as "stopped".
    /// Test: `stop_sends_sigkill_to_a_process_that_ignores_sigterm`.
    pub fn send_signal(&self, signal: Signal) -> io::Result<()> {
        // SAFETY: the pidfd is open for the lifetime of `self`; a null siginfo
        // and zero flags are the documented "like kill(2)" form.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.get_ref().as_raw_fd(),
                signal.raw(),
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether the process has exited (a zombie counts), without waiting.
    pub(crate) fn has_exited(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.fd.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, zero timeout.
        let rc = unsafe { libc::poll(&raw mut pfd, 1, 0) };
        rc > 0 && pfd.revents & libc::POLLIN != 0
    }

    /// Wait up to `limit` for the process to exit; true when it did.
    ///
    /// What: awaits readability of the pidfd, which the kernel raises on exit.
    /// A readiness event is re-checked with `poll`, so only a real exit ends
    /// the wait early.
    pub(crate) async fn wait_exit(&self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if self.has_exited() {
                return true;
            }
            match tokio::time::timeout_at(deadline, self.fd.readable()).await {
                Ok(Ok(mut guard)) => {
                    if !self.has_exited() {
                        guard.clear_ready();
                    }
                }
                // A reactor error leaves the poll check as the only answer.
                Ok(Err(_)) | Err(_) => return self.has_exited(),
            }
        }
    }
}

/// The executable file name of `pid`, from `/proc/<pid>/exe`.
///
/// Why: the peer pid alone does not say the process is the daemon; a reused
/// pid or another program serving the path must never be signalled.
/// What: the link target's file name, with the ` (deleted)` suffix the kernel
/// appends after the binary was replaced on disk — the exact state of a daemon
/// that outlived an upgrade, which must still be recognised.
/// Test: `exe_name_strips_the_deleted_suffix`.
pub(crate) fn exe_name(pid: u32) -> io::Result<String> {
    let target = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    Ok(strip_deleted(&target.to_string_lossy()))
}

/// Pure half of [`exe_name`]: file name of a `/proc/<pid>/exe` link target.
pub(crate) fn strip_deleted(link_target: &str) -> String {
    let trimmed = link_target
        .strip_suffix(" (deleted)")
        .unwrap_or(link_target);
    Path::new(trimmed)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Outcome of dialing the socket.
pub(crate) enum Dial {
    /// Connected; carries the listener's pid when the kernel reported one.
    Peer(Option<u32>),
    /// ENOENT or ECONNREFUSED: nothing listens.
    NotServing,
    /// Timeout or another errno: undecided.
    Inconclusive,
}

/// Connect once and read the peer pid.
///
/// Why: `uds::probe_socket_verdict` drops its stream, and the stream is what
/// `SO_PEERCRED` reads, so this keeps it long enough to ask. The errno
/// classification is the same as that probe's.
pub(crate) async fn dial(socket: &Path) -> Dial {
    match tokio::time::timeout(PROBE_TIMEOUT, UnixStream::connect(socket)).await {
        Ok(Ok(stream)) => Dial::Peer(crate::uds::peer_pid(&stream)),
        Ok(Err(e))
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Dial::NotServing
        }
        Ok(Err(_)) | Err(_) => Dial::Inconclusive,
    }
}
