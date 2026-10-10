//! Process control for `tm secrets exec` (#9621): signals, the child's
//! process group, the terminal and inherited fds.
//!
//! Why: the stub runs in its own process group so tm can stop it, and
//! everything it starts, with one `killpg` on any failure. A group of its own
//! is a background group, so when tm owns the terminal it hands the terminal
//! to the child's group and takes it back afterwards; otherwise an
//! interactive command stops on its first terminal read (SIGTTIN).
//! What: [`Signals`] (SIGINT, SIGTERM, SIGHUP sent to tm), [`kill_group`],
//! [`Foreground`] (the terminal handoff, undone on drop) and
//! [`clear_cloexec`] / [`set_cloexec`] (keep one fd across exec, or not).
//! Test: `tests/tm_secrets_exec.rs` — `exec_forwards_sigterm_and_revokes`,
//! `exec_kills_the_stub_group_when_the_grant_is_refused`,
//! `exec_drain_after_exit_answers_terminal_ctrl_c` ([`Foreground`]'s drop).

use std::io;
use std::os::fd::RawFd;

use tokio::signal::unix::{Signal, SignalKind, signal};

/// The three signals tm forwards to the child's group.
pub(crate) struct Signals {
    int: Signal,
    term: Signal,
    hup: Signal,
}

impl Signals {
    /// Install the handlers. Done before the spawn, so no signal is missed.
    pub(crate) fn install() -> io::Result<Self> {
        Ok(Self {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
        })
    }

    /// The next signal tm received, as its number.
    pub(crate) async fn next(&mut self) -> i32 {
        tokio::select! {
            _ = self.int.recv() => libc::SIGINT,
            _ = self.term.recv() => libc::SIGTERM,
            _ = self.hup.recv() => libc::SIGHUP,
        }
    }
}

/// Send `sig` to the process group `pgid`.
///
/// What: `pgid` is the stub's pid, which `process_group(0)` made its group
/// id. The caller signals before it reaps the child, so the id is still
/// reserved. A group with no members left (ESRCH) is ignored.
pub(crate) fn kill_group(pgid: u32, sig: i32) {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return;
    };
    if pgid <= 1 {
        return;
    }
    // SAFETY: plain syscall on a positive group id; no memory is shared.
    unsafe {
        libc::killpg(pgid, sig);
    }
}

/// Clear `FD_CLOEXEC` on `fd`, so it survives the next exec.
///
/// What: async-signal-safe (`fcntl` only), so it may run in `pre_exec`.
pub(crate) fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    cloexec(fd, false)
}

/// Set `FD_CLOEXEC` on `fd`, so no later exec or spawn inherits it (#9621).
pub(crate) fn set_cloexec(fd: RawFd) -> io::Result<()> {
    cloexec(fd, true)
}

/// Set or clear `FD_CLOEXEC` on `fd`; `fcntl` only.
fn cloexec(fd: RawFd, on: bool) -> io::Result<()> {
    // SAFETY: `fcntl` on an fd number; an invalid fd is an error, not UB.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if on {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The terminal handed to the child's group; handed back on drop.
///
/// Why: see the module doc. Terminal keys (Ctrl-C) then reach the command,
/// and tm sees it exit. Known limit: Ctrl-Z stops the command but not tm.
pub(crate) struct Foreground {
    /// tm's own group, when the terminal was handed over.
    restore: Option<libc::pid_t>,
}

impl Foreground {
    /// Hand stdin's terminal to `pgid` when tm's group owns it; else nothing.
    pub(crate) fn hand_to(pgid: u32) -> Self {
        let none = Self { restore: None };
        let Ok(pgid) = libc::pid_t::try_from(pgid) else {
            return none;
        };
        // SAFETY: plain syscalls on fd 0 and this process; no memory shared.
        unsafe {
            if libc::isatty(0) != 1 {
                return none;
            }
            let own = libc::getpgrp();
            if libc::tcgetpgrp(0) != own {
                return none;
            }
            // Close the spawn race: the child may not have run setpgid yet.
            libc::setpgid(pgid, pgid);
            if libc::tcsetpgrp(0, pgid) != 0 {
                return none;
            }
            Self { restore: Some(own) }
        }
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        let Some(own) = self.restore.take() else {
            return;
        };
        // SAFETY: signal-mask and terminal syscalls on this thread and fd 0.
        // tm is in a background group now, so SIGTTOU is blocked on this
        // thread for the call; POSIX then allows it and sends no signal.
        unsafe {
            let mut block: libc::sigset_t = std::mem::zeroed();
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut block);
            libc::sigaddset(&mut block, libc::SIGTTOU);
            libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut old);
            libc::tcsetpgrp(0, own);
            libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        }
    }
}
