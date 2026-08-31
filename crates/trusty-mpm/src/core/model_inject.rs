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
//!
//! Issue #4467: this module owns THREE launch-line builders, and every one of
//! them must be built here rather than hand-written at a call site. That is not
//! style — `daemon::doctor_transcript_saving`'s `launch_lines()` can only read
//! builders the library owns, so a hand-built line is structurally invisible to
//! the check that exists to catch an unscrubbed spawn. Three separate call sites
//! had hand-built their own and all three silently saved no transcript.
//! [`build_inplace_session_command`] and [`build_client_session_command`]
//! deliberately do NOT carry [`SETTING_SOURCES_FLAG`]: their paths do not
//! relocate `CLAUDE_CONFIG_DIR`, so dropping the `user` tier would change which
//! of the operator's own settings and agents load.
//! Test: `claude_command_bare`, `claude_command_with_model`,
//! `claude_command_with_prompt`, `claude_command_with_both`,
//! `write_prompt_file_returns_path`,
//! `inplace_session_command_scrubs_inherited_session_markers`,
//! `client_session_command_scrubs_inherited_session_markers`.

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
/// `CLAUDE_CONFIG_DIR`, where the `user` tier still resolves to the operator's
/// real `~/.claude` and must stay excluded.
///
/// #4181: `tm launch` and `tm connect` are no longer on that list unconditionally.
/// They relocate whenever
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] resolves, and
/// fall back to this flag when it does not (a stripped environment with no
/// resolvable home) — so the exclusion is still what protects those sessions in
/// exactly the case where relocation cannot.
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
/// it emitted a BARE `claude` with no `env` prefix at all — so `tm launch` and
/// `tm connect` launched with the inherited `CLAUDE_CODE_CHILD_SESSION` marker
/// intact and silently saved no transcript.
///
/// Round-2 review correction: earlier wording here (and the doctor's label) also
/// advertised this as the AGENT-DELEGATION launch line via [`build_agent_command`].
/// That was wrong and would have sent an operator chasing a `transcript_saving`
/// failure to a path that does not exist: `build_agent_command` has no production
/// caller anywhere in the workspace — only its own definition and test. It is
/// left in place rather than deleted because `trusty-mpm` is a published crate
/// with no `publish = false`, so removing a `pub fn` is a semver-breaking change
/// that does not belong in a bug-fix PR; removing it is filed as follow-up.
/// Issue #4181 (ADR-0042 precondition): this builder now DOES relocate
/// `CLAUDE_CONFIG_DIR` when `config_dir` is `Some`, so the POSIX ordering
/// constraint is live rather than trivially satisfied — the `-u` scrub flags
/// lead the line and every `NAME=VALUE` assignment follows them, exactly as
/// `runtime::claude_code::env_bin_prefix` does. An assignment placed before a
/// `-u` makes `env` stop parsing options and try to exec `-u` as a command
/// (`env: -u: No such file or directory`), which kills the spawn outright.
///
/// The `env` token becoming the line's PROGRAM is deliberate and safe on the two
/// CLI callers, both of which wrap this output in
/// [`crate::core::spawn_disclaim::disclaim_pane_command`] (#2997). That yields
/// `<tm> internal-spawn-disclaimed env -u … claude …`, so the shim
/// `posix_spawn`s `env` disclaimed and `env` then EXECs `claude` in that same
/// process — the disclaim is a process attribute and survives the exec, and
/// because `env` exec's rather than forks, the process tree
/// `crate::core::process` walks (`pane sh → tm shim → claude`) is unchanged.
/// The daemon path composes the two the other way round (`env … <tm>
/// internal-spawn-disclaimed <abs claude>`); both shapes end with `claude`
/// running as its own responsible process. Note also that the shim resolves its
/// program through `PATH`, and `env` is no less resolvable than the bare
/// `claude` this line used to emit — so this is not a new `PATH` dependency.
/// What: `env <-u marker…> [CLAUDE_CONFIG_DIR='<dir>']
/// [CLAUDE_CODE_OAUTH_TOKEN='<token>'] claude`; appends `--model <model>` when
/// `model` is `Some`; appends `--append-system-prompt-file <path>` when
/// `prompt_file` is `Some`; then ALWAYS appends the flag
/// [`setting_sources_flag`] picks for `config_dir` and [`PERMISSION_MODE_FLAG`].
/// The OAuth token is resolved by [`crate::core::oauth_token::resolve_oauth_token`]
/// and emitted ONLY when `config_dir` is `Some` — see
/// [`build_claude_command_with`] for why a relocated spawn needs it and a
/// non-relocated one must not carry it. Returns the composed string.
/// Test: `claude_command_bare`, `claude_command_with_model`,
/// `claude_command_with_prompt`, `claude_command_with_both`,
/// `claude_command_includes_setting_sources`,
/// `claude_command_includes_permission_mode`,
/// `claude_command_scrubs_inherited_session_markers`,
/// `claude_command_relocates_the_config_dir`.
pub fn build_claude_command(
    model: Option<&str>,
    prompt_file: Option<&Path>,
    config_dir: Option<&Path>,
    mcp_env: &[(String, String)],
) -> String {
    // #4181: a relocated spawn reads its credentials from a Keychain entry keyed
    // by a hash of CLAUDE_CONFIG_DIR, so it needs the token; a non-relocated one
    // already resolves the operator's own login and must not be handed a token
    // the operator did not ask this path to use (#2246).
    let token = config_dir.and_then(|_| crate::core::oauth_token::resolve_oauth_token());
    build_claude_command_with(model, prompt_file, config_dir, token.as_deref(), mcp_env)
}

/// Hermetic core of [`build_claude_command`], taking the OAuth token explicitly.
///
/// Why (issues #4181 / #2246): [`crate::core::oauth_token::resolve_oauth_token`]
/// reads the process environment and the on-disk token file, so a builder that
/// called it internally could not be tested against a relocated `config_dir`
/// without the ambient machine deciding the result. Splitting the resolution out
/// mirrors the `inject_native_trusty_mcps` / `_from` split already used
/// elsewhere in this crate, and lets `daemon::doctor_transcript_saving` probe
/// the REAL relocated shape (config dir AND token present) with a placeholder
/// instead of a credential.
///
/// On macOS the Keychain entry Claude Code reads is keyed by a hash of
/// `CLAUDE_CONFIG_DIR`. Relocating the config dir without also supplying
/// `CLAUDE_CODE_OAUTH_TOKEN` therefore produces the #2246 failure — `/login`
/// reports success and the session is immediately not-logged-in again, because
/// the credential was written under one hash and read under another. The token
/// bypasses the Keychain entirely. `runtime::claude_code::env_bin_prefix` closes
/// the same hole the same way for the daemon path; this is that mechanism, not a
/// second one.
/// #6495: the line also carries
/// [`crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT`] unconditionally, so
/// a `tm launch` / `tm connect` pane starts on Claude Code's classic renderer
/// and keeps its scrollback. Its `${NAME-1}` expansion yields to a value the
/// pane already exports.
/// What: composes `env`, the [`crate::core::claude_env_scrub::env_unset_flags`]
/// `-u` operands, the alternate-screen operand, then the optional
/// `CLAUDE_CONFIG_DIR` and
/// [`crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR`] assignments (both
/// single-quoted via [`crate::core::spawn_disclaim::pane::shell_single_quote`],
/// both AFTER the `-u` flags per POSIX `env` grammar), then `claude` and its
/// flags.
/// Test: `claude_command_relocates_the_config_dir`,
/// `claude_command_relocated_never_scrubs_what_it_assigns`,
/// `claude_command_quotes_a_config_dir_with_a_space`,
/// `claude_command_omits_the_oauth_token_when_absent`,
/// `claude_command_relocated_isolates_by_relocation_not_exclusion`,
/// `claude_command_carries_a_non_empty_mcp_env`,
/// `claude_command_scrubs_inherited_session_markers` (the non-relocated shape),
/// `claude_command_defaults_the_alternate_screen_off`.
pub fn build_claude_command_with(
    model: Option<&str>,
    prompt_file: Option<&Path>,
    config_dir: Option<&Path>,
    oauth_token: Option<&str>,
    mcp_env: &[(String, String)],
) -> String {
    use crate::core::spawn_disclaim::pane::shell_single_quote;

    // #4467: scrub the inherited Claude Code session markers so `tm launch` /
    // `tm connect` / delegations keep native --resume/--continue/rewind. These
    // `-u` flags MUST precede every assignment below (POSIX `env` grammar).
    let mut cmd = format!("env{}", crate::core::claude_env_scrub::env_unset_flags());
    // #6495: start on the classic renderer so the pane keeps native and tmux
    // scrollback; `${NAME-1}` yields to a value the pane already exports.
    cmd.push(' ');
    cmd.push_str(crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT);
    if let Some(dir) = config_dir {
        // #4181: point the session at the tm-owned config home so the `user`
        // settings tier resolves there instead of the operator's `~/.claude`.
        cmd.push_str(" CLAUDE_CONFIG_DIR=");
        cmd.push_str(&shell_single_quote(&dir.display().to_string()));
    }
    if let Some(token) = oauth_token {
        // #2246: bypass the CLAUDE_CONFIG_DIR-keyed Keychain entry.
        cmd.push(' ');
        cmd.push_str(crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR);
        cmd.push('=');
        cmd.push_str(&shell_single_quote(token));
    }
    // #4181: the per-project MCP pins, in place of the arguments the deleted
    // `.mcp.json` injectors wrote. See `core::mcp_session_env`.
    for (name, value) in mcp_env {
        cmd.push(' ');
        cmd.push_str(name);
        cmd.push('=');
        cmd.push_str(&shell_single_quote(value));
    }
    cmd.push_str(" claude");
    if let Some(m) = model {
        cmd.push_str(" --model ");
        cmd.push_str(m);
    }
    if let Some(p) = prompt_file {
        cmd.push_str(" --append-system-prompt-file ");
        cmd.push_str(&p.display().to_string());
    }
    // Isolation + unattended flags, always applied (issue #1269 / step 4).
    // #4181: the isolation lever is now the RELOCATION when `config_dir` is
    // `Some` — `setting_sources_flag` picks the matching tier list so the two
    // can never drift.
    cmd.push(' ');
    cmd.push_str(setting_sources_flag(config_dir));
    cmd.push(' ');
    cmd.push_str(PERMISSION_MODE_FLAG);
    cmd
}

/// The `claude` launch line for an in-place `tm session start` pane.
///
/// Why (issue #4467, review round 2 HIGH): `bin/tm`'s `start_session_in_place`
/// hand-built `format!("claude {PERMISSION_MODE_FLAG}")` and sent it straight to
/// a fresh tmux pane — a SIXTH interactive launch line with no marker scrub, no
/// test, and no doctor coverage. It is reachable on the documented "just run
/// claude here" case (`tm session start` outside a GitHub-backed git repo), so
/// those sessions silently saved no transcript. Living in the library rather
/// than the binary is the point: `daemon::doctor_transcript_saving`'s
/// `launch_lines()` can only read builders the library owns, so a binary-crate
/// builder is structurally invisible to the check that exists to catch exactly
/// this.
///
/// Deliberately NOT routed through [`build_claude_command`]: that would also add
/// [`SETTING_SOURCES_FLAG`], and `--setting-sources project,local` DROPS the
/// `user` tier. On this path `CLAUDE_CONFIG_DIR` is not relocated, so the user
/// tier is the operator's own `~/.claude` — dropping it would change which
/// settings and agents an in-place session loads. This fix scrubs the markers
/// and changes nothing else.
/// What: `env <-u marker…> CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN="${…-1}" claude
/// --dangerously-skip-permissions`. The scrub flags lead the line and the one
/// `NAME=VALUE` assignment follows them, as POSIX `env` grammar requires.
///
/// #6495 added that assignment. It does not disturb the settings-tier reasoning
/// above: it is an environment default, not a `--setting-sources` flag, so this
/// path still loads the operator's own `user` tier.
/// Test: `inplace_session_command_scrubs_inherited_session_markers`,
/// `inplace_session_command_keeps_the_permission_flag_and_nothing_else`,
/// `inplace_session_command_defaults_the_alternate_screen_off`.
pub fn build_inplace_session_command() -> String {
    // #4467: same shared scrub every other launch line uses — never a second
    // mechanism.
    format!(
        "env{} {} claude {}",
        crate::core::claude_env_scrub::env_unset_flags(),
        // #6495: classic renderer by default, yielding to the pane's own value.
        crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT,
        PERMISSION_MODE_FLAG
    )
}

/// The `claude` launch line the daemon CLIENT sends to a fresh tmux pane.
///
/// Why (issue #4467, found by the round-2 anti-drift scan): `DaemonClient`'s
/// `launch_session` and `connect_session` — the TUI/bot surface behind
/// `/connect <dir>` — each hand-built `format!("claude --append-system-prompt-file
/// {}", …)` with no marker scrub. Two more interactive launch lines that saved no
/// transcript, neither of them named in the review that found the sixth. Sharing
/// one builder is what stops the seventh: a hand-built line in a caller is
/// invisible to `daemon::doctor_transcript_saving`'s `launch_lines()`, which can
/// only read builders.
///
/// Deliberately NOT routed through [`build_claude_command`], for the same reason
/// as [`build_inplace_session_command`]: that would add [`SETTING_SOURCES_FLAG`]
/// and drop the `user` settings tier on a path that does not relocate
/// `CLAUDE_CONFIG_DIR`. This scrubs the markers and changes nothing else.
/// What: `env <-u marker…> CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN="${…-1}" claude`
/// plus `--append-system-prompt-file <path>` when `prompt_file` is `Some`. The
/// caller falls back to `None` when writing the prompt file failed, which is why
/// the argument is optional. #6495 added the assignment so a `/connect` pane
/// starts on the classic renderer and keeps its scrollback.
/// Test: `client_session_command_scrubs_inherited_session_markers`,
/// `client_session_command_appends_the_prompt_file`,
/// `client_session_command_defaults_the_alternate_screen_off`.
pub fn build_client_session_command(prompt_file: Option<&Path>) -> String {
    // #4467: same shared scrub every other launch line uses — never a second
    // mechanism.
    let mut cmd = format!(
        "env{} {} claude",
        crate::core::claude_env_scrub::env_unset_flags(),
        // #6495: classic renderer by default, yielding to the pane's own value.
        crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT
    );
    if let Some(p) = prompt_file {
        cmd.push_str(" --append-system-prompt-file ");
        cmd.push_str(&p.display().to_string());
    }
    cmd
}

/// Resolve and build the full `claude` invocation for an agent session.
///
/// Why: agent delegations need the same model-aware command building as PM
/// sessions, but also carry a named agent and a frontmatter model hint.
/// What: calls [`crate::core::config::resolve_agent_model`] for the four-level
/// precedence, then delegates to [`build_claude_command`] with no `config_dir`.
///
/// #4181: this builder has no production caller anywhere in the workspace (see
/// [`build_claude_command`]'s round-2 note), so it is left non-relocating rather
/// than given a `config_dir` parameter no caller would supply.
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
    build_claude_command(Some(&model), prompt_file, None, &[])
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The isolation + unattended suffix every command now carries (#1269).
    const FLAGS: &str = "--setting-sources project,local --dangerously-skip-permissions";

    /// The `env <-u marker…> <alt-screen operand> claude` head every command now
    /// carries (#4467, #6495).
    ///
    /// Interpolated from production so these pins assert each segment's
    /// POSITION; the marker CONTENT is pinned by literal name in
    /// `core::claude_env_scrub`'s `every_marker_is_pinned_by_literal_name`, and
    /// the alternate-screen operand's literal text in `core::alt_screen`'s
    /// `shell_assignment_pins_the_defaulting_form`.
    fn head() -> String {
        format!(
            "env{} {} claude",
            crate::core::claude_env_scrub::env_unset_flags(),
            crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT
        )
    }

    #[test]
    fn claude_command_bare() {
        // No model, no prompt file → the env-scrub head + the isolation flags.
        assert_eq!(
            build_claude_command(None, None, None, &[]),
            format!("{} {FLAGS}", head())
        );
    }

    #[test]
    fn claude_command_with_model() {
        let cmd = build_claude_command(Some("claude-opus-4-5"), None, None, &[]);
        assert_eq!(cmd, format!("{} --model claude-opus-4-5 {FLAGS}", head()));
    }

    /// #4467: this builder is the launch line for `tm launch`, `tm connect` and
    /// every agent delegation, and it previously emitted a BARE `claude` — so all
    /// three silently saved no transcript. Marker names are hard-coded so this
    /// cannot go vacuous if the shared list is emptied.
    #[test]
    fn claude_command_scrubs_inherited_session_markers() {
        let cmd = build_claude_command(None, None, None, &[]);
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
        // Over-scrub guard, unchanged by #4181: this CALL passes no config dir,
        // so the line must neither set nor unset it (#4455). #4181 gave the
        // builder a relocating shape as well — that shape's counterpart guard is
        // `claude_command_relocated_never_scrubs_what_it_assigns`.
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR"),
            "this builder must not touch CLAUDE_CONFIG_DIR at all: {cmd}"
        );
        assert!(
            !cmd.contains("-u CLAUDE_CODE_OAUTH_TOKEN"),
            "must never unset the OAuth token (#2246): {cmd}"
        );
    }

    /// #4181: the relocated line assigns exactly the two variables
    /// `claude_env_scrub::DELIBERATE_SPAWN_ENV` names, and unsets neither.
    ///
    /// Why this is a distinct test rather than an inversion of the one above:
    /// both shapes are production shapes. The non-relocated line is what
    /// `tm launch` emits when the home is unresolvable, and its over-scrub guard
    /// still has to hold; this covers the shape #4181 added.
    #[test]
    fn claude_command_relocated_never_scrubs_what_it_assigns() {
        let dir = Path::new("/tm/claude-config");
        let cmd = build_claude_command_with(None, None, Some(dir), Some("tok"), &[]);
        for name in crate::core::claude_env_scrub::DELIBERATE_SPAWN_ENV {
            assert!(
                cmd.contains(&format!(" {name}=")),
                "the relocated line must ASSIGN {name}: {cmd}"
            );
            assert!(
                !cmd.contains(&format!("-u {name}")),
                "the relocated line must never unset {name} (#4455/#2246): {cmd}"
            );
        }
    }

    /// #4181/#2246: relocating moves which Keychain entry `claude` reads, so the
    /// relocated line carries `CLAUDE_CODE_OAUTH_TOKEN` — and both assignments
    /// come AFTER every `-u` flag, which POSIX `env` requires.
    #[test]
    fn claude_command_relocates_the_config_dir() {
        let dir = Path::new("/tm/claude-config");
        let cmd = build_claude_command_with(None, None, Some(dir), Some("tok-abc"), &[]);
        assert_eq!(
            cmd,
            format!(
                "env{} {} CLAUDE_CONFIG_DIR='/tm/claude-config' \
                 CLAUDE_CODE_OAUTH_TOKEN='tok-abc' claude \
                 --setting-sources user,project,local --dangerously-skip-permissions",
                crate::core::claude_env_scrub::env_unset_flags(),
                crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT
            )
        );
        // POSIX `env` stops parsing options at the first NAME=VALUE, so a
        // `-u` appearing after one would be exec'd as a command and kill the
        // spawn. Read the ordering out of the real line, never restate it.
        let first_assignment = cmd.find('=').expect("the line carries an assignment");
        let last_unset = cmd.rfind("-u ").expect("the line carries scrub flags");
        assert!(
            last_unset < first_assignment,
            "every -u flag must precede every NAME=VALUE assignment: {cmd}"
        );
    }

    /// #4181: a resolvable config dir with NO token still launches — the token
    /// is an optional bypass, not a precondition.
    #[test]
    fn claude_command_omits_the_oauth_token_when_absent() {
        let dir = Path::new("/tm/claude-config");
        let cmd = build_claude_command_with(None, None, Some(dir), None, &[]);
        assert!(
            cmd.contains("CLAUDE_CONFIG_DIR='/tm/claude-config'"),
            "the config dir must still be relocated: {cmd}"
        );
        assert!(
            !cmd.contains("CLAUDE_CODE_OAUTH_TOKEN"),
            "no token resolved → no assignment at all: {cmd}"
        );
        assert!(
            cmd.contains("--setting-sources user,project,local"),
            "the tier list follows the config dir, not the token: {cmd}"
        );
    }

    /// #4181: a config dir containing a space survives the pane shell intact.
    ///
    /// An unquoted `/Users/John Doe/…` word-splits, `env` sees a truncated
    /// assignment plus a stray argv entry, and the pane dies with nothing
    /// surfaced — the same failure `runtime::claude_code` quotes against.
    #[test]
    fn claude_command_quotes_a_config_dir_with_a_space() {
        let dir = Path::new("/Users/John Doe/.trusty-tools/claude-config");
        let cmd = build_claude_command_with(None, None, Some(dir), None, &[]);
        assert!(
            cmd.contains("CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/claude-config'"),
            "the config dir must be single-quoted: {cmd}"
        );
    }

    /// #4181 / ADR-0042: a non-empty `mcp_env` reaches the `tm launch` /
    /// `tm connect` command string.
    ///
    /// Why: with the `.mcp.json` injectors deleted, this assignment is the only
    /// thing that pins the palace and index for a session spawned through this
    /// builder. Every other test here passes `&[]`, so the carrier itself was
    /// untested. The space in the palace value would break the command line if
    /// the assignment were not single-quoted.
    /// Test: itself.
    #[test]
    fn claude_command_carries_a_non_empty_mcp_env() {
        let mcp_env = vec![
            (
                "TRUSTY_MEMORY_PALACE".to_owned(),
                "owner repo slug".to_owned(),
            ),
            ("TRUSTY_INDEX".to_owned(), "idx-42".to_owned()),
        ];
        let cmd = build_claude_command_with(
            None,
            None,
            Some(Path::new("/tm/claude-config")),
            None,
            &mcp_env,
        );

        assert!(
            cmd.contains(" TRUSTY_MEMORY_PALACE='owner repo slug'"),
            "the palace pin must be assigned and single-quoted: {cmd}"
        );
        assert!(
            cmd.contains(" TRUSTY_INDEX='idx-42'"),
            "the index pin must be assigned and single-quoted: {cmd}"
        );
        assert!(
            cmd.find("TRUSTY_INDEX=").unwrap() < cmd.find(" claude").unwrap(),
            "both pins must precede the binary, or they are argv rather than env: {cmd}"
        );
    }

    /// #4181 + #1269: the relocated line loads the `user` tier, and that tier
    /// resolves to the tm-owned config home — so the operator's own `~/.claude`
    /// settings and hooks stay excluded, by relocation instead of by exclusion.
    ///
    /// This is the isolation proof the relocation rests on: a source that SHOULD
    /// be excluded still is. Both halves must hold together — `user` in the tier
    /// list with NO `CLAUDE_CONFIG_DIR` assignment would be the rejected
    /// alternative (adding user scope while still reading the operator's real
    /// `~/.claude`), which is exactly what this pins against.
    #[test]
    fn claude_command_relocated_isolates_by_relocation_not_exclusion() {
        let dir = Path::new("/tm/claude-config");
        let cmd = build_claude_command_with(None, None, Some(dir), None, &[]);
        assert!(
            cmd.contains("--setting-sources user,project,local"),
            "the relocated line must load the user tier: {cmd}"
        );
        let tiers = setting_source_tiers_of(setting_sources_flag(Some(dir)));
        assert!(tiers.contains(&"user"), "{tiers:?}");
        // The `user` tier the line loads is the tm-owned dir, not `~/.claude`:
        // the assignment that redirects it must precede the `claude` program
        // word, or `env` passes it to `claude` as an argument instead of
        // exporting it.
        let assignment = cmd
            .find("CLAUDE_CONFIG_DIR='/tm/claude-config'")
            .expect("the relocated line must redirect the user tier");
        let program = cmd.find(" claude ").expect("the line spawns claude");
        assert!(
            assignment < program,
            "CLAUDE_CONFIG_DIR must be exported by env, not passed to claude: {cmd}"
        );
    }

    /// Assert one launch line carries the full scrub and over-scrubs nothing.
    /// Marker names are hard-coded so an emptied shared list cannot make any
    /// caller of this helper pass vacuously.
    fn assert_scrubbed_launch_line(cmd: &str) {
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
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR"),
            "this builder must not touch CLAUDE_CONFIG_DIR at all: {cmd}"
        );
        assert!(
            !cmd.contains("-u CLAUDE_CODE_OAUTH_TOKEN"),
            "must never unset the OAuth token (#2246): {cmd}"
        );
    }

    /// #4467 round 2 HIGH: `tm session start`'s in-place pane line was
    /// hand-built in the `tm` binary with no scrub — a sixth interactive launch
    /// line that silently saved no transcript.
    #[test]
    fn inplace_session_command_scrubs_inherited_session_markers() {
        assert_scrubbed_launch_line(&build_inplace_session_command());
    }

    /// This pin guards the FLAG list: adding `--setting-sources project,local`
    /// here would drop the `user` tier, and this path does not relocate
    /// `CLAUDE_CONFIG_DIR`, so that tier is the operator's own `~/.claude`.
    /// Pinned as a full string so a flag cannot creep in.
    ///
    /// #6495 changed the expected string by adding an `env` operand
    /// (`ALT_SCREEN_SHELL_ASSIGNMENT`) ahead of `claude`. That is deliberate and
    /// does not weaken what this test asserts: the guard is about which SETTINGS
    /// TIERS the line loads, an environment default changes none of them, and
    /// `--dangerously-skip-permissions` is still the only flag. The
    /// `SETTING_SOURCES_FLAG` assertion below remains the sharp edge.
    #[test]
    fn inplace_session_command_keeps_the_permission_flag_and_nothing_else() {
        assert_eq!(
            build_inplace_session_command(),
            format!(
                "env{} {} claude {PERMISSION_MODE_FLAG}",
                crate::core::claude_env_scrub::env_unset_flags(),
                crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT
            )
        );
        assert!(
            !build_inplace_session_command().contains(SETTING_SOURCES_FLAG),
            "must not add --setting-sources: that would drop the user settings tier"
        );
    }

    /// #4467 round 2: `DaemonClient::launch_session` / `connect_session` (the
    /// TUI/bot surface) hand-built their line too. Found by the anti-drift scan,
    /// not by review.
    #[test]
    fn client_session_command_scrubs_inherited_session_markers() {
        assert_scrubbed_launch_line(&build_client_session_command(None));
        assert_scrubbed_launch_line(&build_client_session_command(Some(Path::new("/tmp/p.txt"))));
    }

    #[test]
    fn client_session_command_appends_the_prompt_file() {
        let head = head();
        assert_eq!(build_client_session_command(None), head);
        assert_eq!(
            build_client_session_command(Some(Path::new("/tmp/p.txt"))),
            format!("{head} --append-system-prompt-file /tmp/p.txt")
        );
    }

    /// #6495: every shell launch line this module owns must carry the
    /// classic-renderer default, and must carry it in the form that yields to a
    /// value the pane already exports. The `${NAME-1}` expansion IS the
    /// operator-precedence mechanism, so asserting the exact operand asserts
    /// both halves at once — a bare `NAME=1` would pass a "contains the
    /// variable" check while silently overriding the operator.
    #[test]
    fn claude_command_defaults_the_alternate_screen_off() {
        let operand = crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT;
        for cmd in [
            build_claude_command(None, None, None, &[]),
            build_claude_command(Some("claude-opus-4-5"), None, None, &[]),
            build_claude_command_with(
                None,
                None,
                Some(Path::new("/tm/claude-config")),
                Some("tok"),
                &[],
            ),
        ] {
            assert!(
                cmd.contains(operand),
                "the launch line must default the alternate screen off: {cmd}"
            );
            // POSIX `env`: an assignment before a `-u` makes `env` exec `-u`.
            let first_assignment = cmd.find('=').expect("the line carries an assignment");
            let last_unset = cmd.rfind("-u ").expect("the line carries scrub flags");
            assert!(
                last_unset < first_assignment,
                "the operand must follow every -u flag: {cmd}"
            );
        }
    }

    /// #6495: the two builders that were assignment-free until now. Kept
    /// separate from the pins above so a regression names the path it broke.
    #[test]
    fn inplace_session_command_defaults_the_alternate_screen_off() {
        let cmd = build_inplace_session_command();
        assert!(
            cmd.contains(crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT),
            "the in-place session line must default the alternate screen off: {cmd}"
        );
    }

    #[test]
    fn client_session_command_defaults_the_alternate_screen_off() {
        let operand = crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT;
        assert!(build_client_session_command(None).contains(operand));
        assert!(
            build_client_session_command(Some(Path::new("/tmp/p.txt"))).contains(operand),
            "the prompt-file shape must carry it too"
        );
    }

    #[test]
    fn claude_command_with_prompt() {
        let path = Path::new("/tmp/prompt.txt");
        let cmd = build_claude_command(None, Some(path), None, &[]);
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
        let cmd = build_claude_command(Some("claude-haiku-4-5"), Some(path), None, &[]);
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
        // Why (#1269/step 4): a session that does NOT relocate CLAUDE_CONFIG_DIR
        // must EXCLUDE the operator's global settings by loading only
        // project,local. #4181 narrowed the scope of this assertion from "every
        // spawned session" to the non-relocated shape — the relocated shape's
        // isolation is proved by
        // `claude_command_relocated_isolates_by_relocation_not_exclusion`, which
        // is the same guarantee reached a different way, not a weaker one.
        let cmd = build_claude_command(None, None, None, &[]);
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
        let cmd = build_claude_command(Some("sonnet"), None, None, &[]);
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
        assert_eq!(cmd, format!("{} --model claude-haiku-4-5 {FLAGS}", head()));
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
