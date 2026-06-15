//! Claude Code runtime adapter.
//!
//! Why: the session manager must have a concrete adapter that launches the
//! `claude` CLI inside a tmux session without leaking `ANTHROPIC_API_KEY` into
//! the pane environment; `env -u ANTHROPIC_API_KEY claude` achieves that.
//! What: [`ClaudeCodeAdapter`] wraps a [`ManagedTmuxDriver`] and implements
//! [`RuntimeAdapter`]; `spawn` sends the env-scrubbed command to the tmux pane
//! after verifying the `claude` binary is on PATH.
//! Test: `claude_code_adapter_spawn_sends_env_scrub_command`,
//! `claude_code_adapter_identifies`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tracing::debug;

use crate::session_manager::ManagedTmuxDriver;

use super::RuntimeAdapter;
use super::RuntimeError;

/// The shell command sent to the tmux pane to start Claude Code.
///
/// Why: the `env -u ANTHROPIC_API_KEY` prefix strips the API key from the
/// child environment so Claude Code falls back to its OAuth login in
/// `~/.claude.json`, preventing accidental key leakage through pane history.
/// The `--setting-sources project,local` flag isolates the session from the
/// operator's interfering global `~/.claude/settings.json` hooks (issue #1269 /
/// step 4) without touching `~/.claude.json` (so OAuth is preserved), and
/// `--permission-mode acceptEdits` keeps the unattended session from blocking on
/// per-tool permission prompts (issue #1269). The isolation flags reuse the
/// shared [`crate::core::model_inject::SETTING_SOURCES_FLAG`] /
/// [`crate::core::model_inject::PERMISSION_MODE_FLAG`] constants so this spawn
/// path and the CLI launch path can never drift.
/// What: built once at first use from `env -u ANTHROPIC_API_KEY claude` plus the
/// two shared flag constants; piped to `tmux send-keys … Enter`.
/// Test: `spawn_command_contains_env_scrub`,
/// `spawn_command_contains_isolation_flags`.
fn spawn_command() -> String {
    format!(
        "env -u ANTHROPIC_API_KEY claude {} {}",
        crate::core::model_inject::SETTING_SOURCES_FLAG,
        crate::core::model_inject::PERMISSION_MODE_FLAG,
    )
}

/// Runtime adapter that launches Claude Code CLI inside a tmux session.
///
/// Why: Claude Code is the primary agent runtime for MPM sessions; coupling the
/// launch sequence (binary check, env scrub, tmux send) to a typed adapter keeps
/// the session manager free of runtime-specific knowledge.
/// What: holds a [`ManagedTmuxDriver`] reference; `spawn` verifies the `claude`
/// binary exists, then sends `env -u ANTHROPIC_API_KEY claude` to the named pane.
/// Test: `claude_code_adapter_spawn_sends_env_scrub_command`,
/// `claude_code_adapter_identifies`.
pub struct ClaudeCodeAdapter {
    tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
}

impl ClaudeCodeAdapter {
    /// Construct an adapter backed by the given tmux driver.
    ///
    /// Why: the session manager injects the tmux driver via `Arc<dyn …>` so
    /// the adapter is testable without a real tmux binary.
    /// What: stores the driver reference.
    /// Test: used in every `ClaudeCodeAdapter` test.
    pub fn new(tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>) -> Self {
        Self { tmux }
    }

    /// Return `true` if the `claude` binary can be found on `PATH`.
    ///
    /// Why: `spawn` must return `RuntimeError::BinaryNotFound` rather than
    /// sending a command that silently fails inside the pane.
    /// What: runs `which claude` and checks the exit status.
    /// Test: `claude_code_adapter_binary_check`.
    fn claude_available() -> bool {
        Command::new("which")
            .arg("claude")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl RuntimeAdapter for ClaudeCodeAdapter {
    /// Launch Claude Code in the named tmux session.
    ///
    /// Why: the session manager calls this after creating the tmux pane so the
    /// actual agent process starts inside it.
    /// What: checks that `claude` is on PATH (returns `BinaryNotFound` if not),
    /// then sends [`spawn_command`] (`env -u ANTHROPIC_API_KEY claude` plus the
    /// isolation/permission flags) to the pane; the task is logged for
    /// observability but not passed to the command (Claude Code reads
    /// instructions from CLAUDE.md or an interactive prompt).
    /// Test: `spawn_sends_env_scrub_when_binary_available`.
    fn spawn(&self, tmux_name: &str, cwd: &Path, task: &str) -> Result<(), RuntimeError> {
        if !Self::claude_available() {
            return Err(RuntimeError::BinaryNotFound(
                "claude binary not found on PATH — install Claude Code first".into(),
            ));
        }
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            "spawning claude-code in tmux pane"
        );
        self.tmux
            .send_line(tmux_name, &spawn_command())
            .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))
    }

    /// Return `"claude-code"` as the adapter's identifier.
    ///
    /// Why: logs and status responses must identify this adapter by name so
    /// operators can distinguish it from future runtimes.
    /// What: static string, no I/O.
    /// Test: `claude_code_adapter_identifies`.
    fn identify(&self) -> &str {
        "claude-code"
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::FakeTmux;
    use super::*;

    #[test]
    fn claude_code_adapter_identifies() {
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake);
        assert_eq!(adapter.identify(), "claude-code");
    }

    #[test]
    fn spawn_command_contains_env_scrub() {
        // The spawn command must always strip the API key from the environment
        // so the session falls back to OAuth in ~/.claude.json.
        let cmd = spawn_command();
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "spawn command must contain env scrub: {cmd}"
        );
        assert!(
            cmd.contains(" claude "),
            "spawn command must invoke claude: {cmd}"
        );
    }

    #[test]
    fn spawn_command_contains_isolation_flags() {
        // Why (#1269): the session_manager spawn path must isolate from the
        // user's global settings and run unattended, exactly like the CLI path.
        let cmd = spawn_command();
        assert!(
            cmd.contains("--setting-sources project,local"),
            "spawn command must isolate settings: {cmd}"
        );
        assert!(
            cmd.contains("--permission-mode acceptEdits"),
            "spawn command must set unattended permission mode: {cmd}"
        );
    }

    #[test]
    fn claude_code_adapter_binary_check_returns_bool() {
        // Just assert the function returns without panicking; we don't assert
        // the result because `claude` may or may not be on PATH in CI.
        let _ = ClaudeCodeAdapter::claude_available();
    }

    #[test]
    fn spawn_sends_env_scrub_when_binary_available() {
        // Patch: if claude is not available this test is a no-op (we cannot
        // install it in CI). We only assert the send when the binary exists.
        if !ClaudeCodeAdapter::claude_available() {
            return;
        }
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn("tmpm-test", Path::new("/tmp"), "some task")
            .expect("spawn");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "tmpm-test");
        assert_eq!(sends[0].1, spawn_command());
    }
}
