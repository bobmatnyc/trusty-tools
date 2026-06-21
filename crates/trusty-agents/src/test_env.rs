//! Shared test-only synchronization primitives and executable-file helpers.
//!
//! Why: Multiple unit tests across different modules (`init::tests`,
//! `mistake_log::tests`, etc.) sandbox `$HOME` with `std::env::set_var` to
//! redirect file I/O into a tempdir. `set_var` is a process-wide mutation
//! and `cargo test` runs unit tests on a multi-threaded executor by default,
//! so two concurrent tests sandboxing HOME stomp on each other and one will
//! observe the other's tempdir (or restore HOME mid-flight). The classic
//! fix is a per-test-module `static Mutex`, but that only serializes tests
//! WITHIN one module — cross-module races (e.g. `init::seed_skills_*` vs
//! `mistake_log::mistake_log_records_nonzero_exit`) still flake.
//! What: Exposes a single process-wide `HOME_LOCK` that every test mutating
//! `$HOME` must hold. Also provides `write_executable_script` and
//! `spawn_script` helpers to avoid ETXTBSY races (#1528).
//! Compiled only under `#[cfg(test)]` so it costs nothing in release builds.
//! Test: Used by `mistake_log::tests::mistake_log_records_nonzero_exit` and
//! `init::tests::*`. The mistake_log test was order-dependent before this
//! lock was introduced — passed in isolation, failed under `cargo test`.

#![cfg(test)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Process-wide mutex serializing tests that mutate `$HOME`.
///
/// Why: `std::env::set_var` is a process-global mutation; tokio's default
/// multi-threaded test runtime causes interleaved tests to overwrite each
/// other's HOME before they restore the original value. A single static
/// Mutex shared across all modules keeps such tests sequential without
/// forcing `--test-threads=1` for the whole crate.
/// What: Use `let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());`
/// at the top of any test that calls `std::env::set_var("HOME", ...)`.
/// `unwrap_or_else(into_inner)` ensures a panic in one test doesn't poison
/// the lock for siblings.
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Process-wide mutex serializing tests that mutate LLM credential env vars
/// (#250). Same rationale as `HOME_LOCK` — `std::env::set_var` is global so
/// concurrent credential-routing tests would race.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Write a shell script to `dir/name`, flush and close the file handle, THEN
/// set the execute bit — eliminating the ETXTBSY race (#1528).
///
/// Why: On Linux, the kernel returns ETXTBSY (os error 26) when a process
/// tries to exec a file that still has an open writable file descriptor.
/// `std::fs::write` leaves no lingering handle, but calling `set_permissions`
/// immediately after the write can still race under heavy CI load if another
/// thread holds a cross-process lock on the inode. The safe pattern is:
/// open → write → flush → **explicitly close the handle** → then chmod.
/// Dropping the `File` before `set_permissions` guarantees the writable fd
/// is gone before the kernel is asked to exec the file.
/// What: Creates the file at `dir.join(name)`, writes `contents`, syncs,
/// drops the file handle, sets mode 0o755, and returns the absolute path.
/// Test: All tests in `subprocess::tests` and `agents::claude_code_runner::tests`
/// that spawn a mock shell script call this helper; ETXTBSY flakiness should
/// not recur after the refactor.
#[cfg(unix)]
pub fn write_executable_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    {
        // Scope the File so it is closed before set_permissions.
        let mut f = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
        f.write_all(contents.as_bytes())
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        f.flush()
            .unwrap_or_else(|e| panic!("flush {}: {e}", path.display()));
        // `f` drops here — fd is closed before chmod.
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    path
}

/// Spawn a shell script at `path` with stdin=null, stdout=piped, stderr=inherit,
/// retrying on ETXTBSY (os error 26) up to 3 times with exponential back-off.
///
/// Why: Even after closing the write handle before chmod, kernel page-cache
/// flush timing under heavy CI load can delay the execute permission becoming
/// visible to `execve`. A small bounded retry (≤3 attempts, 5 ms back-off)
/// acts as belt-and-suspenders without hiding real failures.
/// What: Constructs a `tokio::process::Command` fresh on each attempt
/// (required because `Command` is not `Clone`) and calls `.spawn()`.
/// On ETXTBSY it backs off and retries; on any other error or after
/// `MAX_ATTEMPTS` ETXTBSY hits it returns the error immediately.
/// The stdio setup (stdin null / stdout piped / stderr inherited) matches
/// the pattern used by all subprocess and claude-code-runner tests.
/// Test: Covered indirectly by every async test that calls `spawn_script`;
/// a direct unit test would require artificially injecting ETXTBSY which is
/// impractical in pure Rust tests. The helper's correctness is validated by
/// the absence of CI failures after the refactor.
#[cfg(unix)]
pub async fn spawn_script(path: &std::path::Path) -> std::io::Result<tokio::process::Child> {
    use std::process::Stdio;

    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFF_MS: u64 = 5;

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let mut cmd = tokio::process::Command::new(path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(
                    BACKOFF_MS * (1 << attempt),
                ))
                .await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap())
}
