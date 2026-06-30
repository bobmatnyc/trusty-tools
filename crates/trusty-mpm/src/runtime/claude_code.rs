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
/// `--dangerously-skip-permissions` keeps the unattended orchestration session
/// from blocking on per-tool permission prompts (issue #1269). The isolation flags reuse the
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

/// Build a resume-aware Claude Code command (#1744, #1840).
///
/// Why: `resume_managed` must restore the prior conversation when one is known.
/// `--resume <id>` restores the exact Claude Code conversation identified by
/// `claude_session_id`; `--continue` resumes the most-recent conversation in
/// the workspace when the id is absent AND prior conversation history exists;
/// neither flag is passed when there is no prior conversation, preventing the
/// "No conversation found to continue" error that would otherwise drop the
/// session to a bare shell (#1840).
/// What: `Some(id)` → `--resume <id>`. `None, has_prior_conv=true` → `--continue`.
/// `None, has_prior_conv=false` → plain spawn (no resume flag), so the session
/// starts a fresh conversation instead of erroring. All three share the same
/// env-scrub and isolation flags as [`spawn_command`].
/// Test: `resume_command_with_id_uses_resume_flag`,
/// `resume_command_without_id_with_prior_conv_uses_continue`,
/// `resume_command_without_id_no_prior_conv_uses_plain_spawn`.
fn resume_command(
    claude_bin: &str,
    claude_session_id: Option<&str>,
    has_prior_conv: bool,
) -> String {
    let base = format!(
        "env -u ANTHROPIC_API_KEY {} {} {}",
        claude_bin,
        crate::core::model_inject::SETTING_SOURCES_FLAG,
        crate::core::model_inject::PERMISSION_MODE_FLAG,
    );
    match claude_session_id {
        Some(id) => format!("{base} --resume {id}"),
        None if has_prior_conv => format!("{base} --continue"),
        None => base, // No prior conversation: start fresh to avoid "no conversation found".
    }
}

/// Detect whether Claude Code has prior conversation history for `cwd` (#1840).
///
/// Why: `claude --continue` exits with "No conversation found to continue" when
/// no prior conversation exists for the workspace, immediately dropping to a
/// bare shell. This guard prevents that error by only emitting `--continue`
/// when evidence of a prior conversation exists.
/// What: Claude Code stores per-project conversations in
/// `~/.claude/projects/<encoded-path>/` where `<encoded-path>` is the absolute
/// workspace path with `/` replaced by `-`. Returns `true` when that directory
/// exists and contains at least one `.jsonl` file.
/// Test: `has_prior_conversation_returns_false_for_fresh_workspace`.
fn has_prior_conversation(cwd: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.is_dir() {
        return false;
    }
    // Claude Code encodes the workspace path by replacing every '/' with '-'.
    // A leading '/' becomes '-', so /private/tmp/foo → -private-tmp-foo.
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = projects_dir.join(&encoded);
    project_dir.is_dir()
        && std::fs::read_dir(&project_dir)
            .map(|d| {
                d.flatten()
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            })
            .unwrap_or(false)
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

    /// Resume Claude Code with conversation continuity (#1744, #1840).
    ///
    /// Why: `resume_managed` must restore the prior conversation rather than
    /// starting fresh. If the stored `claude_session_id` is available,
    /// `--resume <id>` restores the exact conversation; when absent AND prior
    /// conversation history is detected (via [`has_prior_conversation`]),
    /// `--continue` resumes the most-recent conversation; when neither applies,
    /// a plain spawn is used to avoid the "No conversation found to continue"
    /// error that otherwise drops the session to a bare shell (#1840).
    /// What: resolves the claude binary, pre-seeds home trust, checks for prior
    /// conversation when `claude_session_id` is `None`, then sends the
    /// appropriate [`resume_command`] to the tmux pane.
    /// Test: `spawn_resume_with_id_uses_resume_flag`,
    /// `spawn_resume_without_id_no_prior_conv_sends_plain_spawn`.
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
        if let Some(id) = claude_session_id {
            tracing::warn!(
                session = %tmux_name,
                claude_session_id = %id,
                "resuming with --resume <id>; if conversation no longer exists on \
                 disk, Claude Code will error — the reap loop will detect and mark \
                 Stopped within ~60 s (#1744)"
            );
        }
        // #1840: only use --continue when prior conversation history exists for cwd.
        // Without this guard, --continue fails with "No conversation found" for
        // sessions that were stopped before claude ever ran (e.g. provisioning error,
        // or a fresh worktree that was never used).
        // Compute file_history separately so the debug log reflects the actual
        // filesystem check result rather than the combined (id || file) value.
        let file_history = claude_session_id.is_none() && has_prior_conversation(cwd);
        let prior = claude_session_id.is_some() || file_history;
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            resume = claude_session_id.is_some(),
            has_prior_conv = file_history, // reflects actual .jsonl file check, not the combined value
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
            .send_line(
                tmux_name,
                &resume_command(&claude_bin, claude_session_id, prior),
            )
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
        // Why: the session_manager spawn path must isolate from the user's global
        // settings and run fully unattended (bypass all permission prompts) so
        // multi-agent orchestration never stalls on interactive approval dialogs.
        let cmd = spawn_command("claude");
        assert!(
            cmd.contains("--setting-sources project,local"),
            "spawn command must isolate settings: {cmd}"
        );
        assert!(
            cmd.contains("--dangerously-skip-permissions"),
            "spawn command must bypass permissions for unattended orchestration: {cmd}"
        );
        // Must not carry the old acceptEdits flag.
        assert!(
            !cmd.contains("acceptEdits"),
            "old acceptEdits flag must not be present: {cmd}"
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
        let cmd = resume_command("claude", Some("abc-123"), false);
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
    fn resume_command_without_id_with_prior_conv_uses_continue() {
        // Why (#1744 / #1840): when no claude_session_id is stored but prior
        // conversation history exists, --continue resumes the most-recent
        // conversation in the workspace rather than starting fresh.
        let cmd = resume_command("claude", None, true);
        assert!(
            cmd.contains("--continue"),
            "resume command without id + prior conv must use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "resume command without id must NOT contain --resume: {cmd}"
        );
    }

    #[test]
    fn resume_command_without_id_no_prior_conv_uses_plain_spawn() {
        // Why (#1840): when no claude_session_id is stored AND no prior
        // conversation exists, --continue would error with "No conversation
        // found to continue". The plain spawn starts a fresh session instead.
        let cmd = resume_command("claude", None, false);
        assert!(
            !cmd.contains("--continue"),
            "resume command without id + no prior conv must NOT use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "resume command without id must NOT use --resume: {cmd}"
        );
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "plain spawn command must still scrub API key: {cmd}"
        );
    }

    #[test]
    fn has_prior_conversation_returns_false_for_fresh_workspace() {
        // Why (#1840): a fresh worktree or temp directory has no Claude
        // conversation history; has_prior_conversation must return false so
        // spawn_resume avoids the "No conversation found to continue" error.
        // We isolate HOME so the test does not read the CI runner's actual
        // ~/.claude/projects/ — on Unix, dirs::home_dir() reads the HOME env var.
        let tmp_home = tempfile::tempdir().expect("tempdir for HOME");
        let tmp_cwd = tempfile::tempdir().expect("tempdir for cwd");
        // SAFETY: modifying HOME is inherently thread-unsafe; this test is designed
        // to run in isolation. In practice cargo test does not parallelize tests
        // within a single module by default, and no other test in this module
        // modifies HOME.
        let old_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", tmp_home.path());
        }
        let result = has_prior_conversation(tmp_cwd.path());
        // Restore HOME regardless of the assertion outcome.
        match old_home {
            Some(h) => unsafe {
                std::env::set_var("HOME", &h);
            },
            None => unsafe {
                std::env::remove_var("HOME");
            },
        }
        assert!(
            !result,
            "fresh workspace with isolated HOME must have no prior conversation (cwd={:?})",
            tmp_cwd.path()
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
    fn spawn_resume_without_id_no_prior_conv_sends_plain_spawn() {
        // Why (#1840): when no claude_session_id is available AND the workspace
        // has no prior conversation, spawn_resume must send a plain spawn
        // (no --continue, no --resume) to avoid "No conversation found to continue".
        if ClaudeCodeAdapter::resolve_claude().is_none() {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        // Use the fresh temp dir as cwd — it has no Claude conversation history.
        adapter
            .spawn_resume("test-tmux-session", tmp.path(), "task", None)
            .expect("spawn_resume without id");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            !sends[0].1.contains("--continue"),
            "spawn_resume without id + no prior conv must NOT use --continue: {}",
            sends[0].1
        );
        assert!(
            !sends[0].1.contains("--resume"),
            "spawn_resume without id must NOT use --resume: {}",
            sends[0].1
        );
    }
}
