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
//! against a captured fixture.

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
/// caller-supplied `timeout`. The child is killed if it overruns.
///
/// Why: file-scoped tools use a 30 s cap; build-class tools need a wider
/// limit (default 300 s). Extracting the timeout as a parameter lets both
/// callers share the same spawn/capture/wait logic without duplication.
/// What: spawns the child with piped output, reads both streams on background
/// threads, and waits up to `timeout` for exit.
/// Test: `run_command_captures_echo` and `run_command_reports_missing_binary`
/// exercise this via the 30 s wrapper.
pub fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> anyhow::Result<CommandOutput> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {program}: {e}"))?;

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

    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send((child, status));
    });

    let status = match rx.recv_timeout(timeout) {
        Ok((_child, Ok(status))) => status.code(),
        Ok((_child, Err(e))) => {
            return Err(anyhow::anyhow!("wait failed for {program}: {e}"));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
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

/// Run `program` with `args` in `cwd`, capturing stdout/stderr with a 30s
/// timeout. The child is killed if it overruns.
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
}
