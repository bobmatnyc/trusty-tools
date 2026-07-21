//! Synchronous piped-stderr disclaimed spawn — the public, platform-agnostic
//! half of [`macos::spawn_stderr_piped_disclaimed`] (the macOS
//! `posix_spawn`-based implementation lives in `macos::stderr_piped`; this
//! file holds the public API, the non-macOS/[`DISABLE_ENV`] fallback, and the
//! shared [`StderrPipedSpawn`] return type both paths produce).
//!
//! Why: `provisioner::clone_progress::clone_with_progress` reads `git clone
//! --progress`'s stderr line-by-line WHILE THE CHILD RUNS (to emit live
//! `CloningRepo` stage percentage detail), so it needs a real-time stderr
//! handle rather than [`super::disclaimed_output`]'s capture-to-completion
//! contract — issue #3267 (#2997 part 6). Split into its own module (split
//! from `spawn_disclaim/mod.rs`, alongside the pre-existing
//! `macos::stderr_piped`/`macos::stdout_piped` split) to keep `mod.rs` under
//! the crate's 500-SLOC production file cap.
//! What: [`StderrPipedSpawn`] is a boxed synchronous stderr reader plus a
//! lifecycle handle; [`disclaimed_stderr_piped_spawn`] spawns `cmd` with
//! stdout discarded to `/dev/null`, stderr piped, and stdin inherited,
//! disclaiming TCC responsibility on macOS exactly like
//! [`super::disclaimed_output`]. On non-macOS (or with [`DISABLE_ENV`] set)
//! it sets the same stdio shape on `cmd` and delegates to `cmd.spawn()`,
//! taking the child's stderr pipe exactly as the pre-fix code did.
//! Test: `tests::disclaimed_stderr_piped_spawn_streams_and_waits`,
//! `tests::disclaimed_stderr_piped_spawn_applies_cwd_and_env_override`,
//! `tests::disclaimed_stderr_piped_spawn_reports_spawn_error_for_missing_binary`,
//! `tests::disclaimed_stderr_piped_spawn_disable_env_forces_plain_path`
//! (macOS-only); `native_tests::disclaimed_stderr_piped_spawn_native_path_round_trips`
//! (any OS — exercises the exact code path Linux always takes).

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};

// Used by the `#[cfg(target_os = "macos")]` branch in
// `disclaimed_stderr_piped_spawn` below AND by both test submodules'
// `TM_DISABLE_SPAWN_DISCLAIM` toggles — but NOT by a plain, non-test,
// non-macOS `--lib` build, where it would otherwise be a genuine unused
// import (issue caught by CI's Linux `Clippy` job: neither the macOS cfg
// branch nor `#[cfg(test)]` is active there).
#[cfg(any(target_os = "macos", test))]
use super::DISABLE_ENV;
#[cfg(target_os = "macos")]
use super::macos;

/// A synchronously-readable, piped-stderr child spawned by
/// [`disclaimed_stderr_piped_spawn`] — stdout discarded, stdin inherited.
///
/// Why: see the module docs.
/// What: `stderr` is a boxed synchronous reader (the macOS-disclaimed pipe
/// read end, or a native `std::process::ChildStderr`) the caller drains to
/// EOF; [`Self::wait`] then reaps the child.
/// Test: see [`disclaimed_stderr_piped_spawn`].
pub struct StderrPipedSpawn {
    /// The child's stderr, readable synchronously while the process runs.
    pub stderr: Box<dyn Read + Send>,
    pub(super) handle: StderrPipedHandle,
}

pub(super) enum StderrPipedHandle {
    Native(std::process::Child),
    #[cfg(target_os = "macos")]
    Disclaimed(libc::pid_t),
}

impl StderrPipedSpawn {
    /// Wait for the child to exit, mirroring `Child::wait()`.
    ///
    /// Why: callers must drain `stderr` to EOF first (as
    /// `clone_with_progress` does) — the pipe's write end only closes once
    /// the child exits or closes fd 2 itself, so waiting before draining a
    /// large-output child can deadlock on a full pipe buffer.
    /// What: reaps the native `Child` or, on the macOS-disclaimed path, the
    /// raw pid via [`macos::wait_for`].
    /// Test: see [`disclaimed_stderr_piped_spawn`].
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match &mut self.handle {
            StderrPipedHandle::Native(child) => child.wait(),
            #[cfg(target_os = "macos")]
            StderrPipedHandle::Disclaimed(pid) => macos::wait_for(*pid),
        }
    }
}

/// Spawn `cmd` with stdout discarded to `/dev/null`, stdin inherited, and
/// stderr piped for synchronous streaming — and, on macOS, disclaim TCC
/// responsibility exactly like [`super::disclaimed_output`]. Returns
/// immediately with a live stderr reader instead of blocking until the child
/// exits.
///
/// Why: fixes one of the two spawn sites named by issue #3267 (#2997 part 6):
/// `clone_progress::clone_with_progress` previously called `cmd.spawn()`
/// directly, so the `git clone`'s (and anything a clone/checkout hook
/// forks') TCC access rolled up to the signed `trusty-mpm` binary — the same
/// mis-attribution shape as #2819/#2721/#2997/#3126, just on the
/// daemon-initiated workspace-provisioning path rather than a `claude`
/// launch. This is the "closest existing shape is the piped/long-lived
/// pattern" the issue called out, minimally adapted: unlike
/// [`super::disclaimed_piped_spawn`] this is synchronous (no tokio —
/// `clone_with_progress` is a blocking function called from a non-async
/// trait method) and pipes ONLY stderr rather than all three streams.
/// What: on macOS, re-derives argv/envp/cwd from `cmd` via the same stable
/// `Command` accessors `macos::spawn_status_inherit_disclaimed` uses, and
/// spawns via `posix_spawnp` with a `/dev/null` stdout file action, a piped
/// stderr, no stdin file action (inherited — matches the pre-existing
/// `Command`'s implicit default), and the disclaim attribute when the
/// private SPI resolves. On non-macOS (or with [`DISABLE_ENV`] set) sets
/// `cmd.stdout(Stdio::null()).stderr(Stdio::piped())` and delegates to
/// `cmd.spawn()`, taking the child's stderr pipe exactly as the pre-fix code
/// did.
/// Test: `tests::disclaimed_stderr_piped_spawn_streams_and_waits`,
/// `tests::disclaimed_stderr_piped_spawn_reports_spawn_error_for_missing_binary`,
/// `tests::disclaimed_stderr_piped_spawn_disable_env_forces_plain_path`
/// (macOS-only); `native_tests::disclaimed_stderr_piped_spawn_native_path_round_trips`
/// (any OS).
pub fn disclaimed_stderr_piped_spawn(mut cmd: Command) -> std::io::Result<StderrPipedSpawn> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_stderr_piped_disclaimed(&cmd);
        }
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("child stderr pipe was not opened"))?;
    Ok(StderrPipedSpawn {
        stderr: Box::new(stderr),
        handle: StderrPipedHandle::Native(child),
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // --- disclaimed_stderr_piped_spawn (issue #3267: clone_with_progress) ---

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_streams_and_waits() {
        // Writes to BOTH stdout and stderr, plus a distinct exit code — pins
        // the exact stdio shape `clone_with_progress` depends on: stdout
        // discarded (never observable), stderr captured, exit status
        // preserved.
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "echo should-be-discarded; echo captured-line >&2; exit 5",
        ]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "captured-line\n");
        let status = spawned.wait().unwrap();
        assert_eq!(status.code(), Some(5));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_applies_cwd_and_env_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmp_path = std::fs::canonicalize(tmp.path()).unwrap();

        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.current_dir(&tmp_path);
        cmd.env("SPAWN_DISCLAIM_STDERR_TEST_VAR_3267", "hello-3267");
        cmd.args([
            "-c",
            "printf '%s' \"$SPAWN_DISCLAIM_STDERR_TEST_VAR_3267\" >&2 && pwd > cwd.txt",
        ]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        let status = spawned.wait().unwrap();
        assert!(status.success());
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "hello-3267");

        let cwd_contents = std::fs::read_to_string(tmp_path.join("cwd.txt")).unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd_contents.trim()).unwrap(),
            tmp_path,
            "expected the Command's current_dir to reach the child via chdir_np"
        );
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_reports_spawn_error_for_missing_binary() {
        let cmd =
            std::process::Command::new("/nonexistent/definitely-not-a-real-binary-3267-stderr");
        // `StderrPipedSpawn` holds a `Box<dyn Read>` trait object, so it
        // isn't `Debug` and can't go through `expect_err` (mirrors
        // `PipedSpawn`'s same constraint — see
        // `spawn_piped_disclaimed_reports_spawn_error_for_missing_binary`).
        let err = disclaimed_stderr_piped_spawn(cmd)
            .err()
            .expect("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 2"]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        let status = spawned.wait().unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "err\n");
        assert_eq!(status.code(), Some(2));
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;

    /// Exercises the exact code path every non-macOS platform always takes
    /// for [`disclaimed_stderr_piped_spawn`] (and that macOS takes under
    /// [`DISABLE_ENV`]) — no `cfg(target_os = "macos")` gate, so this runs on
    /// every CI platform.
    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_native_path_round_trips() {
        if cfg!(target_os = "windows") {
            return; // no portable `sh` equivalent; skip on Windows
        }
        // SAFETY: no other test in this binary reads/writes DISABLE_ENV
        // concurrently with this one.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo native-stderr >&2; exit 4"]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };

        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        let status = spawned.wait().unwrap();
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "native-stderr\n");
        assert_eq!(status.code(), Some(4));
    }
}
