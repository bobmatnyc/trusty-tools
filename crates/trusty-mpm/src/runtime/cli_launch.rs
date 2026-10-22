//! The CLI and fleet `claude` launches, carried to the pane as a launch spec
//! (#8308).
//!
//! Why: `tm launch`, `tm connect`, the in-place `tm session start`,
//! `DaemonClient::launch_session`/`connect_session` and the Architect launch in
//! `tm fleet` typed one composed `env -u … NAME=VALUE … claude --flags` line into
//! the pane. With a relocated config dir, the OAuth token and a supervisor's
//! divert pins that line reached 1229 bytes, over
//! [`crate::core::tmux::MAX_PANE_COMMAND_BYTES`], and it put
//! `CLAUDE_CODE_OAUTH_TOKEN` into the pane's scrollback in cleartext. #8233
//! closed the same class for the daemon's managed launch by moving every
//! parameter into a mode-0600 [`LaunchSpec`] file; this module applies that
//! mechanism to these six sites.
//! What: [`isolated_spec`], [`inplace_spec`] and [`client_spec`] compose the
//! variables and flags the matching `core::model_inject` string builders emit,
//! as a [`LaunchSpec`]. [`send_spec_launch`] writes the spec, reads it back and
//! types [`pane_line`] — `'<tm>' internal-spawn-disclaimed --launch-spec
//! '<file>'` — whose length does not grow with the launch.
//! Test: `crates/trusty-mpm/src/runtime/cli_launch_tests.rs`.

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::launch_spec::{LaunchSpec, LaunchSpecError};

/// The program a CLI launch runs: `claude`, resolved through the pane's `PATH`
/// exactly as the `env … claude` line it replaces resolved it.
const CLAUDE_PROGRAM: &str = "claude";

/// What can stop a CLI launch from reaching its pane.
///
/// Why: each caller turns a failure into its own message and cleanup (a killed
/// session, a bail, a warning), so the step that failed has to be named.
/// What: one variant per step of [`send_spec_launch`].
/// Test: `send_spec_launch_removes_the_spec_when_tmux_fails`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CliLaunchError {
    /// This `tm` binary's own path could not be read, so nothing can read the
    /// spec in the pane.
    #[error("could not resolve this `tm` binary's own path to read the launch spec with (#8308)")]
    NoWrapper,
    /// The spec could not be written or read back.
    #[error(transparent)]
    Spec(#[from] LaunchSpecError),
    /// tmux could not type the line, or exited non-zero.
    #[error("tmux could not type the launch line: {0}")]
    Send(std::io::Error),
}

/// Inputs of an isolated CLI launch: `tm launch`, `tm connect`, the Architect.
///
/// Why: the three callers share one composition (`build_claude_command_with_configured`
/// in string form), and one borrowed struct keeps the builder a single call.
/// What: plain borrows. `oauth_token` is passed in, never resolved here, so a
/// test composes the worst case with a placeholder.
/// Test: `isolated_spec_matches_the_shell_line_it_replaces`.
#[derive(Debug, Clone, Copy)]
pub struct CliLaunch<'a> {
    /// `--model <model>`; `None` for `tm connect`.
    pub model: Option<&'a str>,
    /// `--append-system-prompt-file <path>` when a prompt was written.
    pub prompt_file: Option<&'a Path>,
    /// The relocated `CLAUDE_CONFIG_DIR` (#4181); picks `--setting-sources`.
    pub config_dir: Option<&'a Path>,
    /// `CLAUDE_CODE_OAUTH_TOKEN` (#2246); only meaningful with `config_dir`.
    pub oauth_token: Option<&'a str>,
    /// The per-project MCP pins and the profile stamp (#4181, #8453).
    pub mcp_env: &'a [(String, String)],
    /// The composed session MCP file (#7422).
    pub scoped_mcp: Option<&'a Path>,
    /// The owner `--settings` file the caller provisioned (#8988).
    pub settings: Option<&'a Path>,
    /// Config `tmux.alternate_screen` (#8405).
    pub alternate_screen: bool,
}

/// A spec with the shared scrub and renderer, rooted at `cwd`, and no argv.
///
/// Why: all three shapes unset the same inherited markers (#4467/#8453/#8583)
/// and assign the configured renderer (#8405); the mouse default is applied by
/// [`LaunchSpec::to_command`], which yields to a value the pane exports.
/// What: empty `session_id` and `launch_id` — a CLI launch has no daemon
/// handshake, so the shim neither exports `TM_MANAGED_SESSION_ID` nor writes a
/// started sentinel for it.
fn base(cwd: &Path, alternate_screen: bool) -> LaunchSpec {
    LaunchSpec {
        session_id: String::new(),
        launch_id: String::new(),
        cwd: cwd.to_path_buf(),
        program: CLAUDE_PROGRAM.to_owned(),
        args: Vec::new(),
        env_unset: crate::core::claude_env_scrub::scrubbed_on_spawn()
            .map(str::to_owned)
            .collect(),
        env_set: crate::core::alt_screen::configured_env(alternate_screen),
    }
}

/// The spec for `tm launch`, `tm connect` and the Architect launch.
///
/// Why: the structured twin of
/// [`crate::core::model_inject::build_claude_command_with_configured`], so the
/// token and the paths leave the typed line.
/// What: [`base`], then `CLAUDE_CONFIG_DIR`, the OAuth token, `mcp_env` and,
/// for a PM, the #8992 trim variable, in that order; argv `[--model m]
/// [--append-system-prompt-file p] --setting-sources … [--mcp-config f]
/// [--settings s] [--disallowedTools …] --dangerously-skip-permissions`, every token unquoted
/// because no shell parses it. The profile is the one `mcp_env`'s stamp names.
/// #10127: the shipped [`crate::core::architect_lockdown::Lockdown::PRODUCTION`]
/// applies ([`isolated_spec_with`]); its error refuses the launch.
/// Test: `isolated_spec_matches_the_shell_line_it_replaces`,
/// `worst_case_cli_launch_line_is_short_and_carries_no_token`,
/// `an_architect_isolated_spec_keeps_every_tool`.
pub fn isolated_spec(
    cwd: &Path,
    launch: &CliLaunch<'_>,
) -> Result<LaunchSpec, crate::core::architect_lockdown::LockdownError> {
    isolated_spec_with(
        cwd,
        launch,
        &crate::core::architect_lockdown::Lockdown::PRODUCTION,
    )
}

/// [`isolated_spec`] under the named `lockdown` (#10127).
///
/// What: an Architect's `--settings` pair is
/// [`crate::core::architect_lockdown::Lockdown::settings_argv`] (the owner
/// file and the deny as one payload), and its env loses the lockdown's
/// variables — unset, and dropped from `env_set`. A PM launch is unchanged.
/// Test: `architect_launch_carries_every_glob_and_its_capitalised_variant`,
/// `architect_launch_unsets_the_env_list_and_a_pm_launch_does_not`,
/// `a_carrier_file_that_cannot_be_read_refuses_the_architect_launch`.
pub(crate) fn isolated_spec_with(
    cwd: &Path,
    launch: &CliLaunch<'_>,
    lockdown: &crate::core::architect_lockdown::Lockdown<'_>,
) -> Result<LaunchSpec, crate::core::architect_lockdown::LockdownError> {
    let mut spec = base(cwd, launch.alternate_screen);
    if let Some(dir) = launch.config_dir {
        spec.env_set
            .push(("CLAUDE_CONFIG_DIR".to_owned(), dir.display().to_string()));
    }
    if let Some(token) = launch.oauth_token {
        spec.env_set.push((
            crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR.to_owned(),
            token.to_owned(),
        ));
    }
    spec.env_set.extend(launch.mcp_env.iter().cloned());
    // #8992: the stamp in `mcp_env` names the profile; a PM drops unused tools.
    let profile = crate::core::pm_tool_trim::stamped_profile(launch.mcp_env);
    spec.env_set
        .extend(crate::core::pm_tool_trim::trim_env(profile));
    if let Some(model) = launch.model {
        spec.args.extend(["--model".to_owned(), model.to_owned()]);
    }
    push_prompt(&mut spec.args, launch.prompt_file);
    spec.args
        .extend(split(crate::core::model_inject::setting_sources_flag(
            launch.config_dir,
        )));
    spec.args
        .extend(crate::core::session_mcp_scope::mcp_config_argv(
            launch.scoped_mcp,
        ));
    // #8988: the owner MCP policy, right after the file it filters.
    // #10127: an Architect's deny joins it as ONE `--settings` (last one wins).
    spec.args
        .extend(lockdown.settings_argv(profile, launch.settings)?);
    spec.args
        .extend(crate::core::pm_tool_trim::trim_args(profile));
    spec.args
        .extend(split(crate::core::model_inject::PERMISSION_MODE_FLAG));
    // #10127: an Architect child inherits none of the lockdown's variables.
    lockdown.scrub_spec_env(profile, &mut spec);
    Ok(spec)
}

/// The spec for the in-place `tm session start`.
///
/// Why: the structured twin of
/// [`crate::core::model_inject::build_inplace_session_command_with_prompt`] —
/// no `--setting-sources`, so the operator's own `user` tier still loads.
/// What: [`base`] plus `--append-system-prompt-file <prompt_file>
/// --dangerously-skip-permissions`. `prompt_file` is required, not optional
/// (#8286): every PM launch mode delivers its prompt through that flag, so a
/// spec without it cannot be built.
/// Test: `inplace_and_client_specs_match_the_lines_they_replace`.
pub fn inplace_spec(cwd: &Path, prompt_file: &Path, alternate_screen: bool) -> LaunchSpec {
    let mut spec = base(cwd, alternate_screen);
    push_prompt(&mut spec.args, Some(prompt_file));
    spec.args
        .extend(split(crate::core::model_inject::PERMISSION_MODE_FLAG));
    spec
}

/// The spec for `DaemonClient::launch_session`/`connect_session`.
///
/// Why: the structured twin of
/// [`crate::core::model_inject::build_client_session_command_configured`].
/// What: [`base`] plus `--append-system-prompt-file <path>` when a prompt was
/// written.
/// Test: `inplace_and_client_specs_match_the_lines_they_replace`.
pub fn client_spec(cwd: &Path, prompt_file: Option<&Path>, alternate_screen: bool) -> LaunchSpec {
    let mut spec = base(cwd, alternate_screen);
    push_prompt(&mut spec.args, prompt_file);
    spec
}

/// Append `--append-system-prompt-file <path>` when a prompt was written.
fn push_prompt(args: &mut Vec<String>, prompt_file: Option<&Path>) {
    if let Some(prompt) = prompt_file {
        args.push("--append-system-prompt-file".to_owned());
        args.push(prompt.display().to_string());
    }
}

/// A flag constant's whitespace-separated tokens, as owned argv.
fn split(flag: &str) -> impl Iterator<Item = String> + '_ {
    flag.split_whitespace().map(str::to_owned)
}

/// The directory a CLI launch writes its spec to, under the named home.
///
/// Why: the #8545 pinned-home seam — a test that drives a launch must not
/// write a credential-bearing file into the operator's own config home.
/// What: `<home>/.trusty-tools/trusty-mpm/launch-specs` when `home` is `Some`,
/// else [`LaunchSpec::root`]; `None` only when neither resolves.
/// Test: `spec_dir_nests_under_the_named_home`.
pub fn spec_dir(home: Option<&Path>) -> Option<PathBuf> {
    match home {
        Some(home) => Some(LaunchSpec::root_at(
            &crate::core::paths::FrameworkPaths::under(home).crate_config_root(),
        )),
        None => LaunchSpec::root(),
    }
}

/// The whole line a CLI launch types: `'<tm>' internal-spawn-disclaimed
/// --launch-spec '<file>'`.
///
/// Why: its only variable parts are two tm-owned paths, so it stays short
/// whatever the launch carries, and it carries no secret.
/// What: both paths single-quoted, so a home with a space still parses.
/// Test: `worst_case_cli_launch_line_is_short_and_carries_no_token`.
pub fn pane_line(wrapper_bin: &str, spec_path: &Path) -> String {
    let quote = crate::core::spawn_disclaim::pane::shell_single_quote;
    format!(
        "{} {} --launch-spec {}",
        quote(wrapper_bin),
        crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND,
        quote(&spec_path.display().to_string()),
    )
}

/// Write `spec` under `spec_dir` and type its [`pane_line`] into `session`.
///
/// Why: the one call every CLI and fleet launch site makes, so the write,
/// the read-back and the keystrokes cannot be split across callers.
/// What: resolves this `tm` binary
/// ([`crate::core::spawn_disclaim::launch_wrapper_bin`]), then
/// [`send_spec_launch_with`] on the default tmux.
/// Test: `send_spec_launch_types_a_short_line_naming_a_readable_spec`.
pub fn send_spec_launch(
    session: &str,
    spec: &LaunchSpec,
    spec_dir: &Path,
) -> Result<(), CliLaunchError> {
    let wrapper =
        crate::core::spawn_disclaim::launch_wrapper_bin().ok_or(CliLaunchError::NoWrapper)?;
    send_spec_launch_with(None, &wrapper, session, spec, spec_dir)
}

/// [`send_spec_launch`] with the tmux binary and the wrapper named (test seam).
///
/// What: writes the spec (mode 0600), reads it back ([`LaunchSpec::verify`]),
/// then types the line via [`crate::core::tmux::send_launch_line`], which
/// refuses a line over `MAX_PANE_COMMAND_BYTES` before tmux runs (#8308). On
/// any failure after the write it removes the spec, since it carries
/// credentials and no shim will consume it.
/// Test: `send_spec_launch_types_a_short_line_naming_a_readable_spec`,
/// `send_spec_launch_removes_the_spec_when_tmux_fails`,
/// `cli_launch_paths_type_through_the_guard`.
pub(crate) fn send_spec_launch_with(
    tmux_bin: Option<&str>,
    wrapper: &str,
    session: &str,
    spec: &LaunchSpec,
    spec_dir: &Path,
) -> Result<(), CliLaunchError> {
    let path = spec.write_in(spec_dir)?;
    let result = LaunchSpec::verify(&path)
        .map_err(CliLaunchError::from)
        .and_then(|_| {
            crate::core::tmux::send_launch_line(tmux_bin, session, &pane_line(wrapper, &path))
                .map_err(CliLaunchError::Send)
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

#[cfg(test)]
#[path = "cli_launch_tests.rs"]
mod tests;
