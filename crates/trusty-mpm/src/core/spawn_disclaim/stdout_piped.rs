//! Synchronous piped-stdout disclaimed spawn — the public, platform-agnostic
//! half of [`macos::spawn_stdout_piped_disclaimed`] (the macOS
//! `posix_spawn`-based implementation lives in `macos::stdout_piped`; this
//! file holds the public API, the non-macOS/[`DISABLE_ENV`] fallback, and the
//! shared [`StdoutPipedSpawn`] return type both paths produce).
//!
//! Why: `formatters::info_box::probes::run_git_log` (in the `tm` binary)
//! spawns `git log` with a 3-second SIGKILL watchdog that must be armed with
//! the child's pid BEFORE the blocking read-to-EOF-then-wait — issue #3267
//! (#2997 part 6). Split into its own module (split from
//! `spawn_disclaim/mod.rs`, alongside the pre-existing
//! `macos::stderr_piped`/`macos::stdout_piped` split) to keep `mod.rs` under
//! the crate's 500-SLOC production file cap.
//! What: [`StdoutPipedSpawn`] exposes the child's pid plus a boxed
//! synchronous stdout reader and a lifecycle handle;
//! [`disclaimed_stdout_piped_spawn`] spawns `cmd` with stdout piped, stderr
//! discarded to `/dev/null`, and stdin inherited, disclaiming TCC
//! responsibility on macOS exactly like [`super::disclaimed_output`]. On
//! non-macOS (or with [`DISABLE_ENV`] set) it sets the same stdio shape on
//! `cmd` and delegates to `cmd.spawn()`, exposing `Child::id()` exactly as
//! the pre-fix code did.
//! Test: `tests::disclaimed_stdout_piped_spawn_captures_and_waits`,
//! `tests::disclaimed_stdout_piped_spawn_id_allows_watchdog_kill_before_wait`,
//! `tests::disclaimed_stdout_piped_spawn_reports_spawn_error_for_missing_binary`,
//! `tests::disclaimed_stdout_piped_spawn_disable_env_forces_plain_path`
//! (macOS-only); `native_tests::disclaimed_stdout_piped_spawn_native_path_round_trips`
//! (any OS — exercises the exact code path Linux always takes).

use std::io::Read as _;
use std::process::{Command, Output, Stdio};

// Used by the `#[cfg(target_os = "macos")]` branch in
// `disclaimed_stdout_piped_spawn` below AND by both test submodules'
// `TM_DISABLE_SPAWN_DISCLAIM` toggles — but NOT by a plain, non-test,
// non-macOS `--lib` build, where it would otherwise be a genuine unused
// import (issue caught by CI's Linux `Clippy` job: neither the macOS cfg
// branch nor `#[cfg(test)]` is active there).
#[cfg(any(target_os = "macos", test))]
use super::DISABLE_ENV;
#[cfg(target_os = "macos")]
use super::macos;

/// A piped-stdout child spawned by [`disclaimed_stdout_piped_spawn`] —
/// stderr discarded, stdin inherited — exposing the child's pid so a caller
/// can arm an external kill-watchdog BEFORE blocking on
/// [`Self::wait_with_output`].
///
/// Why: see the module docs — [`super::disclaimed_output`] only returns
/// after the child has already exited, so it cannot expose the pid in time.
/// What: `id` mirrors `std::process::Child::id()`; `wait_with_output`
/// consumes `self`, drains stdout, and reaps the child.
/// Test: see [`disclaimed_stdout_piped_spawn`].
pub struct StdoutPipedSpawn {
    /// The child's OS process id, valid immediately after spawn — used to
    /// arm an external kill-watchdog before [`Self::wait_with_output`] blocks.
    pub id: u32,
    pub(super) stdout: Box<dyn std::io::Read + Send>,
    pub(super) handle: StdoutPipedHandle,
}

pub(super) enum StdoutPipedHandle {
    Native(std::process::Child),
    #[cfg(target_os = "macos")]
    Disclaimed(libc::pid_t),
}

impl StdoutPipedSpawn {
    /// Read stdout to EOF, then wait for exit — mirrors
    /// `Child::wait_with_output()`'s stdout+status contract. `stderr` is
    /// always empty (this spawn shape discards it to `/dev/null`).
    ///
    /// Why/What: see the struct docs.
    /// Test: see [`disclaimed_stdout_piped_spawn`].
    pub fn wait_with_output(mut self) -> std::io::Result<Output> {
        let mut stdout = Vec::new();
        self.stdout.read_to_end(&mut stdout)?;
        let status = match self.handle {
            StdoutPipedHandle::Native(mut child) => child.wait()?,
            #[cfg(target_os = "macos")]
            StdoutPipedHandle::Disclaimed(pid) => macos::wait_for(pid)?,
        };
        Ok(Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    }
}

/// Spawn `cmd` with stdout piped, stderr discarded to `/dev/null`, and stdin
/// inherited — and, on macOS, disclaim TCC responsibility exactly like
/// [`super::disclaimed_output`]. Returns immediately with the child's pid and
/// a live stdout reader instead of blocking until the child exits.
///
/// Why: fixes the second spawn site named by issue #3267 (#2997 part 6):
/// `formatters::info_box::probes::run_git_log` previously called
/// `std::process::Command::new("git")...spawn()` directly. Its 3-second
/// SIGKILL watchdog (guarding against a slow/network-mounted `.git`) needs
/// the pid immediately after spawn — before the blocking
/// read-then-wait — so this fits neither [`super::disclaimed_output`]
/// (blocks internally until the child exits, pid never exposed) nor
/// [`super::disclaimed_piped_spawn`] (async, three pipes); it needs its own
/// thin wrapper following the same argv/envp/cwd/disclaim-SPI reconstruction
/// pattern as `macos::spawn_status_inherit_disclaimed`.
/// What: on macOS, re-derives argv/envp/cwd from `cmd`, spawns via
/// `posix_spawnp` with a piped stdout, a `/dev/null` stderr file action, no
/// stdin file action (inherited), and the disclaim attribute when the
/// private SPI resolves — returning the pid immediately, unwaited. On
/// non-macOS (or with [`DISABLE_ENV`] set) sets
/// `cmd.stdout(Stdio::piped()).stderr(Stdio::null())` and delegates to
/// `cmd.spawn()`, exposing `Child::id()` exactly as the pre-fix code did.
/// Test: `tests::disclaimed_stdout_piped_spawn_captures_and_waits`,
/// `tests::disclaimed_stdout_piped_spawn_reports_spawn_error_for_missing_binary`,
/// `tests::disclaimed_stdout_piped_spawn_disable_env_forces_plain_path`
/// (macOS-only); `native_tests::disclaimed_stdout_piped_spawn_native_path_round_trips`
/// (any OS).
pub fn disclaimed_stdout_piped_spawn(mut cmd: Command) -> std::io::Result<StdoutPipedSpawn> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_stdout_piped_disclaimed(&cmd);
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let id = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout pipe was not opened"))?;
    Ok(StdoutPipedSpawn {
        id,
        stdout: Box::new(stdout),
        handle: StdoutPipedHandle::Native(child),
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // --- disclaimed_stdout_piped_spawn (issue #3267: probes::run_git_log) ---

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_captures_and_waits() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "echo captured-out; echo should-be-discarded >&2; exit 5",
        ]);
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        assert!(spawned.id > 0, "pid must be available before waiting");
        let out = spawned.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(5));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "captured-out\n");
        assert!(out.stderr.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_id_allows_watchdog_kill_before_wait() {
        // Reproduces the exact shape `run_git_log`'s 3-second SIGKILL
        // watchdog depends on: the pid must be usable to kill the child
        // BEFORE `wait_with_output` is called (which would otherwise block
        // forever on `sleep 30`'s stdout never reaching EOF).
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30");
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        let pid = spawned.id;
        assert!(pid > 0);
        // SAFETY: `pid` is our own freshly spawned child.
        let killed = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        assert_eq!(killed, 0, "SIGKILL must reach the child by its exposed pid");
        let out = spawned.wait_with_output().unwrap();
        assert!(!out.status.success());
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_reports_spawn_error_for_missing_binary() {
        let cmd =
            std::process::Command::new("/nonexistent/definitely-not-a-real-binary-3267-stdout");
        // `StdoutPipedSpawn` holds a `Box<dyn Read>` trait object, so it
        // isn't `Debug` and can't go through `expect_err` — same constraint
        // as `StderrPipedSpawn`.
        let err = disclaimed_stdout_piped_spawn(cmd)
            .err()
            .expect("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo out; exit 3"]);
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        let out = spawned.wait_with_output().unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;

    /// Exercises the exact code path every non-macOS platform always takes
    /// for [`disclaimed_stdout_piped_spawn`] (and that macOS takes under
    /// [`DISABLE_ENV`]) — no `cfg(target_os = "macos")` gate, so this runs on
    /// every CI platform.
    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_native_path_round_trips() {
        if cfg!(target_os = "windows") {
            return; // no portable `sh` equivalent; skip on Windows
        }
        // SAFETY: no other test in this binary reads/writes DISABLE_ENV
        // concurrently with this one.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo native-stdout; exit 4"]);
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert!(spawned.id > 0);

        let out = spawned.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "native-stdout\n");
        assert_eq!(out.status.code(), Some(4));
    }
}
