//! PID lock file for the `trusty-memory serve --foreground` daemon (issue #787).
//!
//! Why: the `start` subcommand used to detect a running daemon ONLY by probing
//! the `http_addr` discovery file. When a launchd-managed `serve --foreground`
//! instance crashed without cleaning up `http_addr`, or was deployed from an
//! older binary that did not write `http_addr`, `start` concluded "no daemon
//! running" and forked a new one that silently port-walked to 7071+.
//! This module provides a PID lock file — written by `serve --foreground`
//! before binding, cleared on graceful shutdown — giving `start` a second,
//! independent signal to detect a live daemon.  A stale lock (PID not alive)
//! is reclaimed transparently so a crash does not permanently block startup.
//!
//! What: exposes [`DaemonLock`] (RAII guard), [`acquire_lock`] (O_EXCL-create
//! then fallback to stale-reclaim), [`read_lock_pid`] (inspect without
//! acquiring), and [`lock_file_path`] / [`lock_file_path_for_dir`] helpers.
//! Only used by the `serve --foreground` path — `start`, CLI subcommands, and
//! the MCP bridge must never call [`acquire_lock`].
//!
//! Test: unit tests below cover stale-lock reclaim, live-lock refusal, and
//! the write/remove cycle, all against temp directories so the real
//! `~/.local/share/trusty-memory` is never touched.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Filename of the daemon PID lock file, written under the trusty-memory
/// data directory alongside `http_addr`.
///
/// Why: co-locating it with `http_addr` keeps both discovery files in the
/// same directory so `doctor` and `start` resolve both with one
/// `resolve_data_dir` call.
/// What: the literal filename; callers join it onto the data-dir path.
/// Test: `lock_file_path_uses_data_dir` asserts the constructed path.
pub const LOCK_FILENAME: &str = "daemon.lock";

/// RAII guard that holds the daemon PID lock file.
///
/// Why: tie the lock file's lifetime to the daemon process so the file is
/// removed on both clean shutdown and panic without requiring every exit
/// path to call an explicit cleanup function.
/// What: wraps the lock-file path; `Drop` removes it best-effort (I/O
/// errors are swallowed — the file is reclaimed as stale on next startup).
/// Test: `daemon_lock_drops_removes_file`.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
}

impl DaemonLock {
    /// Construct directly from a path (test helper + internal use only).
    ///
    /// Why: tests need a `DaemonLock` pointing at a tempfile without OS
    /// data-dir resolution.
    /// What: wraps `path`; the file at `path` is assumed to already exist.
    /// Test: used in `daemon_lock_drops_removes_file`.
    pub(crate) fn from_path(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // Best-effort: if remove fails (e.g. concurrent `trusty-memory stop`)
        // we ignore — the next invocation reclaims the stale file.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Resolve the canonical lock-file path for the trusty-memory daemon.
///
/// Why: centralising the path keeps `acquire_lock`, `read_lock_pid`, and
/// diagnostic checks in agreement.  Returns `None` when the data directory
/// cannot be resolved so callers degrade gracefully rather than panicking.
/// What: returns `{resolve_data_dir("trusty-memory")}/daemon.lock`, or
/// `None` on resolution failure.
/// Test: `lock_file_path_uses_data_dir` asserts the constructed path.
pub fn lock_file_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        .map(|d| d.join(LOCK_FILENAME))
}

/// Build the lock-file path under an explicitly supplied directory.
///
/// Why: test code needs to point at a tempdir without mutating the process
/// environment.  `std::env::set_var` inside a parallel test harness is UB
/// (data race on the env block); this function bypasses the env lookup so
/// tests never touch global process state.
/// What: returns `dir.join(LOCK_FILENAME)`.
/// Test: `lock_file_path_uses_data_dir` calls this instead of
/// `lock_file_path()` so the test never mutates the environment.
pub fn lock_file_path_for_dir(dir: &Path) -> PathBuf {
    dir.join(LOCK_FILENAME)
}

/// Check whether a PID is alive on this Unix host.
///
/// Why: `libc::kill(pid, 0)` avoids forking `/bin/kill` and guards against
/// two Linux edge cases: `pid == 0` has process-group semantics (false
/// positive) and `pid > i32::MAX` wraps to negative `pid_t` giving broadcast
/// semantics (also false positive).
/// What: returns `false` for `pid == 0` or `pid > i32::MAX`.  For valid pids
/// calls `libc::kill(pid, 0)`: 0 → alive, `ESRCH` → dead, `EPERM` → alive.
/// On non-Unix platforms always returns `false`.
/// Test: `pid_alive_returns_false_for_pid_zero`,
/// `pid_alive_returns_false_for_overflow_pid`,
/// `pid_alive_returns_true_for_current_pid`.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: kill(2) is async-signal-safe; signal 0 is liveness-only.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        err != libc::ESRCH // EPERM → exists but no permission → alive
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Read the PID stored in the lock file at `path`.
///
/// Why: `acquire_lock` and diagnostic commands need to read the lock file
/// without acquiring it.
/// What: reads the file, trims whitespace, and parses as `u32`. Returns
/// `None` when the file does not exist, is empty, or contains a non-PID.
/// Returns `Err` for I/O errors other than `NotFound`.
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
    Ok(trimmed.parse::<u32>().ok()) // malformed → None
}

/// Write `{pid}\n` to `path` atomically (write to `.tmp` + rename).
///
/// Why: atomic write prevents a concurrent reader from observing a partial
/// file (e.g. a truncated PID) during the write window.
/// What: creates parent dirs; writes PID + newline to `{path}.tmp`;
/// fsyncs; renames to `path`. Returns `Err` on any I/O failure.
/// Test: called by `acquire_lock`; covered by `acquire_lock_writes_own_pid`.
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

/// Try to create the lock file exclusively (`O_CREAT | O_EXCL`) and write `pid`.
///
/// Why: `O_EXCL` makes the create-and-write atomic — only one concurrent
/// caller can win, eliminating the TOCTOU window between "file absent" and
/// "write PID" (first phase of [`acquire_lock`]'s two-phase strategy).
/// What: creates parent dirs; opens with `create_new` (`O_CREAT | O_EXCL`);
/// writes `{pid}\n` and fsyncs.  Returns `Ok(true)` on success, `Ok(false)`
/// when the file already exists (fall through to stale-reclaim), or `Err`
/// for other I/O failures.
/// Test: covered by `acquire_lock_writes_own_pid` (empty-path happy path).
fn try_create_lock_exclusive(path: &Path, pid: u32) -> std::io::Result<bool> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL
        .open(path)
    {
        Ok(mut f) => {
            writeln!(f, "{pid}")?;
            f.sync_all()?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Attempt to acquire the daemon PID lock file at `path`.
///
/// Why: without a lock, a stale `http_addr` lets `start` fork a new daemon
/// that silently port-walks to 7071+.  The lock gives `start` and the
/// single-instance guard in `main.rs` a second detection layer.
///
/// Two-phase acquisition (closes TOCTOU advisory in #797):
/// 1. **O_EXCL create** — atomic; only one caller wins; no TOCTOU window.
/// 2. **Stale-reclaim fallback** — if the file existed, check the recorded
///    PID; if dead, overwrite; if alive, return `Err("already running")`.
///    A narrow TOCTOU remains on the reclaim path but is bounded: at worst
///    two concurrent starters both see a dead PID and one overwrites the
///    other; defence-in-depth (port-abort) catches the loser.
///
/// What: returns a [`DaemonLock`] RAII guard on success; `Err` if a live
/// daemon already holds the lock.
/// Test: `acquire_lock_writes_own_pid`, `acquire_lock_reclaims_stale_pid`,
/// `acquire_lock_refuses_live_pid`.
pub fn acquire_lock(path: &Path) -> Result<DaemonLock> {
    let me = std::process::id();

    // Phase 1: O_EXCL — race-free when the file is absent.
    match try_create_lock_exclusive(path, me) {
        Ok(true) => {
            tracing::info!(
                pid = me,
                "wrote daemon lock at {} (exclusive create)",
                path.display()
            );
            return Ok(DaemonLock::from_path(path.to_path_buf()));
        }
        Ok(false) => {} // File existed; fall through to Phase 2.
        Err(e) => {
            return Err(anyhow::anyhow!(
                "create daemon lock {}: {e}",
                path.display()
            ));
        }
    }

    // Phase 2: file exists — read the recorded PID and decide.
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

    /// Why: the lock file must live in the standard data dir so `doctor` and
    /// `start` resolve it with one `resolve_data_dir` call.
    /// What: constructs the path via `lock_file_path_for_dir` (no env
    /// mutation — `set_var` is UB under the parallel test runner) and asserts
    /// it ends with `daemon.lock` under the supplied tempdir.
    /// Test: itself (pure path construction, no I/O).
    #[test]
    fn lock_file_path_uses_data_dir() {
        let tmp = tempdir().expect("tempdir");
        let path = lock_file_path_for_dir(tmp.path());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(LOCK_FILENAME)
        );
        assert!(
            path.starts_with(tmp.path()),
            "lock file must live under the data dir; got: {path:?}"
        );
    }

    // ── read_lock_pid ──────────────────────────────────────────────────────

    /// Why: a missing lock file means no daemon registered; callers treat
    /// this as "no daemon" not an error.
    /// What: calls `read_lock_pid` on a nonexistent path; asserts `Ok(None)`.
    /// Test: itself.
    #[test]
    fn read_lock_pid_returns_none_for_missing_file() {
        let tmp = tempdir().expect("tempdir");
        let result = read_lock_pid(&tmp.path().join("daemon.lock"))
            .expect("must not error for missing file");
        assert_eq!(result, None);
    }

    /// Why: a valid lock file must round-trip the PID so `acquire_lock` can
    /// check liveness of the recorded process.
    /// What: writes a PID; asserts `read_lock_pid` returns it.
    /// Test: itself.
    #[test]
    fn read_lock_pid_returns_pid_for_valid_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        std::fs::write(&path, "12345\n").expect("write");
        assert_eq!(
            read_lock_pid(&path).expect("must not error for valid file"),
            Some(12345)
        );
    }

    /// Why: a corrupt or empty lock file (e.g. partial write) must be treated
    /// as absent rather than crashing the daemon.
    /// What: writes an empty file; asserts `Ok(None)`.
    /// Test: itself.
    #[test]
    fn read_lock_pid_returns_none_for_empty_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        std::fs::write(&path, "").expect("write");
        assert_eq!(
            read_lock_pid(&path).expect("must not error for empty file"),
            None
        );
    }

    // ── pid_alive ──────────────────────────────────────────────────────────

    /// Why: the current process must always be alive; this is the safe,
    /// reliable alternative to hard-coding PID 1.
    /// What: asserts `pid_alive(std::process::id())` is `true` on Unix.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_true_for_current_pid() {
        assert!(
            pid_alive(std::process::id()),
            "current process must be alive"
        );
    }

    /// Why: `pid == 0` has process-group semantics; the guard must
    /// short-circuit before any syscall.
    /// What: asserts `pid_alive(0)` is `false`.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_false_for_pid_zero() {
        assert!(!pid_alive(0), "pid 0 has process-group semantics");
    }

    /// Why: `pid > i32::MAX` wraps to negative `pid_t` giving broadcast
    /// semantics (`kill(-1, 0)`) — must guard before syscall.
    /// What: asserts both `u32::MAX` and `i32::MAX as u32 + 1` return `false`.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_false_for_overflow_pid() {
        assert!(!pid_alive(u32::MAX), "u32::MAX overflows i32");
        assert!(!pid_alive(i32::MAX as u32 + 1), "first i32-overflow value");
    }

    // ── acquire_lock ───────────────────────────────────────────────────────

    /// Why: the primary use case of `acquire_lock` is writing the daemon's own
    /// PID so future invocations detect the live daemon.  On an empty path the
    /// O_EXCL exclusive-create branch (Phase 1) must succeed.
    /// What: calls `acquire_lock` against a fresh temp path; reads the file;
    /// asserts it contains the current PID.
    /// Test: itself (real fs, no daemon).
    #[test]
    fn acquire_lock_writes_own_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        assert!(!path.exists(), "pre-condition: lock file must not exist");
        let _guard = acquire_lock(&path).expect("acquire must succeed on empty path");
        let written = read_lock_pid(&path)
            .expect("read after write must not error")
            .expect("lock file must contain a PID after acquire");
        assert_eq!(written, std::process::id());
    }

    /// Why: stale locks must be reclaimed after a crash so the daemon can
    /// restart.  Uses a real spawned+reaped child (not `u32::MAX`) to avoid
    /// the broadcast-semantics false-positive on Linux.
    /// What: spawns+reaps `true`, writes its dead PID as the stale lock, calls
    /// `acquire_lock`, asserts success and PID overwrite.
    /// Test: itself (real fs, spawns `true`).
    #[cfg(unix)]
    #[test]
    fn acquire_lock_reclaims_stale_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn 'true' must succeed");
        let dead_pid = child.id();
        child.wait().expect("wait must succeed");
        assert!(
            !pid_alive(dead_pid),
            "pid_alive({dead_pid}) must be false after child was reaped"
        );
        std::fs::write(&path, format!("{dead_pid}\n")).expect("write stale pid");
        let _guard = acquire_lock(&path).expect("acquire must reclaim stale PID");
        let written = read_lock_pid(&path)
            .expect("read after reclaim must not error")
            .expect("lock file must contain a PID after reclaim");
        assert_eq!(written, std::process::id());
    }

    /// Why: non-Unix platforms always return false from `pid_alive` so any
    /// lock is treated as stale.
    /// What: writes a PID; asserts reclaim succeeds.
    /// Test: itself (non-Unix only).
    #[cfg(not(unix))]
    #[test]
    fn acquire_lock_reclaims_stale_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        std::fs::write(&path, "99999\n").expect("write stale pid");
        let _guard = acquire_lock(&path).expect("acquire must reclaim stale PID on non-Unix");
        assert_eq!(
            read_lock_pid(&path).expect("read").expect("pid"),
            std::process::id()
        );
    }

    /// Why: if a live process holds the lock a new `serve --foreground` must
    /// fail loudly rather than starting a duplicate on a different port.
    /// What: writes PID 1 (init/launchd — alive on any Unix) as the held lock;
    /// asserts `acquire_lock` returns `Err` containing "already running".
    /// Test: itself (unix only; skipped if PID 1 unreachable e.g. containers).
    #[test]
    fn acquire_lock_refuses_live_pid() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        #[cfg(unix)]
        {
            if pid_alive(1) {
                std::fs::write(&path, "1\n").expect("write pid 1");
                let result = acquire_lock(&path);
                assert!(result.is_err(), "must refuse live lock holder PID 1");
                assert!(
                    format!("{}", result.unwrap_err()).contains("already running"),
                    "error must mention 'already running'"
                );
            }
        }
    }

    // ── DaemonLock drop ────────────────────────────────────────────────────

    /// Why: `DaemonLock::drop` is the primary safety guarantee — it removes
    /// the file so a clean shutdown leaves no stale lock.
    /// What: acquires the lock; asserts file exists; drops guard; asserts gone.
    /// Test: itself.
    #[test]
    fn daemon_lock_drops_removes_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let guard = acquire_lock(&path).expect("acquire must succeed on empty path");
        assert!(path.exists(), "lock file must exist after acquire");
        drop(guard);
        assert!(!path.exists(), "lock file must be removed on drop");
    }
}
