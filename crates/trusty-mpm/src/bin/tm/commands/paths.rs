//! `tm paths show|set` — the install's host paths (#11821 P6b).
//!
//! Why: `~/.trusty-tools/install.toml` relocates the projects root and the
//! config dir, so it is a trust anchor (Architect ruling, 2026-11-26). The
//! console and MCP never write it; this owner-run command is the one write
//! path, built over `trusty_common::install_paths::write_install_file`.
//! What: [`show`] prints each path with the source that won (env,
//! install-file, config-template, default); a broken install file is an error
//! and no default is printed. [`set`] refuses inside a Claude Code session or
//! an agent ([`refuse_command_inside_claude_code`], `CLAUDE_MPM_SUB_AGENT`),
//! refuses while any tm session is live or the live set cannot be read
//! (Architect D1), merges the flags over the file's other keys, and writes
//! through `write_install_file`, whose validation refuses a relative or empty
//! value. It never moves data. The pm-guard floor denies a session that runs
//! `tm paths set` (`pm_guard_paths_set`) and the trust-anchor floor denies a
//! direct edit of the file.
//! Test: `paths_tests.rs`; end to end in `tests/tm_paths_11821.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use trusty_common::install_paths::{
    InstallFile, InstallPaths, PathOverrides, ResolvedPath, install_file_path, read_install_file,
    write_install_file,
};
use trusty_mpm::core::tmux_server;

use crate::cli::PathsAction;
use crate::commands::fleet::refuse_command_inside_claude_code;
use crate::commands::misc::SUB_AGENT_ENV;

/// The issue every refusal cites.
const ISSUE: &str = "#11821";

/// Dispatch `tm paths <action>` over this process's env, home and tmux.
pub(crate) fn run(action: PathsAction) -> anyhow::Result<()> {
    let home = dirs::home_dir().context("cannot resolve the home directory")?;
    let out = match action {
        PathsAction::Show => show(&home, |n| std::env::var(n).ok(), live_template)?,
        PathsAction::Set {
            projects_root,
            config_dir,
        } => set(
            &SetRequest {
                projects_root,
                config_dir,
            },
            &home,
            |n| std::env::var_os(n),
            live_tm_sessions,
        )?,
    };
    print!("{out}");
    Ok(())
}

/// The `config.yaml` `workspace_root_template`, read only after the install
/// file parsed.
fn live_template() -> Option<String> {
    trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load().workspace_root_template
}

/// Render the resolved paths under `home`.
///
/// Why: an operator must see which source won, and a broken install file
/// must never read as the `$HOME` default (#11821 P6b).
/// What: reads the install file first and returns its error, so a broken
/// file prints no path at all, even when env decides both keys; then
/// [`InstallPaths::resolve_with`] over `env` with `template()` as the
/// template rung. One line per key: name, path, source.
/// Test: `show_prints_each_path_with_its_source`,
/// `show_on_a_broken_install_file_is_the_error_not_the_default`.
pub(crate) fn show(
    home: &Path,
    env: impl Fn(&str) -> Option<String>,
    template: impl FnOnce() -> Option<String>,
) -> anyhow::Result<String> {
    let file = install_file_path(home);
    // #11821 P6b: the file's error, never a default, on a broken install.toml.
    let present = read_install_file(home)
        .with_context(|| broken(&file))?
        .is_some();
    let overrides = PathOverrides {
        projects_root_template: template(),
        ..PathOverrides::default()
    };
    let paths = InstallPaths::resolve_with(env, home, &overrides)
        .context("the install paths do not resolve; no default is shown (#11821)")?;
    let state = if present { "present" } else { "absent" };
    let mut out = format!("install file       {} ({state})\n", file.display());
    out.push_str(&row("projects_root", Some(&paths.projects_root), ""));
    out.push_str(&row("config_dir", Some(&paths.config_dir), ""));
    out.push_str(&row(
        "claude_config_dir",
        paths.claude_config_dir.as_ref(),
        "unset (Claude Code's default)",
    ));
    out.push_str(&row(
        "codex_home",
        paths.codex_home.as_ref(),
        "unset (Codex's default)",
    ));
    Ok(out)
}

/// One `show` line.
fn row(key: &str, hit: Option<&ResolvedPath>, unset: &str) -> String {
    match hit {
        Some(hit) => format!("{key:<18} {} ({})\n", hit.path.display(), hit.source),
        None => format!("{key:<18} {unset}\n"),
    }
}

/// The remedy a broken install file carries.
fn broken(file: &Path) -> String {
    format!(
        "{} is broken or unreadable; fix or remove it by hand from your own shell ({ISSUE})",
        file.display()
    )
}

/// The flags of one `tm paths set`.
#[derive(Debug, Default)]
pub(crate) struct SetRequest {
    /// `--projects-root`.
    pub(crate) projects_root: Option<PathBuf>,
    /// `--config-dir`.
    pub(crate) config_dir: Option<PathBuf>,
}

/// Write the flags into `<home>/.trusty-tools/install.toml`.
///
/// Why: the owner's one write path for a trust anchor (#11821 P6b); a hooked
/// session or an agent must not reach it even past a guard-parser bypass, and
/// a move under live sessions would split their state (Architect D1).
/// What: in order — at least one flag; no Claude Code session marker in
/// `env` and no `CLAUDE_MPM_SUB_AGENT`; `live()` answers no session (an
/// error refuses too); the existing file parses (a broken one is not merged
/// over); then [`write_install_file`] with the flags over the file's other
/// keys. Its `InstallPathsError` (`Relative`, `Empty`, `Write`) is returned
/// as is. Nothing is created under, or moved to, the new paths.
/// Test: `set_refuses_inside_a_claude_code_session`,
/// `set_refuses_an_agent`, `set_refuses_while_sessions_are_live`,
/// `set_refuses_when_live_sessions_cannot_be_read`,
/// `set_refuses_a_relative_path_with_the_typed_error`,
/// `set_refuses_to_merge_over_a_broken_install_file`,
/// `set_writes_through_the_writer_and_keeps_other_keys`,
/// `set_without_a_flag_is_refused`.
pub(crate) fn set(
    req: &SetRequest,
    home: &Path,
    env: impl Fn(&str) -> Option<OsString>,
    live: impl FnOnce() -> Result<Vec<String>, String>,
) -> anyhow::Result<String> {
    if req.projects_root.is_none() && req.config_dir.is_none() {
        bail!("tm paths set needs --projects-root, --config-dir or both");
    }
    // #11821 P6b: owner-only, checked before anything is read.
    refuse_command_inside_claude_code("tm paths set", "install.toml was not written", ISSUE, &env)?;
    if env(SUB_AGENT_ENV).is_some() {
        bail!(
            "tm paths set refused: `{SUB_AGENT_ENV}` is set, so an agent runs it. install.toml \
             was not written. Run `tm paths set` from an operator terminal ({ISSUE})"
        );
    }
    // #11821 D1: no write while a session could hold the old paths.
    match live() {
        Ok(names) if names.is_empty() => {}
        Ok(names) => bail!(
            "tm paths set refused: {} tm session(s) are live ({}). Stop them first; install.toml \
             was not written ({ISSUE}, D1)",
            names.len(),
            names.join(", ")
        ),
        Err(why) => bail!(
            "tm paths set refused: cannot tell whether tm sessions are live ({why}). \
             install.toml was not written ({ISSUE}, D1)"
        ),
    }
    let file = install_file_path(home);
    let old = read_install_file(home)
        .with_context(|| broken(&file))?
        .unwrap_or_default();
    let values = InstallFile {
        projects_root: req.projects_root.clone().or(old.projects_root.clone()),
        config_dir: req.config_dir.clone().or(old.config_dir.clone()),
        ..old.clone()
    };
    let (path, stored) = write_install_file(home, &values)?;
    let mut out = format!("wrote {}\n", path.display());
    for (key, before, after) in [
        ("projects_root", &old.projects_root, &stored.projects_root),
        ("config_dir", &old.config_dir, &stored.config_dir),
    ] {
        if let Some(after) = after {
            let before = before
                .as_ref()
                .map_or_else(|| "unset".to_owned(), |b| b.display().to_string());
            out.push_str(&format!("{key}: {before} -> {}\n", after.display()));
        }
    }
    out.push_str(
        "Nothing was moved: move existing data to the new paths yourself before starting a \
         session. `tm paths show` names the source that wins; an env var outranks the file.\n",
    );
    Ok(out)
}

/// The live tm sessions: `tm-*` on the default tmux server, and every session
/// on a live workspace server (`tm-ws-*`).
///
/// What: no tmux binary, or no server, is zero sessions; any other failure,
/// a timeout included, is `Err`, which [`set`] treats as a refusal.
fn live_tm_sessions() -> Result<Vec<String>, String> {
    use trusty_mpm::core::tmux::run_tmux_argv_bounded;
    let dir = tmux_server::ambient_socket_dir();
    let servers = tmux_server::live_workspace_servers(&dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    let mut live = Vec::new();
    let all = std::iter::once(None).chain(servers.iter().map(|s| Some(s.as_str())));
    for server in all {
        let list = ["list-sessions", "-F", "#{session_name}"].map(str::to_owned);
        let argv = tmux_server::server_argv(server, list);
        let out = match run_tmux_argv_bounded(&argv, tmux_server::PROBE_BUDGET) {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(live),
            Err(e) => return Err(format!("tmux: {e}")),
        };
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("no server running") || err.contains("error connecting") {
                continue;
            }
            return Err(format!("tmux list-sessions: {}", err.trim()));
        }
        for name in String::from_utf8_lossy(&out.stdout).lines() {
            if server.is_some() || name.starts_with("tm-") {
                live.push(server.map_or_else(|| name.to_owned(), |s| format!("{s}:{name}")));
            }
        }
    }
    Ok(live)
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
