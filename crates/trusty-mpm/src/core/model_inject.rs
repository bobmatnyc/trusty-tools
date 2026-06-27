//! Model injection for Claude Code sessions (issue #390).
//!
//! Why: Claude Code silently ignores the `model:` field in agent frontmatter.
//! trusty-mpm must instead build the `claude` CLI invocation with an explicit
//! `--model` flag so the resolved model actually takes effect. This module
//! centralises the command-string construction so every launch path (CLI
//! `tm launch`, `tm sessions start`, daemon) emits the same correctly-formed
//! command.
//! What: [`build_claude_command`] composes the full shell string passed to
//! `tmux send-keys`; it optionally appends `--model <id>` and
//! `--append-system-prompt-file <path>` flags. [`write_prompt_file`] handles
//! the temp-file side of that second flag.
//! Test: `claude_command_bare`, `claude_command_with_model`,
//! `claude_command_with_prompt`, `claude_command_with_both`,
//! `write_prompt_file_returns_path`.

use std::path::{Path, PathBuf};

use crate::core::config::MpmConfig;
use crate::core::delegation_authority::AgentSummary;

/// Write the session prompt text to a unique temp file.
///
/// Why: `claude --append-system-prompt-file` requires a file path; callers
/// must create that file before spawning `claude`. This helper encapsulates the
/// temp-file creation so every launch path handles it consistently.
/// What: writes `prompt` to `<tmp>/trusty-mpm-system-prompt-<uuid>.txt` and
/// returns the path. Returns `None` and logs a warning on any I/O error.
/// Test: `write_prompt_file_returns_path`.
pub fn write_prompt_file(prompt: &str) -> Option<PathBuf> {
    let file = std::env::temp_dir().join(format!(
        "trusty-mpm-system-prompt-{}.txt",
        uuid::Uuid::new_v4()
    ));
    match std::fs::write(&file, prompt) {
        Ok(()) => Some(file),
        Err(err) => {
            tracing::warn!("failed to write system prompt file: {err}");
            None
        }
    }
}

/// Resolve the model for a PM-session launch (no named agent).
///
/// Why: the top-level `tm launch` / `tm sessions start` path spawns Claude
/// Code as the PM, not as a named specialist agent. The model resolution still
/// reads from the config (using the special key `"pm"` or the configured
/// `models.default`) so operators can pin the PM tier.
/// What: looks up `config.models.agents["pm"]` first, then falls back to
/// `config.models.default`, then to the compiled-in default (`"sonnet"`).
/// `explicit` (from a `--model` CLI flag) always wins. All values are expanded
/// through [`MpmConfig::expand_model_alias`].
/// Test: `pm_model_resolution`.
pub fn resolve_pm_model(config: &MpmConfig, explicit: Option<&str>) -> String {
    crate::core::config::resolve_agent_model(config, "pm", None, explicit)
}

/// The setting-sources flag every trusty-mpm-spawned `claude` carries.
///
/// Why (issue #1269 / step 4): the operator's global `~/.claude/settings.json`
/// carries hooks (e.g. claude-mpm's) that bleed into — and interfere with —
/// trusty-mpm sessions. `--setting-sources project,local` tells Claude Code to
/// load ONLY the project (tm-owned workspace `.claude/settings.json`) and local
/// settings sources, excluding `user`. This is the correct isolation lever: it
/// does NOT touch `~/.claude.json`, so the ambient OAuth login tm relies on is
/// preserved.
/// What: the literal flag string appended to every launch command.
/// Test: `claude_command_includes_setting_sources`.
pub const SETTING_SOURCES_FLAG: &str = "--setting-sources project,local";

/// The permission-mode flag every trusty-mpm-spawned `claude` carries.
///
/// Why: tm runs Claude Code in unattended orchestration mode; `--dangerously-skip-permissions`
/// allows the session to operate without any permission prompts, which is required
/// for fully automated multi-agent orchestration where no human is present to approve
/// individual tool calls. This is appropriate because tm sessions run in provisioned,
/// tm-controlled tmux panes under the operator's explicit supervision.
/// What: the literal flag string appended to every launch command.
/// Test: `claude_command_includes_permission_mode`.
pub const PERMISSION_MODE_FLAG: &str = "--dangerously-skip-permissions";

/// Build the full `claude` command string for `tmux send-keys`.
///
/// Why: the command passed to `tmux send-keys` must be a single shell string;
/// constructing it in one place keeps the CLI `launch`, `session start`, and
/// future daemon-driven paths from drifting apart. It is also the single place
/// the session-isolation flag ([`SETTING_SOURCES_FLAG`]) and the
/// unattended-permission flag ([`PERMISSION_MODE_FLAG`]) are applied so every
/// spawn path stays unattended and isolated (issue #1269).
/// What: always starts with `"claude"`; appends `--model <model>` when `model`
/// is `Some`; appends `--append-system-prompt-file <path>` when `prompt_file` is
/// `Some`; then ALWAYS appends [`SETTING_SOURCES_FLAG`] and
/// [`PERMISSION_MODE_FLAG`]. Returns the composed string.
/// Test: `claude_command_bare`, `claude_command_with_model`,
/// `claude_command_with_prompt`, `claude_command_with_both`,
/// `claude_command_includes_setting_sources`,
/// `claude_command_includes_permission_mode`.
pub fn build_claude_command(model: Option<&str>, prompt_file: Option<&Path>) -> String {
    let mut cmd = "claude".to_string();
    if let Some(m) = model {
        cmd.push_str(" --model ");
        cmd.push_str(m);
    }
    if let Some(p) = prompt_file {
        cmd.push_str(" --append-system-prompt-file ");
        cmd.push_str(&p.display().to_string());
    }
    // Isolation + unattended flags, always applied (issue #1269 / step 4).
    cmd.push(' ');
    cmd.push_str(SETTING_SOURCES_FLAG);
    cmd.push(' ');
    cmd.push_str(PERMISSION_MODE_FLAG);
    cmd
}

/// Resolve and build the full `claude` invocation for an agent session.
///
/// Why: agent delegations need the same model-aware command building as PM
/// sessions, but also carry a named agent and a frontmatter model hint.
/// What: calls [`crate::core::config::resolve_agent_model`] for the four-level
/// precedence, then delegates to [`build_claude_command`].
/// Test: `agent_command_uses_config_model`.
pub fn build_agent_command(
    config: &MpmConfig,
    agent: &AgentSummary,
    prompt_file: Option<&Path>,
    explicit: Option<&str>,
) -> String {
    let model = crate::core::config::resolve_agent_model(
        config,
        &agent.name,
        agent.model.as_deref(),
        explicit,
    );
    build_claude_command(Some(&model), prompt_file)
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The isolation + unattended suffix every command now carries (#1269).
    const FLAGS: &str = "--setting-sources project,local --dangerously-skip-permissions";

    #[test]
    fn claude_command_bare() {
        // No model, no prompt file → "claude" + the always-on isolation flags.
        assert_eq!(build_claude_command(None, None), format!("claude {FLAGS}"));
    }

    #[test]
    fn claude_command_with_model() {
        let cmd = build_claude_command(Some("claude-opus-4-5"), None);
        assert_eq!(cmd, format!("claude --model claude-opus-4-5 {FLAGS}"));
    }

    #[test]
    fn claude_command_with_prompt() {
        let path = Path::new("/tmp/prompt.txt");
        let cmd = build_claude_command(None, Some(path));
        assert_eq!(
            cmd,
            format!("claude --append-system-prompt-file /tmp/prompt.txt {FLAGS}")
        );
    }

    #[test]
    fn claude_command_with_both() {
        let path = Path::new("/tmp/sys.txt");
        let cmd = build_claude_command(Some("claude-haiku-4-5"), Some(path));
        assert_eq!(
            cmd,
            format!(
                "claude --model claude-haiku-4-5 --append-system-prompt-file /tmp/sys.txt {FLAGS}"
            )
        );
    }

    #[test]
    fn claude_command_includes_setting_sources() {
        // Why (#1269/step 4): every spawned session must EXCLUDE the user's
        // global settings by loading only project,local sources.
        let cmd = build_claude_command(None, None);
        assert!(
            cmd.contains("--setting-sources project,local"),
            "missing setting-sources isolation flag: {cmd}"
        );
        // Must not load the `user` source.
        assert!(
            !cmd.contains("user"),
            "should not reference the user source: {cmd}"
        );
    }

    #[test]
    fn claude_command_includes_permission_mode() {
        // Why: unattended orchestration sessions must not block on permission prompts;
        // bypass-permissions mode is required for fully automated multi-agent workflows.
        let cmd = build_claude_command(Some("sonnet"), None);
        assert!(
            cmd.contains("--dangerously-skip-permissions"),
            "missing bypass-permissions flag: {cmd}"
        );
        // Must not carry the old acceptEdits flag.
        assert!(
            !cmd.contains("acceptEdits"),
            "old acceptEdits flag must not be present: {cmd}"
        );
    }

    #[test]
    fn write_prompt_file_returns_path() {
        let path = write_prompt_file("hello trusty-mpm").unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello trusty-mpm");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn pm_model_resolution() {
        let cfg = MpmConfig::default();
        // Without explicit model, falls back to compiled-in default.
        let m = resolve_pm_model(&cfg, None);
        assert_eq!(m, "claude-sonnet-4-5");

        // Explicit wins.
        let m = resolve_pm_model(&cfg, Some("haiku"));
        assert_eq!(m, "claude-haiku-4-5");
    }

    #[test]
    fn agent_command_uses_config_model() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = "[models.agents]\nengineer = \"haiku\"\n";
        std::fs::write(dir.path().join("config.toml"), toml).unwrap();
        let cfg = MpmConfig::load(dir.path());

        let agent = AgentSummary {
            name: "engineer".to_string(),
            role: "engineer".to_string(),
            description: None,
            model: Some("sonnet".to_string()),
            extends_chain: vec![],
        };

        // Config per-agent override (haiku) wins over frontmatter (sonnet).
        let cmd = build_agent_command(&cfg, &agent, None, None);
        assert_eq!(cmd, format!("claude --model claude-haiku-4-5 {FLAGS}"));
    }
}
