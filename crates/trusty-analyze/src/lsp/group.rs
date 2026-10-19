//! The process group a language server leads, and the one gate every signal to
//! it passes through (#9078).
//!
//! Why: `killpg(pgid)` is only safe while the leader is unreaped. An unreaped
//! leader, alive or a zombie, holds its pid, so the pgid cannot be reused. Once
//! it is reaped and its helpers are gone, the kernel may hand that number to an
//! unrelated process that leads its own group, and a late `killpg` would kill
//! that process.
//! What: [`monitor`] owns the [`Child`]. It detects the leader's exit with
//! `waitid(WNOWAIT)`, which does not reap. It then sweeps the group once, reaps
//! the leader, and marks the group gone, all under the gate. [`ProcessGroup::kill`]
//! takes the same gate and sends nothing once the group is gone. So every
//! `killpg` runs while the pgid is still reserved, and none runs after the reap.
//! Test: `the_group_is_swept_once_at_reap_and_never_signalled_again`,
//! `dropping_the_manager_kills_the_child_and_its_descendants`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::Child;
use tokio::sync::{watch, Notify};

use super::lock;

/// How often the monitor re-checks the leader when no SIGCHLD arrives.
const POLL: Duration = Duration::from_millis(250);

/// A spawned server's process group (its pgid is the leader's pid).
///
/// Test: `the_group_is_swept_once_at_reap_and_never_signalled_again`.
pub(crate) struct ProcessGroup {
    pgid: u32,
    /// `true` once the leader is reaped. Guards every signal.
    gone: Mutex<bool>,
    signals: AtomicUsize,
}

impl ProcessGroup {
    /// The group led by `pgid`, a child spawned with `process_group(0)`.
    pub(crate) fn new(pgid: u32) -> Self {
        Self {
            pgid,
            gone: Mutex::new(false),
            signals: AtomicUsize::new(0),
        }
    }

    /// SIGKILL the whole group, unless the leader was already reaped.
    pub(crate) fn kill(&self) {
        let gone = lock(&self.gone);
        if !*gone {
            self.signal();
        }
    }

    /// Whether the leader has been reaped; no signal is sent after that.
    #[cfg(test)]
    pub(crate) fn is_gone(&self) -> bool {
        *lock(&self.gone)
    }

    /// How many `killpg` calls this group has issued.
    pub(crate) fn signals_sent(&self) -> usize {
        self.signals.load(Ordering::SeqCst)
    }

    /// Sweep the group once and run `reap`, all under the gate. The group is
    /// marked gone only when `reap` reports the leader reaped.
    fn sweep_then_reap<T>(&self, sweep: bool, reap: impl FnOnce() -> Option<T>) -> Option<T> {
        let mut gone = lock(&self.gone);
        if *gone {
            return None;
        }
        if sweep {
            self.signal();
        }
        let reaped = reap();
        if reaped.is_some() {
            *gone = true;
        }
        reaped
    }

    #[cfg(unix)]
    fn signal(&self) {
        let Ok(pgid) = libc::pid_t::try_from(self.pgid) else {
            return;
        };
        // Never init or our own group: `pgid` is a child that leads a group of
        // its own.
        if pgid > 1 {
            self.signals.fetch_add(1, Ordering::SeqCst);
            // SAFETY: killpg only sends a signal. The caller holds the gate and
            // the leader is unreaped, so `pgid` still names this group.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }

    #[cfg(not(unix))]
    fn signal(&self) {}
}

/// Whether the leader has exited, checked without reaping it.
#[cfg(unix)]
enum Leader {
    Running,
    Exited,
    /// `waitid` failed; the monitor falls back to a plain reap with no sweep.
    Unknown,
}

#[cfg(unix)]
fn leader_state(pid: u32) -> Leader {
    // `id_t` is `u32` on Linux and macOS, the pid's own type.
    let id: libc::id_t = pid;
    // SAFETY: an all-zero siginfo_t is a valid value, and waitid with WNOWAIT
    // only fills it in; it never reaps.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Leader::Unknown;
    }
    // SAFETY: after a successful waitid, si_pid is the field it wrote (0 when
    // the child has not exited, under WNOHANG).
    if unsafe { info.si_pid() } == 0 {
        Leader::Running
    } else {
        Leader::Exited
    }
}

/// Own `child` until it exits, then publish its exit status on `exit`.
///
/// A `kill` notification SIGKILLs the group while the leader is still
/// unreaped. On any exit the group is swept once, then the leader is reaped.
///
/// Test: `the_group_is_swept_once_at_reap_and_never_signalled_again`.
#[cfg(unix)]
pub(crate) async fn monitor(
    mut child: Child,
    group: Arc<ProcessGroup>,
    kill: Arc<Notify>,
    exit: watch::Sender<Option<String>>,
) {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigchld = signal(SignalKind::child()).ok();
    let status = loop {
        let reaped = match leader_state(group.pgid) {
            Leader::Running => None,
            Leader::Exited => group.sweep_then_reap(true, || child.try_wait().transpose()),
            Leader::Unknown => group.sweep_then_reap(false, || child.try_wait().transpose()),
        };
        if let Some(status) = reaped {
            break status;
        }
        tokio::select! {
            () = kill.notified() => group.kill(),
            _ = async {
                match sigchld.as_mut() {
                    Some(s) => { s.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
            () = tokio::time::sleep(POLL) => {}
        }
    };
    let status = match status {
        Ok(s) => s.to_string(),
        Err(e) => format!("wait failed: {e}"),
    };
    tracing::debug!(pgid = group.pgid, signals = group.signals_sent(), %status, "language server reaped");
    let _ = exit.send(Some(status));
}

/// Non-Unix fallback: no process groups, so a kill is the leader's alone.
#[cfg(not(unix))]
pub(crate) async fn monitor(
    mut child: Child,
    group: Arc<ProcessGroup>,
    kill: Arc<Notify>,
    exit: watch::Sender<Option<String>>,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        () = kill.notified() => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    let _ = group.sweep_then_reap(false, || Some(()));
    let status = match status {
        Ok(s) => s.to_string(),
        Err(e) => format!("wait failed: {e}"),
    };
    tracing::debug!(pgid = group.pgid, signals = group.signals_sent(), %status, "language server reaped");
    let _ = exit.send(Some(status));
}
