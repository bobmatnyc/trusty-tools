//! trusty-code (`tcode`) runtime adapter.
//!
//! Why: alongside the OAuth-based Claude Code runtime, operators need a
//! lightweight, API-key-based backend for cost-sensitive deployments. `tcode`
//! (the trusty-code CLI) talks directly to the Anthropic API using the
//! `ANTHROPIC_API_KEY` from the environment — the opposite of `ClaudeCodeAdapter`,
//! which *scrubs* that key so Claude Code falls back to OAuth.
//! What: [`TcodeAdapter`] wraps a [`ManagedTmuxDriver`] and implements
//! [`RuntimeAdapter`]; `spawn` builds a `tcode run-task` command rooted at the
//! workspace and sends it to the named tmux pane after verifying the `tcode`
//! binary is on PATH. The `ANTHROPIC_API_KEY` is intentionally preserved.
//! Test: `tcode_adapter_identifies`, `tcode_build_spawn_command_*`,
//! `tcode_adapter_spawn_sends_run_task` (see the module test block).

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tracing::debug;

use crate::session_manager::ManagedTmuxDriver;

use super::RuntimeAdapter;
use super::RuntimeError;

/// The `tcode` binary name expected on PATH.
///
/// Why: centralises the binary name so the availability check and the spawn
/// command never drift apart.
/// What: literal `"tcode"` — the binary produced by the `trusty-code` crate
/// (`[[bin]] name = "tcode"`).
/// Test: `tcode_build_spawn_command_starts_with_binary`.
const TCODE_BINARY: &str = "tcode";

/// Default agent name `tcode run-task` is invoked with when the operator does
/// not pin one.
///
/// Why: `tcode run-task <agent> <task> --project <cwd>` requires an agent name;
/// `engineer` is the general-purpose implementation agent shipped in every
/// `.claude/agents/` directory, making it the safe, predictable default for a
/// freshly provisioned managed session.
/// What: literal `"engineer"`.
/// Test: `tcode_build_spawn_command_uses_default_agent`.
const DEFAULT_AGENT: &str = "engineer";

/// Runtime adapter that launches the `tcode` CLI inside a tmux session.
///
/// Why: provides the direct-Anthropic-API alternative to `ClaudeCodeAdapter`;
/// coupling the launch sequence (binary check, command construction, tmux send)
/// to a typed adapter keeps the session manager free of runtime-specific logic.
/// What: holds a [`ManagedTmuxDriver`] reference; `spawn` verifies the `tcode`
/// binary exists, builds a `tcode run-task <agent> <task> --project <cwd>`
/// command, and sends it to the named pane. Unlike the Claude Code adapter it
/// does NOT scrub `ANTHROPIC_API_KEY` — `tcode` needs it for the API-key path.
/// Test: `tcode_adapter_identifies`, `tcode_adapter_spawn_sends_run_task`.
pub struct TcodeAdapter {
    tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
}

impl TcodeAdapter {
    /// Construct an adapter backed by the given tmux driver.
    ///
    /// Why: the session manager injects the tmux driver via `Arc<dyn …>` so the
    /// adapter is testable without a real tmux binary.
    /// What: stores the driver reference.
    /// Test: used in every `TcodeAdapter` test.
    pub fn new(tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>) -> Self {
        Self { tmux }
    }

    /// Return `true` if the `tcode` binary can be found on `PATH`.
    ///
    /// Why: `spawn` must return `RuntimeError::BinaryNotFound` rather than
    /// sending a command that silently fails inside the pane.
    /// What: runs `which tcode` and checks the exit status.
    /// Test: `tcode_adapter_binary_check_returns_bool`.
    fn tcode_available() -> bool {
        Command::new("which")
            .arg(TCODE_BINARY)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Build the shell line sent to the tmux pane to start `tcode` for `task` at `cwd`.
///
/// Why: command construction is the single most regression-prone part of the
/// adapter (quoting, project flag, agent selection), so it is factored into a
/// pure function that can be unit-tested without spawning a process or tmux.
/// What: returns `tcode run-task <DEFAULT_AGENT> '<task>' --project '<cwd>'`,
/// single-quoting the task and project path so spaces and shell metacharacters
/// in either are passed literally. Embedded single quotes are escaped using the
/// POSIX `'\''` idiom. The `ANTHROPIC_API_KEY` is deliberately NOT scrubbed —
/// `tcode` uses it for the direct-API path.
/// Test: `tcode_build_spawn_command_*` in the module test block.
fn build_spawn_command(cwd: &Path, task: &str) -> String {
    format!(
        "{TCODE_BINARY} run-task {DEFAULT_AGENT} {} --project {}",
        shell_single_quote(task),
        shell_single_quote(&cwd.to_string_lossy()),
    )
}

/// Wrap `value` in single quotes, escaping any embedded single quotes.
///
/// Why: the task description and workspace path are operator/agent-supplied and
/// may contain spaces or shell metacharacters; single-quoting prevents the tmux
/// pane shell from word-splitting or interpreting them.
/// What: returns `'value'` with each embedded `'` replaced by the POSIX-safe
/// sequence `'\''` (close-quote, escaped-quote, re-open-quote).
/// Test: `tcode_shell_quote_escapes_embedded_quote`.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl RuntimeAdapter for TcodeAdapter {
    /// Launch `tcode` in the named tmux session.
    ///
    /// Why: the session manager calls this after creating the tmux pane so the
    /// `tcode` agent process starts inside it, rooted at the workspace.
    /// What: checks that `tcode` is on PATH (returns `BinaryNotFound` if not),
    /// builds the `tcode run-task` command via [`build_spawn_command`], and
    /// sends it to the pane. `ANTHROPIC_API_KEY` is preserved (API-key path).
    /// Test: `tcode_adapter_spawn_sends_run_task`.
    fn spawn(&self, tmux_name: &str, cwd: &Path, task: &str) -> Result<(), RuntimeError> {
        if !Self::tcode_available() {
            return Err(RuntimeError::BinaryNotFound(
                "tcode binary not found on PATH — install trusty-code first".into(),
            ));
        }
        let command = build_spawn_command(cwd, task);
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            "spawning tcode in tmux pane"
        );
        self.tmux
            .send_line(tmux_name, &command)
            .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))
    }

    /// Return `"tcode"` as the adapter's identifier.
    ///
    /// Why: logs and status responses must identify this adapter by name so
    /// operators can distinguish it from the `claude-code` runtime.
    /// What: static string, no I/O.
    /// Test: `tcode_adapter_identifies`.
    fn identify(&self) -> &str {
        "tcode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::ManagedError;
    use std::sync::Mutex;

    struct FakeTmux {
        sends: Mutex<Vec<(String, String)>>,
    }

    impl FakeTmux {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sends: Mutex::new(Vec::new()),
            })
        }
    }

    impl ManagedTmuxDriver for FakeTmux {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }

        fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
            Ok(())
        }

        fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
            self.sends
                .lock()
                .unwrap()
                .push((name.to_owned(), text.to_owned()));
            Ok(())
        }

        fn capture(&self, _name: &str, _lines: u32) -> Result<String, ManagedError> {
            Ok(String::new())
        }

        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(Vec::new())
        }

        fn session_exists(&self, _name: &str) -> bool {
            false
        }
    }

    #[test]
    fn tcode_adapter_identifies() {
        let fake = FakeTmux::new();
        let adapter = TcodeAdapter::new(fake);
        assert_eq!(adapter.identify(), "tcode");
    }

    #[test]
    fn tcode_build_spawn_command_starts_with_binary() {
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "do a thing");
        assert!(
            cmd.starts_with("tcode run-task "),
            "command must invoke the tcode binary: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_uses_default_agent() {
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "do a thing");
        assert!(
            cmd.contains(&format!("run-task {DEFAULT_AGENT} ")),
            "command must target the default agent: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_includes_project_and_task() {
        let cmd = build_spawn_command(Path::new("/work/space dir"), "fix bug #12");
        assert!(
            cmd.contains("--project '/work/space dir'"),
            "command must root tcode at the (quoted) workspace cwd: {cmd}"
        );
        assert!(
            cmd.contains("'fix bug #12'"),
            "command must carry the (quoted) task description: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_does_not_scrub_api_key() {
        // The API-key path REQUIRES the key in the child env; the tcode adapter
        // must NOT prepend `env -u ANTHROPIC_API_KEY` like the claude adapter does.
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "task");
        assert!(
            !cmd.contains("ANTHROPIC_API_KEY"),
            "tcode adapter must preserve ANTHROPIC_API_KEY (no env scrub): {cmd}"
        );
        assert!(
            !cmd.contains("env -u"),
            "tcode adapter must not scrub the environment: {cmd}"
        );
    }

    #[test]
    fn tcode_shell_quote_escapes_embedded_quote() {
        // A task containing a single quote must remain a single shell token.
        let quoted = shell_single_quote("it's broken");
        assert_eq!(quoted, "'it'\\''s broken'");
    }

    #[test]
    fn tcode_adapter_binary_check_returns_bool() {
        // Asserts the function returns without panicking; `tcode` may or may not
        // be on PATH in CI so we do not assert the result.
        let _ = TcodeAdapter::tcode_available();
    }

    #[test]
    fn tcode_adapter_spawn_sends_run_task() {
        // If tcode is not installed this test is a no-op (we cannot install it
        // in CI). We only assert the send when the binary exists.
        if !TcodeAdapter::tcode_available() {
            return;
        }
        let fake = FakeTmux::new();
        let adapter = TcodeAdapter::new(fake.clone());
        adapter
            .spawn("tmpm-test", Path::new("/tmp"), "some task")
            .expect("spawn");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "tmpm-test");
        assert_eq!(
            sends[0].1,
            build_spawn_command(Path::new("/tmp"), "some task")
        );
    }
}
