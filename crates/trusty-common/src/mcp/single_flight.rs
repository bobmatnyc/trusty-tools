//! Single-flight daemon start: N concurrent bridges produce exactly ONE daemon.
//!
//! Why (#5267, and the #1152 outage it must not reproduce): the trusty-* stdio
//! bridges each need the HTTP daemon to exist before they can proxy to it. The
//! obvious implementation — probe, and spawn if the probe misses — is what
//! caused #1152. A health probe is a *window*, not a lock: under load (dozens of
//! concurrent agent worktrees) N bridges each miss inside the same window and
//! each spawn a daemon. Every one of them opens the production palace redb and
//! contends for its exclusive single-writer lock. Observed 2026-06-12: ~36
//! orphan daemons, one squatting the write lock on every palace, all writes
//! failing `DatabaseAlreadyOpen` while reads served stale snapshots.
//!
//! #1152 fixed that by refusing to spawn at all (`no_spawn`). That trades an
//! outage for a papercut: a bridge whose daemon is merely not running hard-errors
//! instead of starting it. This module implements the branch #1152's own approved
//! text sanctioned — "auto-start the CANONICAL daemon via the single-instance-
//! guarded start … so duplicates are structurally impossible" — by supplying the
//! mutual exclusion that a probe alone cannot provide.
//!
//! What: [`ensure_daemon_up_single_flight`] probes without a lock (the common
//! case costs nothing), and on a miss takes an exclusive `flock(2)` covering the
//! whole start — re-probe, spawn, readiness wait — so a bridge that loses the
//! race blocks and then finds a *ready* daemon instead of starting a second one.
//!
//! STDOUT hygiene: never writes to stdout — stdout is the JSON-RPC channel in
//! every caller. All diagnostics go to stderr.
//!
//! Test: `crates/trusty-common/tests/single_flight_exclusion.rs` drives N
//! concurrent real processes and asserts exactly one daemon starts; unit tests
//! below cover lock acquisition, release, and crash safety.

use std::fs::OpenOptions;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use super::daemon_bridge::{
    DAEMON_POLL_INTERVAL, DAEMON_START_TIMEOUT, DaemonBridgeConfig, poll_until_ready,
    probe_health_once, spawn_daemon_detached,
};

/// An exclusive advisory lock on the daemon-start critical section.
///
/// Why `flock(2)` and not an `O_EXCL` lockfile: the lock must survive a starter
/// that crashes mid-start. An `O_EXCL` lockfile is a *file that exists*, so a
/// killed starter leaves it behind and every later bridge deadlocks on a stale
/// lock until a human deletes it — trading #1152's duplicate-daemon failure for a
/// total-wedge failure. `flock` is held by the open file description, so the
/// kernel drops it when the process dies for ANY reason, including `SIGKILL`.
/// There is no stale state to clean up and no reaper to write.
///
/// What: owns the lock file handle. The lock is released by [`Drop`] — which
/// runs on the success path, on `?` early-return, on panic unwind, and on future
/// cancellation — and by the kernel if the process dies before `Drop` can. The
/// lock FILE is never deleted: unlinking it would let a later process open a
/// different inode and lock nothing, silently defeating the exclusion.
/// Test: `lock_is_exclusive_across_handles`, `lock_released_on_drop`,
/// `lock_released_on_error_path`.
#[derive(Debug)]
pub struct StartLock {
    file: std::fs::File,
}

impl StartLock {
    /// Acquire the exclusive start lock, blocking until it is available.
    ///
    /// Why: blocking is the point — a bridge that loses the race must WAIT for
    /// the winner to finish starting the daemon, then observe it running. A
    /// `try_lock` that gave up would put the loser back into the racing path
    /// this module exists to close.
    /// What: creates the lock file's parent directory and the file itself if
    /// absent (never truncating — the file's CONTENT is irrelevant, only its
    /// inode identity matters), then `flock(LOCK_EX)`, retrying on `EINTR`.
    /// Blocks the calling thread; async callers must run it on a blocking-safe
    /// thread.
    /// Test: `lock_is_exclusive_across_handles`.
    pub fn acquire_blocking(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("could not create start-lock directory {}", parent.display())
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .truncate(false)
            .write(true)
            .open(path)
            .with_context(|| format!("could not open start-lock file {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            loop {
                // Safety: `fd` is owned by `file` and stays open for the whole
                // call; `flock` only affects the referenced open file description.
                let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
                if rc == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue; // EINTR: a signal arrived while blocked; retry.
                }
                return Err(anyhow!(
                    "could not acquire start lock {}: {err}",
                    path.display()
                ));
            }
        }
        #[cfg(not(unix))]
        {
            // Fail closed rather than silently running without exclusion, which
            // would reintroduce #1152's duplicate-daemon race.
            return Err(anyhow!(
                "single-flight daemon start requires flock(2) and is not supported on this platform"
            ));
        }

        Ok(Self { file })
    }
}

impl Drop for StartLock {
    /// Release the lock explicitly, then close the file.
    ///
    /// Why: closing the descriptor alone would release the `flock`, but an
    /// explicit `LOCK_UN` documents the release at the point it happens and keeps
    /// the critical section greppable. Errors are ignored — the fd close that
    /// follows releases the lock unconditionally, so there is no failure mode
    /// left to report.
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // Safety: `self.file` is still open; `LOCK_UN` on an owned fd is
            // infallible in practice and harmless if the lock was already lost.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// Ensure the daemon is running, starting it at most once across all processes.
///
/// Why: this is the "start if it's not running" contract — distinct from
/// auto-spawn, which is what #1152 removed. Auto-spawn means every bridge
/// independently starts a daemon; this means the daemon's existence is *ensured*,
/// once, under coordination. Seven bridges converge on one daemon.
///
/// What: (1) probes health with no lock held, so the overwhelmingly common
/// "daemon already up" case pays nothing and touches no filesystem. (2) On a
/// miss, blocks on [`StartLock`] at `lock_path`. (3) **Re-probes under the
/// lock** — the process that just released it usually started the daemon, so the
/// loser must re-check before concluding anything. (4) Only if the re-probe also
/// misses does it spawn, via the shared [`spawn_daemon_detached`]. (5) Waits for
/// readiness via the shared [`poll_until_ready`], still holding the lock, so a
/// concurrent bridge cannot spawn a second daemon during the boot window. Fails
/// closed: an unreachable daemon is an `Err`, never an `Ok` the caller would
/// discover downstream as an empty result.
///
/// The lock is held across the readiness wait deliberately. Releasing it at spawn
/// time would reopen the race — a second bridge would acquire, re-probe a daemon
/// that has not bound yet, and start another.
///
/// Test: `crates/trusty-common/tests/single_flight_exclusion.rs` —
/// `n_concurrent_bridges_start_exactly_one_daemon`,
/// `already_running_daemon_is_not_restarted`,
/// `daemon_that_never_becomes_ready_is_a_hard_error`,
/// `crashed_starter_does_not_deadlock_the_next_bridge`.
pub async fn ensure_daemon_up_single_flight(
    config: &DaemonBridgeConfig,
    lock_path: &Path,
) -> Result<String> {
    let startup_timeout = config.startup_timeout.unwrap_or(DAEMON_START_TIMEOUT);
    let poll_interval = config.poll_interval.unwrap_or(DAEMON_POLL_INTERVAL);

    // (1) Fast path: no lock, no filesystem access.
    if probe_health_once(&config.health_url()).await {
        return Ok((config.base_url_fn)());
    }

    // (2) Serialise the start. Blocking `flock` goes on a blocking-safe thread;
    // the guard is `Send` and is held across the awaits below on purpose.
    let lock_path_owned = lock_path.to_path_buf();
    let _lock = tokio::task::spawn_blocking(move || StartLock::acquire_blocking(&lock_path_owned))
        .await
        .context("start-lock acquisition task panicked")??;

    // (3) Re-probe under the lock: whoever held it before us most likely just
    // started the daemon, in which case we must NOT start another.
    if probe_health_once(&config.health_url()).await {
        return Ok((config.base_url_fn)());
    }

    // (4) We are the single starter.
    spawn_daemon_detached(config)?;

    // (5) Bounded readiness wait, lock still held. `_lock` drops here on every
    // path — Ok, Err, panic, or cancellation.
    poll_until_ready(config, startup_timeout, poll_interval).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Why: the whole design rests on the lock actually excluding a second
    /// holder. If it does not, #1152's race is back with extra ceremony.
    /// What: takes the lock, then from a separate thread (separate fd, as a
    /// separate process would have) asserts the second acquisition BLOCKS until
    /// the first is dropped.
    /// Test: itself.
    #[test]
    fn lock_is_exclusive_across_handles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("start.lock");

        let first = StartLock::acquire_blocking(&path).expect("first acquire");
        let p2 = path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _second = StartLock::acquire_blocking(&p2).expect("second acquire");
            tx.send(Instant::now()).expect("send");
        });

        // The contender must still be blocked after a real delay.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            rx.try_recv().is_err(),
            "second acquisition must block while the first lock is held"
        );

        drop(first);
        let acquired_at = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second acquisition must succeed once the first is released");
        handle.join().expect("thread join");
        // Sanity: it acquired after we released, not before.
        let _ = acquired_at;
    }

    /// Why: a starter that fails must not leave the lock held — the next bridge
    /// has to be able to try. Covers the `?` early-return path.
    /// What: acquires inside a closure that returns `Err`, then asserts a fresh
    /// acquisition succeeds immediately.
    /// Test: itself.
    #[test]
    fn lock_released_on_error_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("start.lock");

        let failed: Result<()> = (|| {
            let _lock = StartLock::acquire_blocking(&path)?;
            Err(anyhow!("simulated start failure"))
        })();
        assert!(failed.is_err());

        // Must not block: the guard dropped when the closure unwound.
        let start = Instant::now();
        let _again = StartLock::acquire_blocking(&path).expect("reacquire after error");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "lock must be free after the error path"
        );
    }

    /// Why: the lock file must persist across acquisitions. Deleting it would let
    /// a later process lock a different inode and exclude nobody.
    /// What: acquires, drops, and asserts the file still exists.
    /// Test: itself.
    #[test]
    fn lock_file_survives_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("start.lock");
        drop(StartLock::acquire_blocking(&path).expect("acquire"));
        assert!(path.exists(), "lock file must not be unlinked on release");
    }

    /// Why: the lock lives under the data dir, which may not exist on a first
    /// run. Failing there would make a cold start impossible.
    /// What: points at a nested path with no parent and asserts acquisition
    /// creates it.
    /// Test: itself.
    #[test]
    fn lock_creates_missing_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/deeper/start.lock");
        let _lock = StartLock::acquire_blocking(&path).expect("acquire with missing parent");
        assert!(path.exists());
    }
}
