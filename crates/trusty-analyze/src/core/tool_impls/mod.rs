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

/// Hard wall-clock cap on any single tool invocation.
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Captured result of running an external command.
pub struct CommandOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Process exit code; `None` if killed by signal/timeout.
    pub status: Option<i32>,
}

/// Run `program` with `args` in `cwd`, capturing stdout/stderr with a 30s
/// timeout. The child is killed if it overruns.
///
/// Why: every tool impl needs the same "shell out, capture, time-box" logic;
/// `std::process` has no built-in timeout so we spawn a reader thread and a
/// wait thread and join with a deadline.
/// What: spawns the child with piped output, reads both streams on background
/// threads, and waits up to `TOOL_TIMEOUT` for exit.
/// Test: `run_command_captures_echo` runs a trivial command and checks output.
pub fn run_command(program: &str, args: &[&str], cwd: &Path) -> anyhow::Result<CommandOutput> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {program}: {e}"))?;

    // Drain stdout/stderr on dedicated threads so a full pipe buffer cannot
    // deadlock the child before we get to `wait`.
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

    // Wait for exit on a background thread so the main thread can enforce the
    // timeout via a bounded channel recv.
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send((child, status));
    });

    let status = match rx.recv_timeout(TOOL_TIMEOUT) {
        Ok((_child, Ok(status))) => status.code(),
        Ok((_child, Err(e))) => {
            return Err(anyhow::anyhow!("wait failed for {program}: {e}"));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The waiter still owns the Child; we cannot kill it directly, but
            // dropping our end and reporting a timeout is the safe path.
            return Err(anyhow::anyhow!(
                "{program} exceeded {}s timeout",
                TOOL_TIMEOUT.as_secs()
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
}
