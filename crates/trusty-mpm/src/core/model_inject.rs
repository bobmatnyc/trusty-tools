//! Model injection for Claude Code sessions (issue #390).
//!
//! Why: Claude Code silently ignores the `model:` field in agent frontmatter.
//! trusty-mpm must instead build the `claude` CLI invocation with an explicit
//! `--model` flag so the resolved model actually takes effect. This module
//! centralises the command-string construction so every launch path (CLI
//! `tm launch`, `tm session start`, daemon) emits the same correctly-formed
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
/// Why: the top-level `tm launch` / `tm session start` path spawns Claude
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

/// The setting-sources flag a spawn that does NOT relocate `CLAUDE_CONFIG_DIR`
/// carries.
///
/// Why (issue #1269 / step 4): the operator's global `~/.claude/settings.json`
/// carries hooks (e.g. claude-mpm's) that bleed into — and interfere with —
/// trusty-mpm sessions. `--setting-sources project,local` tells Claude Code to
/// load ONLY the project (tm-owned workspace `.claude/settings.json`) and local
/// settings sources, excluding `user`. This is the correct isolation lever: it
/// does NOT touch `~/.claude.json`, so the ambient OAuth login tm relies on is
/// preserved. It stays the flag for launch paths that do NOT inject their own
/// `CLAUDE_CONFIG_DIR` (`tm launch`, `tm connect`), where the `user` tier still
/// resolves to the operator's real `~/.claude` and must stay excluded.
/// What: the literal flag string appended to those launch commands.
/// Test: `claude_command_includes_setting_sources`.
pub const SETTING_SOURCES_FLAG: &str = "--setting-sources project,local";

/// The setting-sources flag a spawn that DOES relocate `CLAUDE_CONFIG_DIR`
/// carries.
///
/// Why (issue #4451): Claude Code discovers subagents (`agents/*.md`) per
/// settings tier, and `$CLAUDE_CONFIG_DIR/agents` IS the `user` tier — the tier
/// [`SETTING_SOURCES_FLAG`] deliberately drops. Since #4437 moved the bundled
/// roster into the tm-managed `CLAUDE_CONFIG_DIR`, a managed session carrying
/// the `project,local` flag saw ZERO bundled specialists: `Agent type
/// 'rust-engineer' not found`, every delegation degrading to `general-purpose`,
/// while the 42 files sat correctly on disk. Adding `user` back is safe here
/// precisely BECAUSE the spawn relocates `CLAUDE_CONFIG_DIR`: the `user` tier
/// then resolves to the tm-owned config home, not the operator's `~/.claude`,
/// so the #1269 isolation goal (keep claude-mpm's global hooks out) is still met
/// — by the relocation rather than by the exclusion.
///
/// Verified live against `claude` 2.1.220 with
/// `CLAUDE_CONFIG_DIR=~/.trusty-tools/trusty-mpm/claude-config` from a cwd with
/// no `.claude/`: `project,local` resolves 5 built-ins only, while both the
/// flag-less default and `user,project,local` resolve all 42 bundled agents and
/// NONE of the operator's own `~/.claude/agents` entries.
/// What: the literal flag string appended to relocated launch commands.
/// Test: `relocated_setting_sources_flag_loads_the_user_tier`,
/// `setting_sources_flag_picks_by_config_dir`.
pub const SETTING_SOURCES_FLAG_RELOCATED: &str = "--setting-sources user,project,local";

/// Pick the setting-sources flag matching a spawn's `CLAUDE_CONFIG_DIR` posture.
///
/// Why (issue #4451): the two flags are not interchangeable, and picking the
/// wrong one fails silently in opposite directions — `project,local` on a
/// relocated spawn hides the bundled roster, `user,project,local` on a
/// non-relocated spawn re-admits the operator's global hooks (#1269). Deriving
/// the choice from the one input that decides it (does this spawn inject
/// `CLAUDE_CONFIG_DIR`?) removes the chance of a call site getting it wrong.
/// What: [`SETTING_SOURCES_FLAG_RELOCATED`] when `config_dir` is `Some`
/// (the spawn points `CLAUDE_CONFIG_DIR` at a tm-owned home), otherwise
/// [`SETTING_SOURCES_FLAG`].
/// Test: `setting_sources_flag_picks_by_config_dir`.
pub fn setting_sources_flag(config_dir: Option<&Path>) -> &'static str {
    if config_dir.is_some() {
        SETTING_SOURCES_FLAG_RELOCATED
    } else {
        SETTING_SOURCES_FLAG
    }
}

/// The settings tiers a `--setting-sources <list>` flag actually enumerates.
///
/// Why (issue #4203): a deploy step that writes agents into a tier the spawn's
/// flag does NOT name is a silent no-op — Claude Code never loads them, and
/// nothing reports an error. Parsing the flag instead of hard-coding the tier
/// names keeps that invariant honest if either flag's value ever changes: the
/// tier check and the spawned command can never drift apart.
/// What: splits `flag` at its first space and then the comma-separated tier
/// list, trimming each name.
/// Test: `setting_source_tiers_parses_the_flag`,
/// `relocated_setting_sources_flag_loads_the_user_tier`.
pub fn setting_source_tiers_of(flag: &'static str) -> Vec<&'static str> {
    flag.split_once(' ')
        .map(|(_, list)| list.split(',').map(str::trim).collect())
        .unwrap_or_default()
}

/// The settings tiers [`SETTING_SOURCES_FLAG`] enumerates.
///
/// Why/What: see [`setting_source_tiers_of`]; this is the non-relocated
/// (`tm launch` / `tm connect`) tier list.
/// Test: `setting_source_tiers_parses_the_flag`.
pub fn setting_source_tiers() -> Vec<&'static str> {
    setting_source_tiers_of(SETTING_SOURCES_FLAG)
}

/// The settings tiers [`SETTING_SOURCES_FLAG_RELOCATED`] enumerates.
///
/// Why (issue #4451): `tm doctor` needs this list to assert that the tier the
/// bundled roster deploys into is one a managed session actually loads. Reading
/// it from the flag — rather than re-stating `["user", "project", "local"]` —
/// is what makes the doctor check a real gate instead of a tautology: change
/// either the flag or the deploy destination alone and the check fails.
/// What: see [`setting_source_tiers_of`].
/// Test: `relocated_setting_sources_flag_loads_the_user_tier`.
pub fn relocated_setting_source_tiers() -> Vec<&'static str> {
    setting_source_tiers_of(SETTING_SOURCES_FLAG_RELOCATED)
}

/// Name the Claude Code settings tier a `.claude/` deploy destination lands in,
/// relative to the cwd the harness is spawned with.
///
/// Why (issue #4203): `FrameworkPaths::default()` deploys agents into
/// `$HOME/.claude` — the `user` tier — while every trusty-mpm spawn carries
/// [`SETTING_SOURCES_FLAG`], which excludes `user`. Deploy and load never
/// intersect. Naming the tier turns that mismatch into something a test can
/// assert on directly, rather than comparing hard-coded path strings that would
/// both pass vacuously if either side moved.
/// What: `"project"` when `claude_home` IS the harness cwd (the payload lands
/// in `<cwd>/.claude`, which both the `project` and `local` tiers read);
/// `"user"` otherwise.
/// Test: `settings_tier_of_names_project_for_the_harness_cwd`,
/// `settings_tier_of_names_user_for_anything_else`.
pub fn settings_tier_of(claude_home: &Path, harness_cwd: &Path) -> &'static str {
    if claude_home == harness_cwd {
        "project"
    } else {
        "user"
    }
}

/// The permission-mode flag every trusty-mpm-spawned `claude` carries.
///
/// Why: tm runs Claude Code in unattended orchestration mode; `--dangerously-skip-permissions`
/// allows the session to operate without any permission prompts, which is required
/// for fully automated multi-agent orchestration where no human is present to approve
/// individual tool calls. This is appropriate because tm session run in provisioned,
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
///
/// Issue #4467: the line is prefixed with `env <-u marker…>` from
/// [`crate::core::claude_env_scrub::env_unset_flags`]. This builder's own doc
/// claimed to be the place "every spawn path stays unattended and isolated", but
/// it emitted a BARE `claude` with no `env` prefix at all — so `tm launch`,
/// `tm connect` and every agent delegation launched with the inherited
/// `CLAUDE_CODE_CHILD_SESSION` marker intact and silently saved no transcript.
/// There are no `NAME=VALUE` assignments on this prefix (unlike
/// `runtime::claude_code::env_bin_prefix`, this path does not relocate
/// `CLAUDE_CONFIG_DIR`), so the POSIX ordering constraint is satisfied trivially
/// — but the flags still lead the line for consistency with that builder.
/// What: always starts with `env <-u marker…> claude`; appends `--model <model>`
/// when `model` is `Some`; appends `--append-system-prompt-file <path>` when
/// `prompt_file` is `Some`; then ALWAYS appends [`SETTING_SOURCES_FLAG`] and
/// [`PERMISSION_MODE_FLAG`]. Returns the composed string.
/// Test: `claude_command_bare`, `claude_command_with_model`,
/// `claude_command_with_prompt`, `claude_command_with_both`,
/// `claude_command_includes_setting_sources`,
/// `claude_command_includes_permission_mode`,
/// `claude_command_scrubs_inherited_session_markers`.
pub fn build_claude_command(model: Option<&str>, prompt_file: Option<&Path>) -> String {
    // #4467: scrub the inherited Claude Code session markers so `tm launch` /
    // `tm connect` / delegations keep native --resume/--continue/rewind.
    let mut cmd = format!("env{} claude", crate::core::claude_env_scrub::env_unset_flags());
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

    /// The `env <-u marker…> claude` head every command now carries (#4467).
    ///
    /// Interpolated from production so these pins assert the marker segment's
    /// POSITION; its CONTENT is pinned by literal name in
    /// `core::claude_env_scrub`'s `every_marker_is_pinned_by_literal_name`.
    fn head() -> String {
        format!("env{} claude", crate::core::claude_env_scrub::env_unset_flags())
    }

    #[test]
    fn claude_command_bare() {
        // No model, no prompt file → the env-scrub head + the isolation flags.
        assert_eq!(
            build_claude_command(None, None),
            format!("{} {FLAGS}", head())
        );
    }

    #[test]
    fn claude_command_with_model() {
        let cmd = build_claude_command(Some("claude-opus-4-5"), None);
        assert_eq!(
            cmd,
            format!("{} --model claude-opus-4-5 {FLAGS}", head())
        );
    }

    /// #4467: this builder is the launch line for `tm launch`, `tm connect` and
    /// every agent delegation, and it previously emitted a BARE `claude` — so all
    /// three silently saved no transcript. Marker names are hard-coded so this
    /// cannot go vacuous if the shared list is emptied.
    #[test]
    fn claude_command_scrubs_inherited_session_markers() {
        let cmd = build_claude_command(None, None);
        assert!(
            cmd.starts_with("env -u "),
            "the launch line must carry an env scrub prefix: {cmd}"
        );
        for marker in [
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDECODE",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
            "CLAUDE_CODE_EXECPATH",
        ] {
            assert!(
                cmd.contains(&format!("-u {marker}")),
                "the launch line must unset {marker}: {cmd}"
            );
        }
        // Over-scrub guard: this path does not relocate the config dir, so it
        // must neither set nor unset it (#4455).
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR"),
            "this builder must not touch CLAUDE_CONFIG_DIR at all: {cmd}"
        );
        assert!(
            !cmd.contains("-u CLAUDE_CODE_OAUTH_TOKEN"),
            "must never unset the OAuth token (#2246): {cmd}"
        );
    }

    #[test]
    fn claude_command_with_prompt() {
        let path = Path::new("/tmp/prompt.txt");
        let cmd = build_claude_command(None, Some(path));
        assert_eq!(
            cmd,
            format!(
                "{} --append-system-prompt-file /tmp/prompt.txt {FLAGS}",
                head()
            )
        );
    }

    #[test]
    fn claude_command_with_both() {
        let path = Path::new("/tmp/sys.txt");
        let cmd = build_claude_command(Some("claude-haiku-4-5"), Some(path));
        assert_eq!(
            cmd,
            format!(
                "{} --model claude-haiku-4-5 --append-system-prompt-file /tmp/sys.txt {FLAGS}",
                head()
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
        // #4467: delegations go through `build_claude_command`, so they carry the
        // same `env <-u marker…>` head — an agent session that saved no
        // transcript was the same defect as a PM session that saved none.
        let cmd = build_agent_command(&cfg, &agent, None, None);
        assert_eq!(
            cmd,
            format!("{} --model claude-haiku-4-5 {FLAGS}", head())
        );
    }

    #[test]
    fn setting_source_tiers_parses_the_flag() {
        // Derived from the flag itself, never hard-coded — the tier check and
        // the spawned command must not be able to drift apart (issue #4203).
        assert_eq!(setting_source_tiers(), vec!["project", "local"]);
        assert!(
            !setting_source_tiers().contains(&"user"),
            "on a spawn that does NOT relocate CLAUDE_CONFIG_DIR the `user` tier \
             is the operator's real ~/.claude and stays excluded (issue #1269)"
        );
    }

    #[test]
    fn relocated_setting_sources_flag_loads_the_user_tier() {
        // #4451: bundled agents deploy into $CLAUDE_CONFIG_DIR/agents, which IS
        // the `user` tier. A relocated spawn that does not load `user` cannot
        // see a single bundled specialist, however complete the deploy is.
        assert!(
            relocated_setting_source_tiers().contains(&"user"),
            "the relocated spawn must load the `user` tier — that is where the \
             bundled roster lives once CLAUDE_CONFIG_DIR is redirected"
        );
        assert_eq!(
            relocated_setting_source_tiers(),
            vec!["user", "project", "local"]
        );
    }

    #[test]
    fn setting_sources_flag_picks_by_config_dir() {
        // #4451: the choice is derived from the one fact that decides it —
        // whether the spawn injects its own CLAUDE_CONFIG_DIR.
        assert_eq!(
            setting_sources_flag(Some(Path::new("/tm/claude-config"))),
            SETTING_SOURCES_FLAG_RELOCATED
        );
        assert_eq!(setting_sources_flag(None), SETTING_SOURCES_FLAG);
    }

    #[test]
    fn settings_tier_of_names_project_for_the_harness_cwd() {
        // `<cwd>/.claude` is read by the project (and local) tiers.
        let cwd = Path::new("/work/checkout");
        assert_eq!(settings_tier_of(cwd, cwd), "project");
    }

    #[test]
    fn settings_tier_of_names_user_for_anything_else() {
        // What `FrameworkPaths::default()` produced: `$HOME/.claude`, a tier
        // `--setting-sources project,local` never loads (issue #4203).
        assert_eq!(
            settings_tier_of(Path::new("/Users/someone"), Path::new("/work/checkout")),
            "user"
        );
    }
}
