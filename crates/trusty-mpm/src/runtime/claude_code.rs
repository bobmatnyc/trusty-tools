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
/// What: built from `env -u ANTHROPIC_API_KEY <claude_bin>` plus the two shared
/// flag constants; piped to `tmux send-keys … Enter`. `claude_bin` is the
/// resolved binary — an absolute path under launchd so the pane (which inherits
/// the daemon's minimal `PATH`) does not need `claude` on its own `PATH` (#1298).
/// Test: `spawn_command_contains_env_scrub`,
/// `spawn_command_contains_isolation_flags`,
/// `spawn_command_uses_resolved_binary`.
fn spawn_command(claude_bin: &str) -> String {
    format!(
        "env -u ANTHROPIC_API_KEY {} {} {}",
        claude_bin,
        crate::core::model_inject::SETTING_SOURCES_FLAG,
        crate::core::model_inject::PERMISSION_MODE_FLAG,
    )
}

/// Build a resume-aware Claude Code command.
///
/// Why (#1744): `resume_managed` must restore the prior conversation when one
/// is known. `--resume <id>` restores the exact Claude Code conversation
/// identified by `claude_session_id`; `--continue` resumes the most-recent
/// conversation in the workspace when the id is absent (graceful degradation);
/// neither flag is passed on a fresh spawn. All three share the same env-scrub
/// and isolation flags as [`spawn_command`] so they are indistinguishable from
/// the daemon's perspective except for the resume mode.
/// What: given the resolved `claude_bin` and an optional `claude_session_id`,
/// returns the full shell command string. `Some(id)` → `--resume <id>`.
/// `None` → `--continue`.
/// Test: `resume_command_with_id_uses_resume_flag`,
/// `resume_command_without_id_uses_continue`.
fn resume_command(claude_bin: &str, claude_session_id: Option<&str>) -> String {
    let base = format!(
        "env -u ANTHROPIC_API_KEY {} {} {}",
        claude_bin,
        crate::core::model_inject::SETTING_SOURCES_FLAG,
        crate::core::model_inject::PERMISSION_MODE_FLAG,
    );
    match claude_session_id {
        Some(id) => format!("{base} --resume {id}"),
        None => format!("{base} --continue"),
    }
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

    /// Resolve the `claude` binary to an absolute path, or `None` if missing.
    ///
    /// Why: under launchd the daemon (and the tmux pane it spawns) inherits a
    /// minimal `PATH` that omits `~/.local/bin` where Claude Code installs, so a
    /// bare `claude` on the pane would fail to launch (spawn `[errored]`, #1298).
    /// Resolving to an absolute path here lets the spawn command invoke claude
    /// by full path, independent of the pane's `PATH`.
    /// What: delegates to [`trusty_common::bin_resolve::resolve_binary`] which
    /// checks the live `PATH` first then the well-known daemon dirs (Homebrew +
    /// `~/.local/bin` + `~/.cargo/bin`); returns the resolved path as a `String`.
    /// Test: `claude_code_adapter_binary_check_returns_option`.
    fn resolve_claude() -> Option<String> {
        trusty_common::bin_resolve::resolve_binary("claude")
            .and_then(|p| p.to_str().map(str::to_owned))
    }
}

impl RuntimeAdapter for ClaudeCodeAdapter {
    /// Launch Claude Code in the named tmux session.
    ///
    /// Why: the session manager calls this after creating the tmux pane so the
    /// actual agent process starts inside it.
    /// What: resolves `claude` to an absolute path (returns `BinaryNotFound`
    /// if it cannot be found on `PATH` or in the well-known daemon dirs), then
    /// sends [`spawn_command`] (`env -u ANTHROPIC_API_KEY <abs-claude>` plus the
    /// isolation/permission flags) to the pane; the task is logged for
    /// observability but not passed to the command (Claude Code reads
    /// instructions from CLAUDE.md or an interactive prompt).
    /// Test: `spawn_sends_env_scrub_when_binary_available`.
    fn spawn(&self, tmux_name: &str, cwd: &Path, task: &str) -> Result<(), RuntimeError> {
        let claude_bin = Self::resolve_claude().ok_or_else(|| {
            RuntimeError::BinaryNotFound(
                "claude binary not found on PATH or in well-known dirs \
                 (e.g. ~/.local/bin) — install Claude Code first"
                    .into(),
            )
        })?;
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            "spawning claude-code in tmux pane"
        );
        // Best-effort: pre-seed workspace trust + renderer-upsell dismissal into
        // ~/.claude.json so the session starts without blocking startup prompts.
        // Non-fatal: a seed failure must never abort the spawn (closes #1696).
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(cwd) {
            tracing::warn!(
                session = %tmux_name,
                cwd = %cwd.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }
        self.tmux
            .send_line(tmux_name, &spawn_command(&claude_bin))
            .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))
    }

    /// Resume Claude Code with conversation continuity.
    ///
    /// Why (#1744): `resume_managed` must restore the prior conversation rather
    /// than starting fresh. If the stored `claude_session_id` is available,
    /// `--resume <id>` restores the exact conversation; otherwise `--continue`
    /// resumes the most-recent conversation in the workspace. Both paths share the
    /// same env-scrub and isolation flags as `spawn`.
    /// What: resolves the claude binary, pre-seeds home trust (same as `spawn`),
    /// then sends [`resume_command`] with the optional id to the tmux pane.
    /// Test: `spawn_resume_with_id_uses_resume_flag`,
    /// `spawn_resume_without_id_uses_continue_flag`.
    fn spawn_resume(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        claude_session_id: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let claude_bin = Self::resolve_claude().ok_or_else(|| {
            RuntimeError::BinaryNotFound(
                "claude binary not found on PATH or in well-known dirs \
                 (e.g. ~/.local/bin) — install Claude Code first"
                    .into(),
            )
        })?;
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            resume = claude_session_id.is_some(),
            "resuming claude-code in tmux pane"
        );
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(cwd) {
            tracing::warn!(
                session = %tmux_name,
                cwd = %cwd.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }
        self.tmux
            .send_line(tmux_name, &resume_command(&claude_bin, claude_session_id))
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
        let cmd = spawn_command("claude");
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
        let cmd = spawn_command("claude");
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
    fn spawn_command_uses_resolved_binary() {
        // Why (#1298): under launchd the pane inherits a minimal PATH, so the
        // spawn command must invoke claude by the resolved (absolute) path
        // rather than a bare `claude` that the pane's PATH cannot find.
        let cmd = spawn_command("/Users/me/.local/bin/claude");
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY /Users/me/.local/bin/claude "),
            "spawn command must invoke the resolved absolute claude path: {cmd}"
        );
    }

    #[test]
    fn claude_code_adapter_binary_check_returns_option() {
        // resolve_claude returns Some(path) or None without panicking; when it
        // resolves, the path must be a non-empty absolute-ish string.
        if let Some(p) = ClaudeCodeAdapter::resolve_claude() {
            assert!(!p.is_empty(), "resolved claude path must be non-empty");
        }
    }

    #[test]
    fn spawn_sends_env_scrub_when_binary_available() {
        // Patch: if claude is not available this test is a no-op (we cannot
        // install it in CI). We only assert the send when the binary exists.
        let Some(claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
            return;
        };
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn("tmpm-test", Path::new("/tmp"), "some task")
            .expect("spawn");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "tmpm-test");
        assert_eq!(sends[0].1, spawn_command(&claude_bin));
    }

    #[test]
    fn resume_command_with_id_uses_resume_flag() {
        // Why (#1744): --resume <id> restores the exact prior conversation;
        // the test pins the contract so accidental regressions are caught early.
        let cmd = resume_command("claude", Some("abc-123"));
        assert!(
            cmd.contains("--resume abc-123"),
            "resume command must include --resume <id>: {cmd}"
        );
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "resume command must still scrub API key: {cmd}"
        );
        assert!(
            !cmd.contains("--continue"),
            "resume command must NOT contain --continue when id is set: {cmd}"
        );
    }

    #[test]
    fn resume_command_without_id_uses_continue() {
        // Why (#1744): when no claude_session_id is stored, --continue resumes
        // the most-recent conversation in the workspace rather than starting fresh.
        let cmd = resume_command("claude", None);
        assert!(
            cmd.contains("--continue"),
            "resume command without id must use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "resume command without id must NOT contain --resume: {cmd}"
        );
    }

    #[test]
    fn spawn_resume_with_id_uses_resume_flag() {
        // Why (#1744): ClaudeCodeAdapter::spawn_resume must send --resume <id>
        // to the pane when the claude_session_id is known.
        if ClaudeCodeAdapter::resolve_claude().is_none() {
            return;
        };
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume(
                "tmpm-test",
                Path::new("/tmp"),
                "task",
                Some("my-session-id"),
            )
            .expect("spawn_resume");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            sends[0].1.contains("--resume my-session-id"),
            "spawn_resume with id must use --resume: {}",
            sends[0].1
        );
    }

    #[test]
    fn spawn_resume_without_id_uses_continue_flag() {
        // Why (#1744): ClaudeCodeAdapter::spawn_resume must send --continue
        // when no claude_session_id is available (graceful degradation).
        if ClaudeCodeAdapter::resolve_claude().is_none() {
            return;
        };
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume("tmpm-test", Path::new("/tmp"), "task", None)
            .expect("spawn_resume without id");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            sends[0].1.contains("--continue"),
            "spawn_resume without id must use --continue: {}",
            sends[0].1
        );
    }
}
