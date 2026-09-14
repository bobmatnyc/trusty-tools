//! Cross-process exclusive advisory lock around a whole-file critical section.
//!
//! Why: [`crate::json_rmw`] already owned this lock, but only for JSON
//! documents it also serialises and publishes itself. `trusty-search`'s
//! `indexes.toml` is a TOML registry with its own loader, its own
//! `skip_serializing_if` shape, and its own fail-closed parse contract, so it
//! cannot route through `json_rmw::update` — yet it has the identical failure
//! mode: several independent PROCESSES (the daemon, `trusty-search prune`,
//! `trusty-search prune-orphans`) each run load → mutate → save-the-whole-file,
//! and a write landing between another writer's load and its save is discarded
//! with both callers reporting success (#5344). Extracting the lock here means
//! there is still exactly ONE implementation of the critical section; `json_rmw`
//! now calls it rather than owning it.
//! What: [`with_exclusive_lock`] runs a closure while holding an exclusive
//! `flock(2)`-style advisory lock on a `<path>.lock` sidecar, releasing it by
//! RAII on every exit path including a panic. [`with_exclusive_lock_timeout`]
//! is the same entry point with the wait bound chosen by the caller, and
//! [`lock_path`] names that sidecar.
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! file_lock::tests`.
//!
//! # Contract
//!
//! - **Serialisation.** The lock is held by the open file description, so it
//!   serialises separate PROCESSES and separate threads that each call
//!   [`with_exclusive_lock`], on Unix and Windows alike.
//! - **Advisory, not mandatory.** A process that writes the guarded file
//!   without going through this entry point is not blocked. Every writer of a
//!   given file must use it.
//! - **Never fail open.** A lock that cannot be created or acquired — including
//!   one whose acquisition times out — is an `Err`; the closure never runs.
//!   Proceeding unlocked is the lost-update bug this module exists to remove.
//! - **Bounded, never indefinite — for local contention.** Acquisition retries
//!   until [`DEFAULT_LOCK_TIMEOUT`] (or the caller's own bound) and then fails
//!   with an [`std::io::ErrorKind::TimedOut`] error wrapping a [`LockTimeout`].
//!   A holder that is wedged rather than dead — SIGSTOP'd, stopped in a
//!   debugger — therefore costs the waiter a bounded delay and a diagnosable
//!   error instead of a silent hang (#7762). The bound is enforced BETWEEN
//!   `try_write` attempts, not inside one: a `$HOME` wedged on a stalled
//!   network filesystem can leave a single `flock(2)` call blocked past the
//!   timeout, and the deadline is never reached because the loop never gets
//!   control back.
//! - **Diagnostic pid in the sidecar.** The holder writes its pid into the
//!   sidecar after acquiring, which is the sidecar's only content and the only
//!   thing a waiter reads out of it. It is best-effort: it goes stale on
//!   release, and an empty or unparsable sidecar simply yields "holder pid
//!   unknown". It never gates acquisition and never turns a timeout into a
//!   success.
//! - **Not reentrant.** Nesting two [`with_exclusive_lock`] calls on the same
//!   path cannot succeed — the second acquisition uses a different descriptor,
//!   so it waits out its whole timeout and then errors.
//! - **Blocking.** Acquisition blocks the calling thread for up to the timeout.
//!   Async callers must run it on a blocking-safe thread (e.g.
//!   `tokio::task::spawn_blocking`).
//!
//! [`with_exclusive_lock`]: crate::file_lock::with_exclusive_lock
//! [`with_exclusive_lock_timeout`]: crate::file_lock::with_exclusive_lock_timeout
//! [`lock_path`]: crate::file_lock::lock_path
//! [`DEFAULT_LOCK_TIMEOUT`]: crate::file_lock::DEFAULT_LOCK_TIMEOUT
//! [`LockTimeout`]: crate::file_lock::LockTimeout

use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long [`with_exclusive_lock`] waits before giving up.
///
/// Why: the default has to suit the interactive path — every
/// `.claude/settings.json` writer behind `tm launch` goes through this lock
/// (#7762) — where a user watching a silent terminal is the failure. Ten
/// seconds is far longer than any real critical section here (a few hundred
/// kilobytes of load → mutate → save) and short enough to read as "something is
/// wrong" rather than as a hang.
/// What: the bound [`with_exclusive_lock`] passes to
/// [`with_exclusive_lock_timeout`]. A batch or daemon path that genuinely
/// wants to wait longer calls the latter directly.
/// Test: `with_exclusive_lock_default_path_is_unchanged_when_uncontended`.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Gap between acquisition attempts.
///
/// Short enough that an uncontended-after-a-moment lock is taken promptly,
/// long enough that a long wait is not a spin.
const ACQUIRE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The bounded wait expired without the lock.
///
/// Why: "could not acquire" must be actionable without a debugger. The two
/// facts a waiter can act on are WHICH lock it waited for and WHO held it, so
/// both travel in the error rather than in a log line the caller may not emit.
/// What: carried as the inner error of an [`std::io::Error`] with kind
/// [`std::io::ErrorKind::TimedOut`], so callers that already propagate
/// `io::Error` need no change and callers that want the fields recover them
/// with `err.get_ref().and_then(|e| e.downcast_ref::<LockTimeout>())`.
/// `holder_pid` is `None` when the sidecar is empty or unparsable — it is a
/// diagnostic, never a precondition.
/// Test: `with_exclusive_lock_timeout_errors_while_another_descriptor_holds_it`,
/// `with_exclusive_lock_timeout_reports_unknown_pid_for_a_garbage_sidecar`,
/// `with_exclusive_lock_timeout_names_the_pid_of_a_holding_process`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockTimeout {
    /// The sidecar that could not be locked — [`lock_path`] of the guarded file.
    pub lock_path: PathBuf,
    /// The pid the sidecar named, when it named a parsable one.
    pub holder_pid: Option<u32>,
    /// The bound that expired.
    pub waited: Duration,
}

impl std::fmt::Display for LockTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "timed out after {:?} waiting for the exclusive lock {}",
            self.waited,
            self.lock_path.display()
        )?;
        match self.holder_pid {
            Some(pid) => write!(f, " (holder pid {pid})"),
            None => write!(f, " (holder pid unknown)"),
        }
    }
}

impl std::error::Error for LockTimeout {}

/// Sidecar lock-file path for `path`.
///
/// Why: locking the guarded file itself would mean opening it for write before
/// we know whether the update will succeed, and the lock would be lost across
/// the `rename` that publishes a new version (the renamed-over inode, and any
/// lock on it, is discarded). A stable sidecar survives every publish.
/// What: appends `.lock` to the file name, keeping it in the same directory.
/// Test: `lock_path_is_a_sidecar`.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Run `f` while holding the exclusive cross-process lock guarding `path`.
///
/// Why: see the module docs — this is the one place the load → mutate → save
/// critical section is made safe against writers in other processes.
/// What: [`with_exclusive_lock_timeout`] at [`DEFAULT_LOCK_TIMEOUT`]. `f`'s own
/// return value — commonly a `Result` — passes through untouched; the `Err`
/// returned here is only ever a lock-acquisition failure, so a caller can never
/// confuse "could not lock" with "the work failed".
/// Test: `with_exclusive_lock_serialises_separate_descriptors`,
/// `with_exclusive_lock_releases_on_panic`, `with_exclusive_lock_unopenable_errors`,
/// `with_exclusive_lock_default_path_is_unchanged_when_uncontended`.
pub fn with_exclusive_lock<R>(path: &Path, f: impl FnOnce() -> R) -> std::io::Result<R> {
    with_exclusive_lock_timeout(path, DEFAULT_LOCK_TIMEOUT, f)
}

/// [`with_exclusive_lock`] with the caller's own wait bound.
///
/// Why: #7762 — the blocking acquisition this replaced hung `tm launch` with no
/// output whenever a holder was wedged rather than dead. A non-interactive
/// writer may legitimately want to wait longer than the interactive default, so
/// the bound is a parameter rather than a constant.
/// What: creates (if needed) and opens the [`lock_path`] sidecar, retries the
/// non-blocking exclusive acquisition every [`ACQUIRE_POLL_INTERVAL`] until
/// `timeout` elapses, records the acquiring pid in the sidecar, runs `f`, and
/// releases the lock by RAII. One attempt always happens, so a zero `timeout`
/// is a single try. On expiry the return is an [`std::io::ErrorKind::TimedOut`]
/// error wrapping [`LockTimeout`] and `f` is never run. The bound covers the
/// gap BETWEEN `try_write` calls, not the inside of one: on a wedged network
/// filesystem, a single `try_write` can block in `flock(2)` past `timeout`,
/// and this function does not observe the deadline until that call returns.
/// Test: `with_exclusive_lock_timeout_errors_while_another_descriptor_holds_it`,
/// `with_exclusive_lock_timeout_reports_unknown_pid_for_a_garbage_sidecar`,
/// `with_exclusive_lock_timeout_names_the_pid_of_a_holding_process`,
/// `with_exclusive_lock_records_the_acquiring_pid`.
pub fn with_exclusive_lock_timeout<R>(
    path: &Path,
    timeout: Duration,
    f: impl FnOnce() -> R,
) -> std::io::Result<R> {
    let lock = lock_path(path);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)?;
    let mut rw = fd_lock::RwLock::new(lock_file);
    // See #7762: bounded acquisition. Expiry is an error, never a bypass.
    let started = Instant::now();
    let mut guard = loop {
        match rw.try_write() {
            Ok(guard) => break guard,
            // Contended, or a signal landed mid-call: both are retryable.
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                LockTimeout {
                    holder_pid: read_holder_pid(&lock),
                    lock_path: lock,
                    waited: timeout,
                },
            ));
        }
        std::thread::sleep(ACQUIRE_POLL_INTERVAL.min(remaining));
    };
    record_holder_pid(&mut guard);
    Ok(f())
}

/// Stamp this process's pid into the held sidecar.
///
/// Why: a later waiter can only name the holder if the holder left its name;
/// `flock(2)` itself exposes no owner, and `F_GETLK` describes `fcntl` record
/// locks, not these.
/// What: truncate-and-rewrite under the lock we already hold, so the pid is the
/// sidecar's whole content. Failures are swallowed: the pid is diagnostic, and
/// an unwritable sidecar must not undo an acquisition that succeeded. A
/// `set_len(0)` that succeeds followed by a rewind or write that fails leaves
/// the sidecar empty rather than restoring its prior content; a later
/// [`read_holder_pid`] then reports "holder pid unknown" exactly as it would
/// for a sidecar that was never written.
/// Test: `with_exclusive_lock_records_the_acquiring_pid`,
/// `with_exclusive_lock_acquires_over_a_garbage_sidecar`.
fn record_holder_pid(lock_file: &mut File) {
    if lock_file.set_len(0).is_err() || lock_file.rewind().is_err() {
        return;
    }
    let _ = lock_file.write_all(std::process::id().to_string().as_bytes());
}

/// The pid the sidecar names, if it names one.
///
/// Why: read on the failure path only, where any answer is better than none and
/// no answer must still be an error.
/// What: parses the sidecar's whole trimmed content as a pid. Missing, empty,
/// non-UTF-8 and unparsable content all read as `None`. This read is not
/// synchronised with [`record_holder_pid`]'s truncate-then-write: a waiter can
/// land in the gap between them, so `None` here also covers a live holder
/// caught mid-write, not only an empty or garbage sidecar.
/// Test: `with_exclusive_lock_timeout_reports_unknown_pid_for_a_garbage_sidecar`.
fn read_holder_pid(lock: &Path) -> Option<u32> {
    std::fs::read_to_string(lock).ok()?.trim().parse().ok()
}

#[cfg(test)]
#[path = "file_lock_tests.rs"]
mod tests;
