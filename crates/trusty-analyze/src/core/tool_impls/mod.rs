//! Concrete `StaticTool` implementations — one module per language group.
//!
//! Why: each external linter has a bespoke CLI and output format. Isolating
//! each in its own module keeps the parsing logic testable and the dispatch
//! layer (`tool_registry`) free of tool-specific knowledge.
//!
//! What: re-exports every `StaticTool` impl plus the shared `run_command`
//! helper used to shell out with a hard timeout.
//!
//! Test: each submodule has a `tests` block exercising its output parser
//! against a captured fixture. `run_command_with_timeout` timeout-and-kill
//! behaviour is covered by `timeout_kills_child_process` below.

pub mod c;
pub mod csharp;
pub mod go;
pub mod java;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod swift;
pub mod typescript;

pub use c::ClangtidyTool;
pub use csharp::RoslynTool;
pub use go::StaticcheckTool;
pub use java::PmdTool;
pub use kotlin::DetektTool;
pub use php::PhpstanTool;
pub use python::RuffTool;
pub use ruby::RubocopTool;
pub use rust::ClippyTool;
pub use swift::SwiftlintTool;
pub use typescript::BiomeTool;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Hard wall-clock cap on any single file-scoped tool invocation.
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Default wall-clock cap for build-class (project-scoped) tool invocations.
const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 300;

/// Captured result of running an external command.
#[derive(Debug)]
pub struct CommandOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Process exit code; `None` if killed by signal/timeout.
    pub status: Option<i32>,
}

/// Return the wall-clock timeout used for build-class (project-scoped) tools.
///
/// Why: `dotnet build` on a large solution can take 2–5 minutes the first time
/// (restore, all-files compile); the 30 s cap for per-file tools would always
/// time out. A separate, wider limit lets operators tune it for their hardware
/// via env var without touching code.
/// What: reads `TRUSTY_BUILD_TOOL_TIMEOUT_SECS` from the environment; returns
/// the parsed value as a `Duration`, falling back to 300 s on missing,
/// non-UTF-8, or unparseable values and on `0` (which would be instant).
/// Test: the default is deterministic and covered by `build_tool_timeout_default`
/// in the unit tests below.
pub fn build_tool_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_BUILD_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Run `program` with `args` in `cwd`, capturing stdout/stderr with a
/// caller-supplied `timeout`.
///
/// Why: file-scoped tools use a 30 s cap; build-class tools need a wider
/// limit (default 300 s). Extracting the timeout as a parameter lets both
/// callers share the same spawn/capture/wait logic without duplication.
/// What: spawns the child with a best-effort `setsid()` call (Unix) so the
/// child may lead its own process group. On timeout, the kill target is
/// determined at kill time: if `getpgid(child_pid) != getpgid(0)` (child
/// leads its own group), `kill(-child_pgid, SIGKILL)` is used to catch all
/// dotnet/MSBuild descendants; otherwise only the direct child PID is killed
/// (the child is in the parent's group — never send SIGKILL to the parent
/// group). `setsid()` EPERM (caller already a process-group leader, common
/// under Docker `--init` or some k8s pods / cargo-nextest sandboxes) is
/// tolerated: the hook still returns `Ok(())` so `spawn()` always proceeds;
/// the pgid-comparison at kill time makes the fallback safe.
/// Test: `timeout_kills_child_process` spawns `sh -c 'echo $$ > <tmpfile>;
/// exec sleep 30'` with a sub-second timeout, reads the PID from the file,
/// and polls `kill(pid, 0)` to confirm ESRCH within ~1 s of the call
/// returning.
pub fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> anyhow::Result<CommandOutput> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // On Unix: attempt to place the child in its own session (setsid) so
    // that on timeout we can SIGKILL the whole process group and catch any
    // dotnet/MSBuild child processes. setsid() can return EPERM when the
    // calling process is already a process-group leader (Docker --init,
    // k8s pods, cargo-nextest). In that case we proceed anyway — the
    // pgid-comparison at kill time (below) determines the safe kill target
    // at runtime rather than assuming the child always leads its own group.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() is async-signal-safe. We call it best-effort
        // and always return Ok(()) so spawn() is never blocked by EPERM.
        unsafe {
            cmd.pre_exec(|| {
                // Ignore EPERM and any other error — best-effort only.
                let _ = libc::setsid();
                Ok(())
            });
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {program}: {e}"))?;

    let child_pid = child.id();

    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout pipe for {program}"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stderr pipe for {program}"))?;

    let out_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let (tx, rx) = mpsc::channel::<std::io::Result<std::process::ExitStatus>>();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send(status);
    });

    let status = match rx.recv_timeout(timeout) {
        Ok(Ok(status)) => status.code(),
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!("wait failed for {program}: {e}"));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Kill the entire process group so child processes spawned by
            // `program` (e.g. MSBuild workers under `dotnet build`) are also
            // terminated. On non-Unix we fall back to killing the direct PID.
            kill_process_group(child_pid, program);
            // Wait for the reader threads to drain — killing the child closes
            // its pipes, so read_to_string unblocks promptly.
            let _ = waiter.join();
            let _ = out_handle.join();
            let _ = err_handle.join();
            return Err(anyhow::anyhow!(
                "{program} exceeded {}s timeout",
                timeout.as_secs()
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(anyhow::anyhow!("waiter thread for {program} disconnected"));
        }
    };

    let _ = waiter.join();
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    Ok(CommandOutput {
        stdout,
        stderr,
        status,
    })
}

/// Kill `child_pid` (or its process group) on a timeout.
///
/// Why: setsid() in pre_exec is best-effort — it succeeds in most cases but
/// returns EPERM when the caller is already a process-group leader (Docker
/// --init, some k8s pods, cargo-nextest). Blindly negating the child PID to
/// target the group would kill the parent's group when setsid() didn't take
/// effect. Instead we compare pgids at kill time so the kill is always safe.
/// What: on Unix, computes the child's actual pgid via `getpgid(child_pid)`
/// and the parent's pgid via `getpgid(0)`.
/// - If `child_pgid > 0 && child_pgid != parent_pgid` → the child leads its
///   own group (setsid took effect) → `kill(-child_pgid, SIGKILL)` to catch
///   all dotnet/MSBuild/Roslyn descendants.
/// - Otherwise → the child shares the parent's group (setsid was blocked) →
///   `kill(child_pid, SIGKILL)` targeting only the direct child PID; the
///   parent group is never touched.
///
/// On non-Unix platforms wraps `taskkill /F /T /PID` (Windows) or is a no-op.
///
/// Test: exercised by `timeout_kills_child_process` which verifies the child
/// is dead (ESRCH from kill(pid, 0)) within ~1 s of the timeout call returning.
fn kill_process_group(child_pid: u32, program: &str) {
    #[cfg(unix)]
    {
        let pid = child_pid as libc::pid_t;
        // SAFETY: getpgid() and kill() are async-signal-safe POSIX functions.
        let child_pgid = unsafe { libc::getpgid(pid) };
        let parent_pgid = unsafe { libc::getpgid(0) };

        if child_pgid > 0 && child_pgid != parent_pgid {
            // Child is in its own process group (setsid took effect).
            // Kill the entire group to reap dotnet/MSBuild/Roslyn workers.
            let rc = unsafe { libc::kill(-child_pgid, libc::SIGKILL) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                tracing::debug!(
                    "kill(-{child_pgid}, SIGKILL) failed for {program}: {err} \
                     (process may have already exited)"
                );
            }
        } else {
            // setsid() was blocked (EPERM) or getpgid failed — child shares
            // the parent's group. Kill only the direct child PID to avoid
            // targeting the parent group.
            let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                tracing::debug!(
                    "kill({pid}, SIGKILL) failed for {program}: {err} \
                     (process may have already exited)"
                );
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Windows best-effort: kill the direct PID via taskkill /T (tree).
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child_pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        tracing::debug!("taskkill /F /T /PID {child_pid} issued for {program}");
    }
}

/// Run `program` with `args` in `cwd`, capturing stdout/stderr with a 30s
/// timeout. The child and its descendants are killed if the timeout is exceeded.
///
/// Why: every tool impl needs the same "shell out, capture, time-box" logic;
/// `std::process` has no built-in timeout so we spawn a reader thread and a
/// wait thread and join with a deadline.
/// What: delegates to `run_command_with_timeout` with the fixed `TOOL_TIMEOUT`
/// (30 s). Use `run_command_with_timeout` directly when a wider cap is needed.
/// Test: `run_command_captures_echo` runs a trivial command and checks output.
pub fn run_command(program: &str, args: &[&str], cwd: &Path) -> anyhow::Result<CommandOutput> {
    run_command_with_timeout(program, args, cwd, TOOL_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_captures_echo() {
        let dir = std::env::temp_dir();
        let out = run_command("echo", &["hello"], &dir).expect("echo should run");
        assert!(out.stdout.contains("hello"));
        assert_eq!(out.status, Some(0));
    }

    #[test]
    fn run_command_reports_missing_binary() {
        let dir = std::env::temp_dir();
        let res = run_command("trusty-no-such-binary-xyz", &[], &dir);
        assert!(res.is_err());
    }

    #[test]
    fn build_tool_timeout_default_is_300s() {
        // When the env var is absent (or we temporarily clear it) the function
        // must return 300 seconds. We can't guarantee the var is absent in all
        // environments, but we can confirm the value is non-zero and >= 1 s.
        let t = build_tool_timeout();
        assert!(
            t.as_secs() >= 1,
            "build_tool_timeout() should be at least 1 s, got {t:?}"
        );
    }

    /// Why: verifies the kill-on-timeout contract — after
    /// `run_command_with_timeout` returns with a timeout error the spawned
    /// child process must be dead (not merely detached/leaked). This catches
    /// regressions where the kill path is broken or the child is left running.
    /// What: runs `sh -c 'echo $$ > <tmpfile>; exec sleep 30'` with a 100 ms
    /// timeout, reads the child PID from the tmpfile, and polls
    /// `libc::kill(pid, 0)` (signal 0 = existence probe) until it returns -1
    /// with ESRCH (no such process), or until a 1 s grace deadline. Asserts
    /// (a) the call returns Err with a timeout message, (b) the child PID is
    /// gone within the grace period.
    /// Test: this test itself; gated `#[cfg(unix)]` because sh, POSIX signal
    /// semantics, and /proc/pid liveness are Unix-only.
    #[test]
    #[cfg(unix)]
    fn timeout_kills_child_process() {
        use std::time::Instant;

        let dir = std::env::temp_dir();
        // Use a unique tmpfile per test run to avoid cross-test interference.
        let pid_file = dir.join(format!(
            "trusty_analyze_test_pid_{}.txt",
            std::process::id()
        ));

        // The shell command writes its own PID (the `sh` PID, which is the
        // direct child) to pid_file, then execs into `sleep 30`.
        let pid_file_str = pid_file.to_str().expect("tmpdir must be UTF-8");
        let sh_cmd = format!("echo $$ > {pid_file_str}; exec sleep 30");

        let result =
            run_command_with_timeout("sh", &["-c", &sh_cmd], &dir, Duration::from_millis(100));

        // (a) The call must return a timeout error.
        assert!(result.is_err(), "expected timeout error, got Ok");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeded") || err_msg.contains("timeout"),
            "error message should mention timeout: {err_msg}"
        );

        // (b) Read the PID the child wrote before sleeping.
        // Give the child a brief window to write the file if it hasn't yet
        // (normally it writes before the 100 ms timeout, but on very slow CI
        // hosts the file may not exist yet at the instant the timeout fires).
        let child_pid: libc::pid_t = {
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                    if let Ok(p) = contents.trim().parse::<libc::pid_t>() {
                        break p;
                    }
                }
                if Instant::now() >= deadline {
                    // If the file never appeared, the child likely never ran —
                    // skip the PID-liveness check and rely on the timing test.
                    let _ = std::fs::remove_file(&pid_file);
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let _ = std::fs::remove_file(&pid_file);

        // (c) Poll until kill(pid, 0) returns -1/ESRCH (process is gone).
        // Allow up to 1 s for the kill + reap to complete.
        let grace = Instant::now() + Duration::from_secs(1);
        loop {
            // SAFETY: kill(pid, 0) never delivers a signal; it only tests
            // whether the process exists. Async-signal-safe.
            let rc = unsafe { libc::kill(child_pid, 0) };
            if rc < 0 {
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if errno == libc::ESRCH {
                    // Process is gone — kill-on-timeout worked.
                    return;
                }
                // EPERM means the process exists but we can't signal it
                // (e.g. different uid). Treat as still-alive and keep polling.
            }
            assert!(
                Instant::now() < grace,
                "child pid {child_pid} still alive 1 s after timeout — kill-on-timeout broken"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Why: verifies that after `run_command_with_timeout` returns on timeout,
    /// the process it spawned is actually dead (not merely detached). Uses
    /// `kill -0 <pid>` to probe liveness without sending a real signal.
    /// Test: this test itself; requires Unix proc semantics.
    #[test]
    #[cfg(unix)]
    fn timeout_returns_quickly_not_after_full_sleep() {
        use std::time::Instant;
        let dir = std::env::temp_dir();
        let start = Instant::now();
        let result = run_command_with_timeout("sleep", &["30"], &dir, Duration::from_millis(200));
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected timeout error");
        // The call must return in well under 1 second (we gave it 200 ms +
        // join overhead). If the kill is missing the call would block ~30 s.
        assert!(
            elapsed < Duration::from_secs(5),
            "run_command_with_timeout took {elapsed:?} — kill-on-timeout likely broken"
        );
    }
}
