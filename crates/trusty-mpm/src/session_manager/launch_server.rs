//! Which tmux server a fresh PM launch creates its session on (#10101, step 6
//! of #8928).
//!
//! Why: a fresh PM launches on the default tmux server. The per-workspace
//! server of #8928/#10101 is opt-in: the owner ruling of 2026-11-18
//! supersedes the 2026-09-30 per-workspace default on this host.
//! What: [`launch_target`] answers the driver for the workspace's server and
//! the socket path the record stores only when [`WORKSPACE_TMUX_ENV`] is `1`
//! and the driver has a socket directory and reaches a named server; else
//! `None`, the default server. Only a create calls this; a resume
//! moves a stopped record in `resume_server` (#10101).
//! Test: `launch_server_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::manager::ManagedTmuxDriver;

/// `TM_WORKSPACE_TMUX=1` launches fresh PMs on the workspace's own server.
/// Any other value, or none, uses the default server (#10101, owner ruling
/// 2026-11-18).
pub const WORKSPACE_TMUX_ENV: &str = "TM_WORKSPACE_TMUX";

/// Whether `value` (the opt-in variable's value) asks for the workspace server.
/// Test: `with_no_env_a_launch_stays_on_the_default_server`.
// #10101: opt-in, not opt-out — owner ruling 2026-11-18.
pub(crate) fn opted_in(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

/// The driver on `workspace`'s server and that server's socket path, or `None`
/// to launch on the default server.
///
/// What: `None` unless [`WORKSPACE_TMUX_ENV`] is `1`, or when `tmux` has no socket
/// directory, or `tmux` cannot reach a named server. Otherwise the server is
/// [`crate::core::tmux_server::workspace_server_name`] for the project root
/// that owns `workspace` ([`server_workspace`]) and the
/// socket is that name in `tmux`'s socket directory, the path the routing
/// accepts (`tmux_route::route`).
/// Test: `with_no_env_a_launch_stays_on_the_default_server`,
/// `a_fresh_launch_creates_its_session_on_the_workspace_server`,
/// `only_the_opt_in_reaches_the_workspace_server`,
/// `two_worktrees_of_one_project_share_one_server`.
pub(crate) fn launch_target(
    tmux: &dyn ManagedTmuxDriver,
    workspace: &Path,
) -> Option<(Arc<dyn ManagedTmuxDriver>, PathBuf)> {
    let opt_in = std::env::var(WORKSPACE_TMUX_ENV).ok();
    launch_target_with(tmux, workspace, opt_in.as_deref())
}

/// [`launch_target`] with the opt-in variable's value passed in, so a test
/// never sets process-global environment.
fn launch_target_with(
    tmux: &dyn ManagedTmuxDriver,
    workspace: &Path,
    opt_in: Option<&str>,
) -> Option<(Arc<dyn ManagedTmuxDriver>, PathBuf)> {
    if !opted_in(opt_in) {
        return None;
    }
    let dir = tmux.socket_dir()?;
    let name = server_name_for(workspace);
    let driver = tmux.on_server(&name)?;
    Some((
        driver,
        crate::core::tmux_server::server_socket_path(&dir, &name),
    ))
}

/// The `-L` server a CLI launch (`tm launch`, `tm connect`, the in-place
/// `tm session start`) from `workspace` creates its PM on, and that server's
/// socket; `None` for the default server (#10101).
///
/// Why: a PM the CLI starts must land on the same server the daemon would
/// pick for that workspace, so the two launch paths share one rule.
/// What: [`cli_launch_server_with`] with [`WORKSPACE_TMUX_ENV`] read from the
/// process environment and tmux's socket directory for this process
/// ([`crate::core::tmux_server::ambient_socket_dir`]).
/// Test: `a_cli_launch_names_the_daemons_workspace_server`,
/// `the_cli_needs_the_opt_in_for_the_workspace_server`.
pub fn cli_launch_server(workspace: &Path) -> Option<(String, PathBuf)> {
    let opt_in = std::env::var(WORKSPACE_TMUX_ENV).ok();
    let dir = crate::core::tmux_server::ambient_socket_dir();
    cli_launch_server_with(workspace, opt_in.as_deref(), &dir)
}

/// [`cli_launch_server`] with the opt-in value and socket directory passed
/// in: `None` unless opted in, else [`server_name_for`] `workspace` and its
/// socket in `socket_dir`.
fn cli_launch_server_with(
    workspace: &Path,
    opt_in: Option<&str>,
    socket_dir: &Path,
) -> Option<(String, PathBuf)> {
    if !opted_in(opt_in) {
        return None;
    }
    let name = server_name_for(workspace);
    let socket = crate::core::tmux_server::server_socket_path(socket_dir, &name);
    Some((name, socket))
}

/// The server name a fresh PM at `workspace` launches on, for both the
/// daemon's [`launch_target`] and the CLI's [`cli_launch_server`].
fn server_name_for(workspace: &Path) -> String {
    crate::core::tmux_server::workspace_server_name(&server_workspace(workspace))
}

/// The workspace whose server a session at `cwd` launches on: the project
/// root that owns `cwd`'s git worktree registry, so every PM of one project
/// shares one server; `cwd` itself outside a repository (#10101, ruling Dc1
/// of 2026-10-20, per-workspace `tmux -L`).
/// Test: `two_worktrees_of_one_project_share_one_server`.
fn server_workspace(cwd: &Path) -> PathBuf {
    super::worktree_scope::project_root_for(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

#[cfg(test)]
#[path = "launch_server_tests.rs"]
mod tests;
