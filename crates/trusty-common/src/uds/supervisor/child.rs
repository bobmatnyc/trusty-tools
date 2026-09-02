//! Child-process lifecycle for [`super::UdsServiceSupervisor`] (#5089).
//!
//! Why: spawn, graceful termination, socket-file cleanup and the RSS
//! measurement are the four mechanical operations the supervisor's state
//! machine sits on top of. Keeping them here leaves the state machine readable
//! and keeps each one testable without standing up a supervisor.
//! What: [`ChildHandle`] (the per-instance bookkeeping), [`spawn_child`],
//! [`terminate_child`], [`remove_socket_file`], and [`over_rss_limit`].
//! Test: `tests.rs` — `over_rss_limit_is_false_without_a_measurement`,
//! `over_rss_limit_is_false_when_disabled`, `terminate_child_reaps_a_live_child`,
//! `a_child_that_exits_before_binding_reports_its_status_and_stderr`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};

use crate::log_buffer::LogBuffer;

use super::{SpawnSpec, SupervisorError};

/// Stderr lines retained per child for a spawn-failure report (#6600).
///
/// Twenty covers a panic message with its header, or the handful of lines a
/// service prints before it gives up on a lock, while a supervisor holding
/// several children still costs kilobytes.
pub(super) const STDERR_TAIL_LINES: usize = 20;

/// How long [`SpawnedChild::stderr_tail`] waits for the relay to finish reading
/// a dead child's pipe (#6600).
///
/// The write end is already closed by the time it is called, so the relay
/// reaches EOF as soon as it has drained what the kernel buffered — microseconds
/// in practice. The bound exists so an unreadable pipe cannot stall a path whose
/// whole point is to fail fast.
const STDERR_DRAIN: Duration = Duration::from_millis(200);

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

/// A freshly-launched child, plus the tail of its stderr when one was captured.
///
/// Why (#6600): `ensure_running` now reports a child that died before it bound,
/// and an exit status alone rarely says which precondition failed — "exit
/// status: 1" and "Database already open. Cannot acquire lock." send an
/// operator to different places. Carrying the buffer beside the handle is what
/// lets the error quote the second.
/// What: the `tokio::process::Child`, and `Some(buffer)` when this supervisor
/// piped the child's stderr. `None` means stderr was inherited — see
/// [`spawn_child`] for which children get which.
/// Test: `a_child_that_exits_before_binding_reports_its_status_and_stderr`.
#[derive(Debug)]
pub(super) struct SpawnedChild {
    pub(super) child: Child,
    stderr: Option<LogBuffer>,
    relay: Option<tokio::task::JoinHandle<()>>,
}

impl SpawnedChild {
    /// The last [`STDERR_TAIL_LINES`] the child wrote; empty when stderr was
    /// inherited rather than captured.
    ///
    /// 🔴 **Call this only once the child has EXITED.** The relay task is still
    /// reading when the process dies, so the lines that explain the death may
    /// not have reached the buffer yet — this waits for the relay to finish,
    /// which only happens when the write end of the pipe is closed. On a live
    /// child that wait would run for the child's whole lifetime.
    ///
    /// The wait is bounded by [`STDERR_DRAIN`] regardless, so a pipe that never
    /// reaches EOF cannot hold the failure path open.
    /// Test: `a_child_that_exits_before_binding_reports_its_status_and_stderr`.
    pub(super) async fn stderr_tail(&mut self) -> Vec<String> {
        let Some(buffer) = self.stderr.as_ref() else {
            return Vec::new();
        };
        if let Some(relay) = self.relay.take() {
            let _ = tokio::time::timeout(STDERR_DRAIN, relay).await;
        }
        buffer.tail(STDERR_TAIL_LINES)
    }
}

/// Spawn one child from `spec`.
///
/// Why: isolates the `Command` builder so the supervisor's state machine can be
/// read without it, and so the stdio and `kill_on_drop` decisions live in one
/// place rather than at each service's call site.
/// What: creates `spec.create_dirs` first (failing here gives a cleaner error
/// than a child that immediately exits), closes stdin and stdout — a supervised
/// UDS service speaks its socket, not its pipes — routes the child's stderr to
/// this process's stderr, and sets `kill_on_drop` so an unsupervised drop reaps
/// the child rather than leaking it.
///
/// `detached` inverts that last decision (#6350). An on-demand service ends on
/// its own idle window and is meant to be reused by the next client, so a
/// transient caller's exit must not take it down — see
/// [`super::SupervisorConfig::with_detached`].
///
/// 🔴 **`detached` also decides whether stderr is captured (#6600), and the two
/// arms are not interchangeable.** A supervised child's stderr is PIPED and
/// copied through by [`relay_stderr`], so its tail is quotable when the child
/// dies before binding. A DETACHED child keeps `Stdio::inherit()`: it is meant
/// to outlive the process that spawned it, and a pipe whose read end left with
/// that process turns the child's next log write into `EPIPE` — which
/// `eprintln!` turns into a panic. Capturing a detached child's stderr would
/// kill the server detached mode exists to keep alive.
///
/// Test: `spawn_child_creates_requested_directories`,
/// `spawn_child_reports_a_missing_binary`,
/// `detached_children_are_not_retained_in_the_population`,
/// `a_child_that_exits_before_binding_reports_its_status_and_stderr`.
pub(super) async fn spawn_child(
    service: &str,
    key: &str,
    spec: &SpawnSpec,
    detached: bool,
) -> Result<SpawnedChild, SupervisorError> {
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

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // #6600: piped for a supervised child so its stderr is quotable,
        // inherited for a detached one so it survives this process.
        .stderr(if detached {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .kill_on_drop(!detached);

    let mut child = command.spawn().map_err(|source| SupervisorError::Spawn {
        service: service.to_string(),
        key: key.to_string(),
        program: spec.program.clone(),
        source,
    })?;

    let relayed = child.stderr.take().map(relay_stderr);
    let (stderr, relay) = match relayed {
        Some((buffer, handle)) => (Some(buffer), Some(handle)),
        None => (None, None),
    };
    Ok(SpawnedChild {
        child,
        stderr,
        relay,
    })
}

/// Copy a captured child's stderr to this process's stderr, keeping the tail.
///
/// Why: piping stderr for the sake of [`SpawnedChild::stderr_tail`] must not
/// cost the operator the child's log stream, which `Stdio::inherit()` gave them
/// for free. The relay passes each line through UNCHANGED — not through
/// `tracing` — so the child's own formatting and level survive, and no line
/// disappears because this process's `RUST_LOG` filters it out.
/// What: a task that reads lines until the pipe closes (the child exited, or
/// was reaped), writing each to `tokio::io::stderr()` and pushing it onto a
/// bounded [`LogBuffer`]. The returned buffer is a cheap handle onto the same
/// ring, so a caller reads the tail without stopping the relay; the returned
/// join handle is how [`SpawnedChild::stderr_tail`] knows the pipe has been
/// drained. Write failures are ignored: losing a log line must never take down
/// a supervisor.
/// Test: `a_child_that_exits_before_binding_reports_its_status_and_stderr`.
fn relay_stderr(pipe: ChildStderr) -> (LogBuffer, tokio::task::JoinHandle<()>) {
    let buffer = LogBuffer::new(STDERR_TAIL_LINES);
    let sink = buffer.clone();
    let handle = tokio::spawn(async move {
        let mut out = tokio::io::stderr();
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = out.write_all(line.as_bytes()).await;
            let _ = out.write_all(b"\n").await;
            let _ = out.flush().await;
            sink.push(line);
        }
    });
    (buffer, handle)
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
