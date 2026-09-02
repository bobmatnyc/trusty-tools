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

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};

use crate::log_buffer::LogBuffer;

use super::{SpawnSpec, SupervisorError};

/// Stderr lines retained per child for a spawn-failure report (#6600).
///
/// Twenty covers a panic message with its header, or the handful of lines a
/// service prints before it gives up on a lock, while a supervisor holding
/// several children still costs kilobytes.
pub(super) const STDERR_TAIL_LINES: usize = 20;

/// Largest number of bytes the relay buffers for ONE stderr line (#6600 review).
///
/// Why: [`STDERR_TAIL_LINES`] bounds how MANY lines are retained and says
/// nothing about how long one may be. A child that writes a megabyte before its
/// first newline — a panic with a huge `Debug` payload, a serialised request
/// echoed into a log — would otherwise be buffered whole by the reader and then
/// retained whole by the ring, and twenty such lines per child is the supervisor
/// carrying tens of megabytes for as long as the child lives. The bound is on
/// the READ, not on a truncation after the fact, so the oversized line is never
/// resident in the first place.
///
/// What: 8 KiB. A line longer than this is relayed to stderr in 8 KiB pieces and
/// retained as those pieces — the operator loses no bytes, and the ring's
/// worst case becomes `STDERR_TAIL_LINES * STDERR_LINE_CAP`, about 160 KiB.
/// Test: `a_child_writing_an_enormous_line_does_not_grow_the_relay_buffer`.
pub(super) const STDERR_LINE_CAP: u64 = 8 * 1024;

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
///
/// Three details the obvious `lines()` loop gets wrong (#6600 review):
///   - each read is bounded by [`STDERR_LINE_CAP`], so a child that writes
///     without ever emitting a newline cannot grow this task's buffer;
///   - the line and its terminator go out in ONE `write_all`, because two
///     writes can interleave with another child's relay and split a line;
///   - invalid UTF-8 is relayed lossily rather than ending the relay. `lines()`
///     yields `Err` there, and the old `while let Ok(Some(_))` treated that as
///     EOF — one bad byte and the operator silently lost the rest of the stream.
///
/// 🔴 **One LOGICAL line takes one ring slot, however long it is (#6601
/// review).** Bounding the READ is not by itself enough: a 1 MiB line arrives as
/// 128 capped reads, and pushing each one retained a single line as 20 entries
/// and evicted every real line before it. Measured on the pre-fix code, a child
/// printing `Database already open. Cannot acquire lock.` and then 1 MiB of
/// padding left a ring of `[8192 × 18, 0, 5]` — nothing but padding, on the
/// exact failure #6600 exists to diagnose, and one empty entry from a body that
/// ended level with the cap. So an over-cap line keeps its capped prefix, and
/// [`drain_over_cap_line`] reads to the next newline WITHOUT retaining any of
/// it. The prefix carries a marker naming how many bytes were dropped, and
/// prefix plus marker still fit [`STDERR_LINE_CAP`], so the ring's worst case
/// stays `STDERR_TAIL_LINES * STDERR_LINE_CAP`.
///
/// Every byte still reaches this process's stderr — the drain relays what it
/// discards. Only the RETAINED copy is truncated. The capped prefix is written
/// without a synthesised terminator for the same reason: inserting one would
/// split the operator's view of a line the child wrote whole. A terminator is
/// supplied only at EOF, where the child left the stream mid-line.
///
/// A read error DOES end the relay, with a `debug!`: the pipe is gone, and a
/// bare `continue` on a persistent error would spin.
/// Test: `a_child_that_exits_before_binding_reports_its_status_and_stderr`,
/// `a_child_writing_an_enormous_line_does_not_grow_the_relay_buffer`,
/// `a_real_line_survives_the_over_cap_line_that_follows_it`.
fn relay_stderr(pipe: ChildStderr) -> (LogBuffer, tokio::task::JoinHandle<()>) {
    relay_stderr_into(pipe, tokio::io::stderr())
}

/// [`relay_stderr`] with the pass-through destination supplied by the caller.
///
/// Why the seam exists: the buffer bound is proven with a 1 MiB line, and
/// `relay_stderr` would faithfully copy that megabyte to the test runner's own
/// stderr — a megabyte in every CI log, on a property that has nothing to do
/// with where the bytes go. The test passes `tokio::io::sink()`; production has
/// exactly one caller and it passes `tokio::io::stderr()`.
/// Test: `a_child_writing_an_enormous_line_does_not_grow_the_relay_buffer`.
pub(super) fn relay_stderr_into<W>(
    pipe: ChildStderr,
    mut out: W,
) -> (LogBuffer, tokio::task::JoinHandle<()>)
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let buffer = LogBuffer::new(STDERR_TAIL_LINES);
    let sink = buffer.clone();
    let handle = tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut raw: Vec<u8> = Vec::new();
        loop {
            raw.clear();
            let read = match (&mut reader)
                .take(STDERR_LINE_CAP)
                .read_until(b'\n', &mut raw)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!(error = %e, "supervised child stderr relay ended on a read error");
                    break;
                }
            };
            if read == 0 {
                break;
            }

            if raw.ends_with(b"\n") {
                // One write, terminator included — see the doc comment.
                let _ = out.write_all(&raw).await;
                sink.push(decode_line(&raw[..raw.len() - 1]));
                continue;
            }

            if read as u64 != STDERR_LINE_CAP {
                // The pipe ended mid-line. Nothing was dropped; supply the
                // terminator the child never wrote so stderr is not left
                // hanging.
                raw.push(b'\n');
                let _ = out.write_all(&raw).await;
                sink.push(decode_line(&raw[..raw.len() - 1]));
                continue;
            }

            // Over the cap. Relay the prefix as-is, then relay-and-discard the
            // rest so this one logical line takes one ring slot (#6601 review).
            let _ = out.write_all(&raw).await;
            let dropped = drain_over_cap_line(&mut reader, &mut out).await;
            sink.push(if dropped == 0 {
                decode_line(&raw)
            } else {
                truncated_entry(&raw, dropped)
            });
        }
        // Flushed once at EOF rather than per line: `tokio::io::stderr` writes
        // through, so a flush inside the loop bought nothing per line.
        let _ = out.flush().await;
    });
    (buffer, handle)
}

/// Relay the remainder of an over-cap line to `out` without retaining it.
///
/// Why: the retained copy of a line is capped, but the operator's copy must not
/// be — losing the second half of a panic message is the outcome #6600 was filed
/// against. This reads on, writing every byte through, and reports only how much
/// it threw away.
/// What: capped reads until a newline or EOF. Returns the count of
/// NON-terminator bytes discarded, which is `0` when the line happened to end
/// exactly at the cap — that case is not a truncation and earns no marker.
/// Test: `a_real_line_survives_the_over_cap_line_that_follows_it`,
/// `a_child_writing_an_enormous_line_does_not_grow_the_relay_buffer`.
async fn drain_over_cap_line<W>(reader: &mut BufReader<ChildStderr>, out: &mut W) -> u64
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut discarded: u64 = 0;
    let mut chunk: Vec<u8> = Vec::new();
    loop {
        chunk.clear();
        let read = match (&mut *reader)
            .take(STDERR_LINE_CAP)
            .read_until(b'\n', &mut chunk)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(error = %e, "supervised child stderr relay ended draining a long line");
                break;
            }
        };
        if read == 0 {
            break;
        }
        let _ = out.write_all(&chunk).await;
        if chunk.ends_with(b"\n") {
            discarded += read as u64 - 1;
            break;
        }
        discarded += read as u64;
    }
    discarded
}

/// Decode one retained stderr line, tolerating a codepoint split by the cap.
///
/// Why: [`STDERR_LINE_CAP`] is a BYTE bound, so it can land inside a multi-byte
/// character. `from_utf8_lossy` alone turns that tail into a U+FFFD that reads
/// like corrupt child output; the bytes are in fact intact and simply continue
/// past the cap.
/// What: an incomplete TRAILING sequence is dropped; genuinely invalid bytes
/// anywhere else still become U+FFFD, because the relay must never end on one.
/// Test: `a_child_writing_an_enormous_line_does_not_grow_the_relay_buffer`.
fn decode_line(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(e) if e.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The ring entry standing in for an over-cap line.
///
/// Why: a silently shortened line invites the reader to believe they have the
/// whole message. Naming the dropped byte count is what tells an operator to go
/// look at this process's stderr for the rest.
/// What: the prefix, trimmed so prefix-plus-marker still fits
/// [`STDERR_LINE_CAP`] — which is what keeps the ring's worst case at
/// `STDERR_TAIL_LINES * STDERR_LINE_CAP`.
/// Test: `a_real_line_survives_the_over_cap_line_that_follows_it`.
fn truncated_entry(prefix: &[u8], dropped: u64) -> String {
    let marker = format!("…[+{dropped} bytes truncated]");
    let room = (STDERR_LINE_CAP as usize).saturating_sub(marker.len());
    let mut kept = decode_line(&prefix[..prefix.len().min(room)]);
    kept.push_str(&marker);
    kept
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
