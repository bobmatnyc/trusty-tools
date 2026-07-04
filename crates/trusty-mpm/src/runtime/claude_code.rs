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

/// Single-quote a string for safe interpolation into the pane shell command.
///
/// Why: [`env_bin_prefix`] interpolates the resolved `CLAUDE_CONFIG_DIR` path into
/// a command string that `send_line` types into the tmux pane's live shell. An
/// UNQUOTED path containing a space — e.g. a macOS home `/Users/John Doe/…` —
/// word-splits, so `env` sees `CLAUDE_CONFIG_DIR=/Users/John` plus a stray
/// `Doe/…` argv entry; `claude` never execs and the pane silently dies with no
/// error surfaced anywhere. POSIX single-quoting disables ALL word-splitting,
/// globbing, and expansion, so any path survives intact.
/// What: wraps `s` in single quotes, escaping any embedded single quote with the
/// canonical close-reopen sequence `'\''` (a macOS home never contains a `'`, but
/// the escape keeps the quoting correct for arbitrary paths and is cheap).
/// Test: `env_bin_prefix_quotes_config_dir_with_space`,
/// `shell_single_quote_escapes_embedded_quote`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Compose the `env …` prefix + resolved binary that starts every managed
/// `claude`, wiring in the tm-owned `CLAUDE_CONFIG_DIR` when available (DOC-34).
///
/// Why: two invariants must hold on the pane command. (1) `-u ANTHROPIC_API_KEY`
/// strips the API key so Claude Code falls back to OAuth, preventing key leakage
/// through pane history. (2) When a managed `CLAUDE_CONFIG_DIR` is resolved,
/// `env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR='<dir>'` points the session at the tm-owned config home
/// (`~/.trusty-tools/trusty-mpm/claude-config/`). Under the retained
/// `--setting-sources project,local` flag (see [`spawn_command`]) this config
/// home does NOT supply the agent roster/skills/hooks — those load from the
/// project layer — but it DOES isolate the session's auth: with the API key
/// scrubbed, `claude` authenticates via the keychain/`.credentials.json` keyed to
/// this config-dir path (the tm-managed login), never touching the operator's
/// `~/.claude`. The path is SINGLE-QUOTED via [`shell_single_quote`] so a home
/// dir with a space does not word-split and break the pane. Both settings are
/// applied via a single `env` prefix, consistent with the scrub-only prefix.
/// What: `env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR='<dir>' <claude_bin>` (path
/// single-quoted) when `config_dir` is `Some`; the legacy
/// `env -u ANTHROPIC_API_KEY <claude_bin>` when `None` (home unresolved — no
/// config dir to point at). The `-u NAME` option MUST precede any `NAME=VALUE`
/// assignment per POSIX `env` grammar (`env [OPTION]... [NAME=VALUE]...
/// [COMMAND]...`); putting `CLAUDE_CONFIG_DIR=<dir>` before `-u` makes `env`
/// stop parsing options at the first `NAME=VALUE` and try to exec `-u` as a
/// command (`env: -u: No such file or directory`), which fatally kills every
/// managed session spawn.
/// Test: `spawn_command_contains_env_scrub`, `spawn_command_sets_claude_config_dir`,
/// `spawn_command_without_config_dir_omits_it`,
/// `env_bin_prefix_quotes_config_dir_with_space`,
/// `env_bin_prefix_orders_unset_flag_before_config_dir_assignment`.
fn env_bin_prefix(claude_bin: &str, config_dir: Option<&Path>) -> String {
    match config_dir {
        Some(dir) => {
            let quoted = shell_single_quote(&dir.display().to_string());
            format!("env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR={quoted} {claude_bin}")
        }
        None => format!("env -u ANTHROPIC_API_KEY {claude_bin}"),
    }
}

/// The shell command sent to the tmux pane to start Claude Code.
///
/// Why: the env prefix (see [`env_bin_prefix`]) strips `ANTHROPIC_API_KEY` and,
/// when available, injects the tm-owned `CLAUDE_CONFIG_DIR` for AUTH isolation.
/// `--setting-sources project,local` is RETAINED and is LOAD-BEARING for where
/// the roster comes from: it restricts every setting source Claude Code loads —
/// settings.json, subagents (`agents/*.md`), and skills (`skills/`) — to the
/// `project` + `local` tiers, EXCLUDING the `user` tier. Because
/// `CLAUDE_CONFIG_DIR` relocates the `user` tier, the agents/skills/hooks the
/// daemon provisions INTO that config home are NOT read while this flag is in
/// force (empirically verified against `claude` 2.1.201, DOC-34 review). The
/// full framework roster, skills, project hooks (trusty-memory + PM-guard) and
/// MCP servers are instead delivered by the PROJECT layer — `<workspace>/.claude/
/// {agents,skills,settings.json,.mcp.json}` — which `session_launch::
/// prepare_session` (run on every daemon spawn path) deploys and which
/// `project,local` DOES load. The flag also excludes the operator's global
/// `~/.claude/settings.json` (#1269). `--dangerously-skip-permissions` keeps the
/// unattended orchestration session from blocking on per-tool prompts (#1269).
/// Both flags reuse the shared [`crate::core::model_inject::SETTING_SOURCES_FLAG`]
/// / [`crate::core::model_inject::PERMISSION_MODE_FLAG`] constants so this spawn
/// path and the CLI launch path can never drift.
/// What: built from [`env_bin_prefix`] plus the two shared flag constants; piped
/// to `tmux send-keys … Enter`. `claude_bin` is the resolved binary — an
/// absolute path under launchd so the pane (which inherits the daemon's minimal
/// `PATH`) does not need `claude` on its own `PATH` (#1298).
/// Test: `spawn_command_contains_env_scrub`,
/// `spawn_command_contains_isolation_flags`,
/// `spawn_command_uses_resolved_binary`,
/// `spawn_command_sets_claude_config_dir`.
fn spawn_command(claude_bin: &str, config_dir: Option<&Path>) -> String {
    format!(
        "{} {} {}",
        env_bin_prefix(claude_bin, config_dir),
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
/// env prefix (scrub + `CLAUDE_CONFIG_DIR`) and isolation flags as
/// [`spawn_command`].
/// Test: `resume_command_with_id_uses_resume_flag`,
/// `resume_command_without_id_with_prior_conv_uses_continue`,
/// `resume_command_without_id_no_prior_conv_uses_plain_spawn`,
/// `resume_command_sets_claude_config_dir`.
fn resume_command(
    claude_bin: &str,
    config_dir: Option<&Path>,
    claude_session_id: Option<&str>,
    has_prior_conv: bool,
) -> String {
    let base = format!(
        "{} {} {}",
        env_bin_prefix(claude_bin, config_dir),
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
/// no prior conversation exists for the workspace. This guard prevents that error.
/// What: delegates to [`has_prior_conversation_in`] with the standard
/// `~/.claude/projects/` directory derived from `dirs::home_dir()`.
/// Test: `has_prior_conversation_returns_false_for_fresh_workspace` exercises
/// the inner function directly via `has_prior_conversation_in`.
fn has_prior_conversation(cwd: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    has_prior_conversation_in(cwd, &home.join(".claude").join("projects"))
}

/// Inner implementation of conversation-history detection, testable with injected dir.
///
/// Why: allows unit tests to pass a temp directory as `projects_dir` without
/// mutating the `HOME` environment variable (which is thread-unsafe under parallel
/// test execution). Separating the I/O root from the detection logic keeps the
/// detection pure and mockable.
/// What: returns `true` when `<projects_dir>/<encoded-cwd>/` exists and contains
/// at least one `.jsonl` file. The encoded path replaces every `/` with `-`.
/// A leading `/` becomes `-`, so `/private/tmp/foo` → `-private-tmp-foo`.
/// Test: `has_prior_conversation_returns_false_for_fresh_workspace`,
/// `has_prior_conversation_returns_true_when_jsonl_exists`.
fn has_prior_conversation_in(cwd: &Path, projects_dir: &Path) -> bool {
    if !projects_dir.is_dir() {
        return false;
    }
    // Claude Code encodes the workspace path by replacing every '/' with '-'.
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

/// Resolve, provision, and trust-seed the managed `CLAUDE_CONFIG_DIR` for a spawn.
///
/// Why (DOC-34): every managed spawn points `claude` at the tm-owned config home
/// primarily for AUTH + TRUST isolation — with the API key scrubbed the session
/// authenticates via the keychain/`.credentials.json` keyed to this config-dir
/// path, and its per-workspace trust is seeded into `<config_dir>/.claude.json`
/// (NEVER `~/.claude.json`), so a managed session never reads or writes the
/// operator's `~/.claude`. NOTE (DOC-34 review): under the retained
/// `--setting-sources project,local` flag the config home's provisioned
/// agents/skills/settings.json are NOT loaded (that flag excludes the `user`
/// tier this dir relocates); the framework roster/skills/hooks are delivered by
/// the PROJECT layer via `session_launch::prepare_session`. The full-roster
/// provisioning here is therefore belt-and-suspenders (it would load only if the
/// flag were ever dropped) — the load-bearing effect of the config dir is auth +
/// trust isolation. This centralises the three coupled steps — resolve the path,
/// provision it, and seed workspace trust — so `spawn` and `spawn_resume` stay
/// identical and cannot drift.
/// What: resolves [`crate::core::trusty_tools_config::managed_claude_config_dir`].
/// When `Some`: provisions it via
/// [`crate::core::managed_config::ensure_managed_config_dir`] and seeds managed
/// trust via [`crate::core::standalone::preseed_managed_trust`] (both non-fatal —
/// a failure logs a warning but the dir is still returned so the session never
/// silently falls back to the project's `.claude/`), returning `Some(dir)`. When
/// `None` (home unresolved): falls back to the legacy
/// [`crate::core::home_trust_seed::preseed_home_trust`] and returns `None`
/// (no `CLAUDE_CONFIG_DIR` to inject).
/// Test: exercised via `spawn_sends_env_scrub_when_binary_available` and the
/// `spawn_command`/`resume_command` config-dir tests (the command-string layer);
/// the provisioning itself is covered in `core::managed_config`.
fn prepare_managed_config(tmux_name: &str, cwd: &Path) -> Option<std::path::PathBuf> {
    let Some(config_dir) = crate::core::trusty_tools_config::managed_claude_config_dir() else {
        // Home unresolved (stripped env): no config dir to point at. Fall back
        // to the legacy home-trust seed so startup prompts are still dismissed.
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(cwd) {
            tracing::warn!(
                session = %tmux_name,
                cwd = %cwd.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }
        return None;
    };

    // Provision the tm-owned config dir with the full framework roster. Non-fatal:
    // even on a partial provisioning error we still point CLAUDE_CONFIG_DIR at it,
    // because that is strictly safer than falling back to the project's committed
    // `.claude/` (the #1996 regression this whole change exists to prevent).
    if let Err(e) = crate::core::managed_config::ensure_managed_config_dir(&config_dir) {
        tracing::warn!(
            session = %tmux_name,
            config_dir = %config_dir.display(),
            "managed config dir provisioning failed (non-fatal): {e}"
        );
    }

    // Seed workspace trust into <config_dir>/.claude.json (isolation invariant:
    // NEVER ~/.claude.json) so the session starts without the trust/MCP dialogs.
    if let Err(e) = crate::core::standalone::preseed_managed_trust(&config_dir, cwd) {
        tracing::warn!(
            session = %tmux_name,
            cwd = %cwd.display(),
            "managed trust pre-seed failed (non-fatal): {e}"
        );
    }

    Some(config_dir)
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
    /// if it cannot be found on `PATH` or in the well-known daemon dirs),
    /// provisions + trust-seeds the tm-owned `CLAUDE_CONFIG_DIR` via
    /// [`prepare_managed_config`], then sends [`spawn_command`]
    /// (`env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR=<dir> <abs-claude>` plus the
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
        // Point the session at the tm-owned CLAUDE_CONFIG_DIR for auth + trust
        // isolation and seed trust there — never at `~/.claude.json` (DOC-34).
        // The framework roster/skills/hooks load from the PROJECT layer under
        // `--setting-sources project,local`, not from this config dir (see
        // `prepare_managed_config`). Non-fatal throughout (closes #1696).
        let config_dir = prepare_managed_config(tmux_name, cwd);
        self.tmux
            .send_line(
                tmux_name,
                &spawn_command(&claude_bin, config_dir.as_deref()),
            )
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
    /// What: resolves the claude binary, provisions + trust-seeds the tm-owned
    /// `CLAUDE_CONFIG_DIR` via [`prepare_managed_config`], checks for prior
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
        let config_dir = prepare_managed_config(tmux_name, cwd);
        self.tmux
            .send_line(
                tmux_name,
                &resume_command(&claude_bin, config_dir.as_deref(), claude_session_id, prior),
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

    /// RAII guard that redirects `$HOME` to a temp dir and restores it on drop
    /// (including panic).
    ///
    /// Why: the adapter's `spawn`/`spawn_resume` now provision the real managed
    /// `CLAUDE_CONFIG_DIR` (resolved under `$HOME`). Tests that drive them must
    /// redirect `$HOME` so the provisioning/trust-seed side effects land in a
    /// throwaway dir instead of the developer's real `~/.trusty-tools`. Pair with
    /// `#[serial_test::serial]` since it mutates process-global env.
    struct HomeGuard {
        prev: Option<String>,
        _tmp: tempfile::TempDir,
    }
    impl HomeGuard {
        fn set() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let prev = std::env::var("HOME").ok();
            // SAFETY: callers are #[serial], so no other test thread reads HOME
            // concurrently; Drop restores the prior value even on panic.
            unsafe { std::env::set_var("HOME", tmp.path()) };
            Self { prev, _tmp: tmp }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev {
                Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    #[test]
    fn claude_code_adapter_identifies() {
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake);
        assert_eq!(adapter.identify(), "claude-code");
    }

    #[test]
    fn spawn_command_contains_env_scrub() {
        // The spawn command must always strip the API key from the environment
        // so the session falls back to OAuth (keyed by CLAUDE_CONFIG_DIR).
        let cmd = spawn_command("claude", None);
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
    fn spawn_command_sets_claude_config_dir() {
        // DOC-34 / #1996: when a managed config dir is available the spawn
        // command MUST export it so the framework roster comes from tm's config
        // home, not the project's committed `.claude/`. It must co-exist with the
        // API-key scrub.
        let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
        let cmd = spawn_command("claude", Some(dir));
        assert!(
            cmd.contains(
                "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' claude"
            ),
            "spawn command must scrub the key (via -u, BEFORE the NAME=VALUE assignment per \
             POSIX env grammar) then set (single-quoted) CLAUDE_CONFIG_DIR: {cmd}"
        );
        assert!(
            cmd.contains("--setting-sources project,local"),
            "isolation flag must still be present with a config dir: {cmd}"
        );
    }

    #[test]
    fn env_bin_prefix_orders_unset_flag_before_config_dir_assignment() {
        // Regression guard (fatal bug): POSIX `env`'s grammar is
        // `env [OPTION]... [NAME=VALUE]... [COMMAND]...` — option flags like
        // `-u NAME` MUST precede any `NAME=VALUE` assignment. Putting
        // `CLAUDE_CONFIG_DIR=<dir>` before `-u` makes `env` stop parsing options
        // at the first NAME=VALUE and try to exec `-u` as a command
        // (`env: -u: No such file or directory`), fatally killing every managed
        // session spawn.
        let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
        let cmd = spawn_command("claude", Some(dir));
        let u_pos = cmd
            .find("-u ANTHROPIC_API_KEY")
            .expect("cmd must contain -u ANTHROPIC_API_KEY");
        let config_pos = cmd
            .find("CLAUDE_CONFIG_DIR=")
            .expect("cmd must contain CLAUDE_CONFIG_DIR=");
        assert!(
            u_pos < config_pos,
            "-u ANTHROPIC_API_KEY must appear BEFORE CLAUDE_CONFIG_DIR= per POSIX env \
             option-then-assignment grammar: {cmd}"
        );
    }

    #[test]
    fn spawn_command_without_config_dir_omits_it() {
        // When home is unresolvable there is no config dir to point at; the
        // command must fall back to the legacy scrub-only prefix (no bare
        // `CLAUDE_CONFIG_DIR=` token).
        let cmd = spawn_command("claude", None);
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR"),
            "no config dir → must not reference CLAUDE_CONFIG_DIR: {cmd}"
        );
    }

    #[test]
    fn env_bin_prefix_quotes_config_dir_with_space() {
        // CRITICAL (DOC-34 review): a home dir with a space must NOT word-split
        // the pane command. The path must appear single-quoted and INTACT so
        // `env` receives one CLAUDE_CONFIG_DIR value, not two argv tokens.
        let dir = Path::new("/Users/John Doe/.trusty-tools/trusty-mpm/claude-config");
        let cmd = spawn_command("claude", Some(dir));
        assert!(
            cmd.contains(
                "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/trusty-mpm/claude-config' claude"
            ),
            "config dir with a space must be single-quoted and intact: {cmd}"
        );
        // The bare unquoted form (which would word-split) must NOT appear.
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR=/Users/John Doe"),
            "config dir must never be interpolated unquoted: {cmd}"
        );
        // Resume path shares env_bin_prefix, so it must quote identically.
        let resume = resume_command("claude", Some(dir), None, false);
        assert!(
            resume.contains(
                "CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/trusty-mpm/claude-config'"
            ),
            "resume command must also single-quote a spaced config dir: {resume}"
        );
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quote() {
        // A literal single quote in the path must be escaped via the POSIX
        // close-reopen sequence '\'' so the quoting stays balanced. (A macOS
        // home never contains one, but the escape must be correct regardless.)
        assert_eq!(shell_single_quote("/a/b"), "'/a/b'");
        assert_eq!(
            shell_single_quote("/Users/o'brien/cfg"),
            r"'/Users/o'\''brien/cfg'"
        );
    }

    #[test]
    fn spawn_command_contains_isolation_flags() {
        // Why: the session_manager spawn path must isolate from the user's global
        // settings and run fully unattended (bypass all permission prompts) so
        // multi-agent orchestration never stalls on interactive approval dialogs.
        let cmd = spawn_command("claude", None);
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
        let cmd = spawn_command("/Users/me/.local/bin/claude", None);
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

    #[serial_test::serial]
    #[test]
    fn spawn_sends_env_scrub_when_binary_available() {
        // Patch: if claude is not available this test is a no-op (we cannot
        // install it in CI). We only assert the send when the binary exists.
        // HOME is redirected so the spawn's config-dir provisioning + trust seed
        // land in a throwaway dir, not the developer's real ~/.trusty-tools.
        let _home = HomeGuard::set();
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
        // The sent command must equal the builder output for whatever config dir
        // resolves under the redirected HOME (Some in a normal env).
        let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir();
        assert_eq!(
            sends[0].1,
            spawn_command(&claude_bin, config_dir.as_deref())
        );
        // And it must carry the env scrub regardless (the option must precede
        // any intervening `CLAUDE_CONFIG_DIR=` assignment per POSIX env grammar).
        assert!(sends[0].1.contains("-u ANTHROPIC_API_KEY"));
    }

    #[test]
    fn resume_command_with_id_uses_resume_flag() {
        // Why (#1744): --resume <id> restores the exact prior conversation;
        // the test pins the contract so accidental regressions are caught early.
        let cmd = resume_command("claude", None, Some("abc-123"), false);
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
    fn resume_command_sets_claude_config_dir() {
        // DOC-34: the resume path must also carry CLAUDE_CONFIG_DIR so resumed
        // sessions read the same tm-owned roster as fresh spawns.
        let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
        let cmd = resume_command("claude", Some(dir), Some("abc-123"), false);
        assert!(
            cmd.contains(
                "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' claude"
            ),
            "resume command must export (single-quoted) CLAUDE_CONFIG_DIR after the -u option: {cmd}"
        );
        assert!(
            cmd.contains("--resume abc-123"),
            "resume command must still include --resume <id>: {cmd}"
        );
    }

    #[test]
    fn resume_command_without_id_with_prior_conv_uses_continue() {
        // Why (#1744 / #1840): when no claude_session_id is stored but prior
        // conversation history exists, --continue resumes the most-recent
        // conversation in the workspace rather than starting fresh.
        let cmd = resume_command("claude", None, None, true);
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
        let cmd = resume_command("claude", None, None, false);
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
        // Why (#1840): a fresh worktree has no Claude conversation history;
        // has_prior_conversation must return false to avoid "No conversation found".
        // Uses has_prior_conversation_in with a temp projects_dir — no HOME env
        // mutation, making this test safe for parallel execution.
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects_dir = tmp.path().join("projects");
        // projects_dir does not exist → always returns false.
        assert!(
            !has_prior_conversation_in(tmp.path(), &projects_dir),
            "no projects dir → false"
        );
        // Create the projects dir but with no entry for this workspace → still false.
        std::fs::create_dir_all(&projects_dir).unwrap();
        assert!(
            !has_prior_conversation_in(tmp.path(), &projects_dir),
            "projects dir exists but no entry for this workspace → false"
        );
    }

    #[test]
    fn has_prior_conversation_returns_true_when_jsonl_exists() {
        // Why (#1840): verify the positive path — a workspace with a .jsonl file
        // in its encoded project dir must return true.
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("my-workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let projects_dir = tmp.path().join("projects");
        // Encode the cwd path as Claude does: replace '/' with '-'.
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = projects_dir.join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("session.jsonl"), "{}").unwrap();
        assert!(
            has_prior_conversation_in(&cwd, &projects_dir),
            "workspace with .jsonl must return true"
        );
    }

    #[serial_test::serial]
    #[test]
    fn spawn_resume_with_id_uses_resume_flag() {
        // Why (#1744): ClaudeCodeAdapter::spawn_resume must send --resume <id>
        // to the pane when the claude_session_id is known.
        // HOME is redirected so the config-dir provisioning is hermetic.
        let _home = HomeGuard::set();
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

    #[serial_test::serial]
    #[test]
    fn spawn_resume_without_id_no_prior_conv_sends_plain_spawn() {
        // Why (#1840, #1845 item 3): when no claude_session_id is available AND the
        // workspace has no prior conversation, spawn_resume must send a plain spawn
        // (no --continue, no --resume) to avoid "No conversation found to continue".
        //
        // The command construction is tested via resume_command() directly with a fake
        // binary so assertions ALWAYS run even when the `claude` binary is absent in CI.
        // The adapter merely calls resume_command() with the same arguments; testing the
        // function directly proves the selection logic without CI depending on claude.
        let cmd = resume_command("__fake_claude__", None, None, false);
        assert!(
            !cmd.contains("--continue"),
            "plain-spawn path must NOT use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "plain-spawn path must NOT use --resume: {cmd}"
        );
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "plain-spawn path must still scrub API key: {cmd}"
        );

        // Additionally verify the adapter-level plumbing when the binary is present.
        // HOME is redirected so the config-dir provisioning is hermetic.
        let _home = HomeGuard::set();
        let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
            return; // core assertions above already ran; adapter path requires binary
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume("test-tmux-session", tmp.path(), "task", None)
            .expect("spawn_resume without id");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            !sends[0].1.contains("--continue"),
            "adapter plain-spawn must NOT use --continue: {}",
            sends[0].1
        );
        assert!(
            !sends[0].1.contains("--resume"),
            "adapter plain-spawn must NOT use --resume: {}",
            sends[0].1
        );
    }
}
