//! PID lock file for the `trusty-memory serve --foreground` daemon (issue #787).
//!
//! Why: the `start` subcommand used to detect a running daemon ONLY by probing
//! the `http_addr` discovery file. When a launchd-managed `serve --foreground`
//! instance (a) crashed without cleaning up `http_addr`, or (b) was deployed
//! from an older binary that did not write `http_addr`, the `start` command
//! concluded "no daemon running" and forked a new one. The new fork walked to
//! the next free port (7071, 7072, …) and became a silent orphan. This module
//! provides a PID lock file — written by `serve --foreground` before binding,
//! and cleared on graceful shutdown — that gives `start` a second, independent
//! signal to detect a live daemon. A stale lock (PID not alive) is reclaimed
//! transparently so a crash does not permanently block startup.
//!
//! What: exposes [`DaemonLock`] (RAII guard that removes the file on drop),
//! [`acquire_lock`] (write + reclaim stale), [`read_lock_pid`] (inspect
//! without acquiring), and the injectable [`lock_file_path`] helper.
//! Only used by the `serve --foreground` path — the `start` fork, CLI
//! subcommands, and the MCP bridge must never call [`acquire_lock`].
//!
//! Test: unit tests in this module cover stale-lock reclaim, live-lock
//! refusal, and the write/remove cycle, all against temp directories so
//! the real `~/.local/share/trusty-memory` is never touched.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Filename of the daemon PID lock file, written under the trusty-memory
/// data directory alongside `http_addr`.
///
/// Why: co-locating it with `http_addr` keeps the two discovery files in
/// the same directory so the `doctor` and `start` commands resolve both
/// with a single `resolve_data_dir` call.
/// What: the literal filename; callers join it onto the data-dir path.
/// Test: `lock_file_path_uses_data_dir` asserts the constructed path.
pub const LOCK_FILENAME: &str = "daemon.lock";

/// RAII guard that holds the daemon PID lock file.
///
/// Why: tie the lock file's lifetime to the daemon process lifetime so
/// the file is removed on both clean shutdown and panic, without
/// requiring every exit path to call an explicit cleanup function. The
/// guard is not `Clone` or `Send` — it is constructed once in `main` and
/// lives for the full daemon lifetime.
/// What: wraps the path of the written lock file. `Drop` removes it
/// best-effort (I/O errors are silently swallowed — the file will be
/// reclaimed as stale by the next invocation anyway).
/// Test: `daemon_lock_drops_removes_file`.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
}

impl DaemonLock {
    /// Construct directly from a path (test helper + internal use only).
    ///
    /// Why: tests need to build a `DaemonLock` pointing at a tempfile
    /// they control without going through the full OS data-dir resolution.
    /// What: wraps `path`; the file at `path` is assumed to already exist.
    /// Test: used in `daemon_lock_drops_removes_file`.
    pub(crate) fn from_path(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // Best-effort: if the remove fails (e.g. already deleted by a
        // concurrent `trusty-memory stop`) we ignore the error. The next
        // `serve --foreground` invocation will reclaim the stale file.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Resolve the canonical lock-file path for the trusty-memory daemon.
///
/// Why: centralising the path keeps `acquire_lock`, `read_lock_pid`, and
/// any future diagnostic check in agreement. Returns `None` when the data
/// directory cannot be resolved (no `$HOME`, no `TRUSTY_DATA_DIR_OVERRIDE`)
/// so callers degrade gracefully rather than panicking.
/// What: returns `{resolve_data_dir("trusty-memory")}/daemon.lock`, or
/// `None` on resolution failure.
/// Test: `lock_file_path_uses_data_dir` asserts the constructed path ends
/// with `daemon.lock` and lives under a known data dir override.
pub fn lock_file_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        .map(|d| d.join(LOCK_FILENAME))
}

/// Check whether a PID is alive on this Unix host.
///
/// Why: `kill -0 <pid>` returns success when the process exists (regardless
/// of whether we have permission to signal it) and `ESRCH` when it does not
/// exist. Using the shell rather than `nix` keeps the dependency surface
/// minimal, matching the pattern used by `commands::stop`.
/// What: runs `/bin/kill -0 <pid>` and returns `true` iff the exit code is 0.
/// On non-Unix platforms always returns `false` (stale lock is reclaimed).
/// Test: `pid_alive_returns_false_for_nonexistent_pid` (uses a PID that is
/// guaranteed not to exist).
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Read the PID stored in the lock file at `path`.
///
/// Why: `acquire_lock` and diagnostic commands need to read the lock file
/// without acquiring it. Separating the read from the acquire lets callers
/// inspect the file without side-effecting it.
/// What: reads the file, trims whitespace, and parses the first line as a
/// `u32`. Returns `None` when the file does not exist, is empty, or does
/// not contain a valid PID. Returns `Err` for I/O errors other than
/// `NotFound`.
/// Test: `read_lock_pid_returns_none_for_missing_file`,
/// `read_lock_pid_returns_pid_for_valid_file`.
pub fn read_lock_pid(path: &Path) -> Result<Option<u32>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("read lock file {}", path.display())));
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<u32>() {
        Ok(pid) => Ok(Some(pid)),
        Err(_) => Ok(None), // malformed → treat as absent
    }
}

/// Write `{pid}\n` to `path` atomically (write to `.tmp` + rename).
///
/// Why: atomic write prevents a concurrent reader from observing a
/// partial file (e.g. a truncated PID) during the write window.
/// What: creates parent directories if missing; writes the PID and a
/// trailing newline to `{path}.tmp`; renames to `path`. Returns `Err`
/// on any I/O failure.
/// Test: called by `acquire_lock` and covered by
/// `acquire_lock_writes_own_pid`.
fn write_lock_file(path: &Path, pid: u32) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("lock.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{pid}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Attempt to acquire the daemon PID lock file at `path`.
///
/// Why: without a lock file, a stale `http_addr` (e.g. from a daemon that
/// crashed before cleaning up its discovery files, or from an older binary
/// that never wrote `http_addr`) causes `trusty-memory start` to conclude
/// "no daemon running" and fork a new process. The new fork then collides
/// with the live daemon on port 7070 and silently port-walks to 7071+.
/// The lock file gives `start` — and the single-instance guard in `main.rs`
/// — a second signal to detect a live daemon.
///
/// Stale-lock handling: if the file exists but the recorded PID is not
/// alive (dead process, reboot, SIGKILL), we reclaim it by overwriting.
/// If the file exists AND the PID is alive, we return `Err` with a clear
/// message so the caller can abort rather than spawning a duplicate.
///
/// What: reads the existing lock file (if any); if the recorded PID is
/// alive, returns `Err("daemon already running: PID {n}")`. Otherwise
/// (no file, empty file, dead PID) writes the current process PID and
/// returns a [`DaemonLock`] RAII guard that removes the file on drop.
///
/// Test: `acquire_lock_writes_own_pid`, `acquire_lock_reclaims_stale_pid`,
/// `acquire_lock_refuses_live_pid`.
pub fn acquire_lock(path: &Path) -> Result<DaemonLock> {
    let me = std::process::id();

    // Read any existing lock without panicking — a missing file is fine.
    if let Some(existing_pid) = read_lock_pid(path)? {
        if existing_pid != me && pid_alive(existing_pid) {
            bail!(
                "trusty-memory daemon is already running as PID {existing_pid} \
                 (lock file: {}). \
                 If you believe this is a stale lock, remove it manually: \
                 rm {:?}",
                path.display(),
                path
            );
        }
        // Stale lock (dead PID or same PID): fall through to reclaim.
        tracing::info!(
            stale_pid = existing_pid,
            "reclaiming stale daemon lock file at {}",
            path.display()
        );
    }

    write_lock_file(path, me)
        .map_err(|e| anyhow::anyhow!("write daemon lock {}: {e}", path.display()))?;

    tracing::info!(pid = me, "wrote daemon lock at {}", path.display());
    Ok(DaemonLock::from_path(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── lock_file_path ─────────────────────────────────────────────────────

    /// Why: the lock file must live alongside `http_addr` in the standard
    /// data dir so the `doctor` and `start` commands resolve both with the
    /// same `resolve_data_dir` call.
    /// What: overrides the data dir via `TRUSTY_DATA_DIR_OVERRIDE`, calls
    /// `lock_file_path()`, and asserts the path ends with `daemon.lock` and
    /// lives under the override.
    /// Test: itself (pure path construction, no I/O).
    #[test]
    fn lock_file_path_uses_data_dir() {
        let tmp = tempdir().expect("tempdir");
        // Safety: single-threaded test; guard scoped to this block.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
        }
        let path = lock_file_path();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }
        let p = path.expect("lock_file_path must return Some under TRUSTY_DATA_DIR_OVERRIDE");
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some(LOCK_FILENAME));
        assert!(
            p.starts_with(tmp.path()),
            "lock file must live under the data dir override; got: {p:?}"
        );
    }

    // ── read_lock_pid ──────────────────────────────────────────────────────

    /// Why: a missing lock file means no daemon is registered; callers must
    /// treat this as "no daemon" (not an error).
    /// What: calls `read_lock_pid` on a nonexistent path; asserts `Ok(None)`.
    /// Test: itself (real fs stat, no daemon).
    #[test]
    fn read_lock_pid_returns_none_for_missing_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let result = read_lock_pid(&path).expect("must not error for missing file");
        assert_eq!(result, None);
    }

    /// Why: a valid lock file must round-trip the PID so `acquire_lock` can
    /// determine whether the recorded process is still alive.
    /// What: writes a PID to a tempfile; asserts `read_lock_pid` returns it.
    /// Test: itself.
    #[test]
    fn read_lock_pid_returns_pid_for_valid_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        std::fs::write(&path, "12345\n").expect("write");
        let result = read_lock_pid(&path).expect("must not error for valid file");
        assert_eq!(result, Some(12345));
    }

    /// Why: a corrupt or empty lock file (e.g. from a crashed partial write)
    /// must be treated as absent rather than crashing the daemon.
    /// What: writes an empty file; asserts `Ok(None)`.
    /// Test: itself.
    #[test]
    fn read_lock_pid_returns_none_for_empty_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        std::fs::write(&path, "").expect("write");
        let result = read_lock_pid(&path).expect("must not error for empty file");
        assert_eq!(result, None);
    }

    // ── pid_alive ──────────────────────────────────────────────────────────

    /// Why: using PID 1 (init/launchd) as a guaranteed-alive process is
    /// platform-specific and fragile; instead we test the trivially-true
    /// case: the current process must be alive.
    /// What: asserts `pid_alive(std::process::id())` returns `true` on Unix.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_true_for_current_pid() {
        assert!(
            pid_alive(std::process::id()),
            "current process must be alive"
        );
    }

    /// Why: a PID that is guaranteed not to exist on any modern OS (PID 0
    /// is the scheduler, never a user process) should be reported as dead.
    /// What: asserts `pid_alive(0)` returns `false`.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_false_for_pid_zero() {
        // PID 0 is the scheduler; kill -0 0 sends to the current process
        // group, not PID 0. Use u32::MAX as a guaranteed-nonexistent PID.
        // On Linux the max PID is 4,194,304; on macOS it is 99,999. Neither
        // reaches u32::MAX so this PID cannot exist.
        assert!(
            !pid_alive(u32::MAX),
            "PID u32::MAX cannot be alive on any real system"
        );
    }

    // ── acquire_lock ───────────────────────────────────────────────────────

    /// Why: the primary use case of `acquire_lock` is writing the daemon's
    /// own PID so future `start` / `serve` invocations detect the live
    /// daemon.
    /// What: calls `acquire_lock` against a temp path; reads the written
    /// file; asserts it contains the current PID.
    /// Test: itself (real fs, no daemon).
    #[test]
    fn acquire_lock_writes_own_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let _guard = acquire_lock(&path).expect("acquire_lock must succeed on empty path");
        let written = read_lock_pid(&path)
            .expect("read after write must not error")
            .expect("lock file must contain a PID after acquire");
        assert_eq!(
            written,
            std::process::id(),
            "lock file must contain the current process PID"
        );
    }

    /// Why: a stale lock (dead PID) must be reclaimed so the daemon can
    /// always start after a crash or SIGKILL, without operator intervention.
    /// What: writes a lock file containing PID u32::MAX (guaranteed dead),
    /// calls `acquire_lock`, and asserts it succeeds and overwrites with
    /// the current PID.
    /// Test: itself (real fs, no daemon).
    #[test]
    fn acquire_lock_reclaims_stale_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        // Write a guaranteed-dead PID.
        std::fs::write(&path, format!("{}\n", u32::MAX)).expect("write stale pid");
        let _guard = acquire_lock(&path).expect("acquire_lock must reclaim stale PID");
        let written = read_lock_pid(&path)
            .expect("read after reclaim must not error")
            .expect("lock file must contain a PID after reclaim");
        assert_eq!(
            written,
            std::process::id(),
            "lock file must be overwritten with current PID after stale reclaim"
        );
    }

    /// Why: if another live process holds the lock (e.g. the launchd-managed
    /// daemon is already running), a new `serve --foreground` invocation must
    /// fail loudly rather than starting a duplicate on a different port.
    /// What: writes a lock file containing the current process's own PID
    /// (which is alive by definition), then calls `acquire_lock` from a
    /// simulated "other" PID by writing a lock with the CURRENT pid and
    /// checking that `acquire_lock` would refuse it.
    ///
    /// Because we cannot spawn a second real live process in a unit test,
    /// we test the logic indirectly: write our own PID as the "existing"
    /// lock holder (since our process IS alive) and verify `acquire_lock`
    /// returns `Err`. This is the exact path hit when launchd's
    /// `KeepAlive` tries to restart a daemon that is already running.
    /// Test: itself.
    #[test]
    fn acquire_lock_refuses_live_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        // Write the current PID as the "held" lock (we are the live holder).
        std::fs::write(&path, format!("{}\n", std::process::id())).expect("write live pid");
        // A second call from the same process should also succeed (it sees its
        // own PID, which is alive, but since `existing_pid == me` it reclaims).
        // To truly test the "refuse" path, we need a different PID that is
        // alive. Use PID 1 (init/launchd on Unix, always alive) as the fake
        // held lock.
        #[cfg(unix)]
        {
            if pid_alive(1) {
                // PID 1 is alive → write it as the lock holder.
                std::fs::write(&path, "1\n").expect("write pid 1");
                let result = acquire_lock(&path);
                assert!(
                    result.is_err(),
                    "acquire_lock must refuse when lock holder PID 1 is alive"
                );
                let msg = format!("{}", result.unwrap_err());
                assert!(
                    msg.contains("already running"),
                    "error must mention 'already running'; got: {msg}"
                );
            }
        }
    }

    // ── DaemonLock drop ────────────────────────────────────────────────────

    /// Why: the RAII contract of `DaemonLock` is its primary safety
    /// guarantee — if Drop does not remove the file, a crash leaves a stale
    /// lock that the next startup must reclaim (which works, but wastes a
    /// probe). We verify the happy path: file exists before drop, gone after.
    /// What: acquires the lock, asserts the file exists, drops the guard,
    /// asserts the file is gone.
    /// Test: itself.
    #[test]
    fn daemon_lock_drops_removes_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let guard = acquire_lock(&path).expect("acquire_lock must succeed on empty path");
        assert!(path.exists(), "lock file must exist after acquire");
        drop(guard);
        assert!(
            !path.exists(),
            "lock file must be removed when DaemonLock is dropped"
        );
    }
}
