//! Child-process lifecycle for [`super::UdsServiceSupervisor`] (#5089).
//!
//! Why: spawn, graceful termination, socket-file cleanup and the RSS
//! measurement are the four mechanical operations the supervisor's state
//! machine sits on top of. Keeping them here leaves the state machine readable
//! and keeps each one testable without standing up a supervisor.
//! What: [`ChildHandle`] (the per-instance bookkeeping), [`spawn_child`],
//! [`terminate_child`], [`remove_socket_file`], and [`over_rss_limit`].
//! Test: `tests.rs` — `over_rss_limit_is_false_without_a_measurement`,
//! `over_rss_limit_is_false_when_disabled`, `terminate_child_reaps_a_live_child`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use super::{SpawnSpec, SupervisorError};

/// Per-instance child bookkeeping stored in the supervisor's map.
///
/// Why: keeping the `Child` alongside the socket path lets every reaping path
/// terminate the process and clean up its socket in one pass without
/// re-resolving anything.
/// What: the `tokio::process::Child`, the socket it was told to bind, and a
/// monotonic LRU stamp. The stamp is a counter rather than an `Instant` because
/// two `ensure_running` calls inside the same clock tick would compare equal and
/// make the victim choice arbitrary — which is exactly what a burst fan-out
/// produces.
/// Test: covered through every supervisor test that reaps.
#[derive(Debug)]
pub(super) struct ChildHandle {
    pub(super) child: Child,
    pub(super) socket_path: PathBuf,
    pub(super) last_used: u64,
}

/// Spawn one child from `spec`.
///
/// Why: isolates the `Command` builder so the supervisor's state machine can be
/// read without it, and so the stdio and `kill_on_drop` decisions live in one
/// place rather than at each service's call site.
/// What: creates `spec.create_dirs` first (failing here gives a cleaner error
/// than a child that immediately exits), closes stdin and stdout — a supervised
/// UDS service speaks its socket, not its pipes — inherits stderr so the child's
/// tracing output reaches the parent's log stream, and sets `kill_on_drop` so an
/// unsupervised drop reaps the child rather than leaking it.
///
/// `detached` inverts that last decision (#6350). An on-demand service ends on
/// its own idle window and is meant to be reused by the next client, so a
/// transient caller's exit must not take it down — see
/// [`super::SupervisorConfig::with_detached`].
/// Test: `spawn_child_creates_requested_directories`,
/// `spawn_child_reports_a_missing_binary`,
/// `detached_children_are_not_retained_in_the_population`.
pub(super) async fn spawn_child(
    service: &str,
    key: &str,
    spec: &SpawnSpec,
    detached: bool,
) -> Result<Child, SupervisorError> {
    for dir in &spec.create_dirs {
        if !dir.exists() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|source| SupervisorError::CreateDir {
                    service: service.to_string(),
                    key: key.to_string(),
                    path: dir.clone(),
                    source,
                })?;
        }
    }

    Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(!detached)
        .spawn()
        .map_err(|source| SupervisorError::Spawn {
            service: service.to_string(),
            key: key.to_string(),
            program: spec.program.clone(),
            source,
        })
}

/// Send SIGTERM, wait out the service's patience window, then SIGKILL.
///
/// Why: a clean SIGTERM is the only signal a child's own shutdown handler can
/// act on, and for a service that acks writes before flushing them, that
/// handler is the difference between durable and lost. The wait must exceed the
/// child's OWN budget with margin, not merely match it — signal delivery, the
/// flush, socket cleanup and exit all have to fit. [`super::ServiceTimeouts`]
/// enforces that relationship at construction.
/// What: `libc::kill(SIGTERM)` on unix (tokio's `Child` exposes no SIGTERM),
/// then `wait()` under `patience`, then tokio's `kill()` — which sends SIGKILL
/// and waits, so the process is definitely gone when it returns.
/// Test: `terminate_child_reaps_a_live_child`.
pub(super) async fn terminate_child(child: &mut Child, patience: Duration) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `libc::kill` is safe to call with any pid; the kernel returns
        // -1/EINVAL/ESRCH rather than misbehaving on bad input. The return value
        // is intentionally ignored — either the signal landed (the child exits)
        // or it did not (the SIGKILL below covers it).
        unsafe {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    match tokio::time::timeout(patience, child.wait()).await {
        Ok(Ok(status)) => {
            tracing::debug!(?status, "supervised child exited after SIGTERM");
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => {
            tracing::warn!("supervised child ignored SIGTERM after {patience:?} — sending SIGKILL");
            child.kill().await
        }
    }
}

/// Best-effort unlink of a child's socket file.
///
/// Why: a child that exits cleanly unlinks its own socket, but a SIGKILLed one
/// leaves the file behind and the next spawn for that instance then fails to
/// bind with EADDRINUSE — and a reaped instance is exactly the one most likely
/// to be spawned again shortly.
/// What: `remove_file`; `NotFound` is the expected clean-exit case and is not
/// logged. Any other error is logged at `debug!` and ignored — a stale socket
/// costs one failed spawn, not correctness.
/// Test: covered through the supervisor's shutdown and reap paths.
pub(super) async fn remove_socket_file(service: &str, key: &str, socket_path: &Path) {
    if let Err(e) = tokio::fs::remove_file(socket_path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(
            service = %service,
            instance = %key,
            socket = %socket_path.display(),
            "could not remove supervised child's socket (likely already cleaned up): {e}"
        );
    }
}

/// Decide whether a child has breached the RSS ceiling.
///
/// Why (#2846): the point of this limit is that it is compared against a real
/// measurement, so the two ways a measurement can be missing need an explicit,
/// tested answer rather than whatever `unwrap_or` happens to do. A child with no
/// pid has already exited — nothing to reclaim. A pid whose RSS cannot be read
/// yields `None`, which means "no reading", NOT "zero"; reaping on it would kill
/// every healthy child on any platform where the read is unavailable, turning a
/// memory guardrail into an outage.
/// What: `false` when enforcement is off, when the pid is gone, or when the
/// reading is unavailable. Otherwise `true` iff the measured MB is at or above
/// the ceiling. Pure — takes the pid rather than the handle so it is testable
/// without a live process.
/// Test: `over_rss_limit_is_false_without_a_measurement`,
/// `over_rss_limit_is_false_when_disabled`.
pub fn over_rss_limit(pid: Option<u32>, limit_mb: Option<u64>) -> bool {
    let (Some(limit), Some(pid)) = (limit_mb, pid) else {
        return false;
    };
    crate::sys_metrics::process_rss_mb(pid).is_some_and(|mb| mb >= limit)
}
