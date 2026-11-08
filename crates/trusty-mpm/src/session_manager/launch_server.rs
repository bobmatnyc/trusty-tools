//! Which tmux server a fresh PM launch creates its session on (#10101, step 6
//! of #8928).
//!
//! Why: a PM on the default server shares it with the Architect's pane, and
//! its sandbox grant would reach that pane. Each workspace gets a server of
//! its own (#8928's owner ruling of 2026-09-30).
//! What: [`launch_target`] answers the driver for the workspace's server and
//! the socket path the record stores, or `None` for the default server: when
//! [`WORKSPACE_TMUX_ENV`] is `0` (the one-release opt-out, ruling Q4), or when
//! the driver has no socket directory or cannot reach a named server (a test
//! fake). Existing records are never moved (ruling Q3): only a create calls
//! this.
//! Test: `launch_server_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::manager::ManagedTmuxDriver;

/// `TM_WORKSPACE_TMUX=0` launches fresh PMs on the default server, as before
/// #10101. Any other value, or none, uses the workspace's server.
pub const WORKSPACE_TMUX_ENV: &str = "TM_WORKSPACE_TMUX";

/// Whether `value` (the opt-out variable's value) keeps the default server.
/// Test: `the_opt_out_keeps_the_default_server`.
fn opted_out(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "0")
}

/// The driver on `workspace`'s server and that server's socket path, or `None`
/// to launch on the default server.
///
/// What: `None` when [`WORKSPACE_TMUX_ENV`] is `0`, `tmux` has no socket
/// directory, or `tmux` cannot reach a named server. Otherwise the server is
/// [`crate::core::tmux_server::workspace_server_name`] for the project root
/// that owns `workspace` ([`server_workspace`]) and the
/// socket is that name in `tmux`'s socket directory, the path the routing
/// accepts (`tmux_route::route`).
/// Test: `a_fresh_launch_creates_its_session_on_the_workspace_server`,
/// `the_opt_out_keeps_the_default_server`,
/// `two_worktrees_of_one_project_share_one_server`.
pub(crate) fn launch_target(
    tmux: &dyn ManagedTmuxDriver,
    workspace: &Path,
) -> Option<(Arc<dyn ManagedTmuxDriver>, PathBuf)> {
    let opt_out = std::env::var(WORKSPACE_TMUX_ENV).ok();
    launch_target_with(tmux, workspace, opt_out.as_deref())
}

/// [`launch_target`] with the opt-out variable's value passed in, so a test
/// never sets process-global environment.
fn launch_target_with(
    tmux: &dyn ManagedTmuxDriver,
    workspace: &Path,
    opt_out: Option<&str>,
) -> Option<(Arc<dyn ManagedTmuxDriver>, PathBuf)> {
    if opted_out(opt_out) {
        return None;
    }
    let dir = tmux.socket_dir()?;
    let name = crate::core::tmux_server::workspace_server_name(&server_workspace(workspace));
    let driver = tmux.on_server(&name)?;
    Some((
        driver,
        crate::core::tmux_server::server_socket_path(&dir, &name),
    ))
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
