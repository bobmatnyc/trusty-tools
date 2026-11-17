//! A session the daemon first sees registers its checkout as a project (#10368).
//!
//! Why: trusty-mpm 1.14.0 registered a checkout only from `tm connect`, on the
//! client. A `tm launch` of a checkout with an origin, any other `POST /sessions`
//! client, and a running tm session a discovery scan adopts all reached the
//! daemon without a `projects.json` row, so `tm secrets assign` answered
//! "project not registered" for a project with a live session.
//! What: [`register_seen_session_project`] runs
//! [`register_session_checkout`] against the daemon's own registry and logs the
//! outcome; [`register_adopted_sessions`] applies it to the tm-launched tmux
//! sessions one discovery scan adopted (never a native `claude` cwd, D50). Neither ever fails the session: a failure is a warn
//! line naming the cause, and the session stays registered.
//! Test: `session_project_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::session::SessionHost;
use crate::daemon::state::DaemonState;
use crate::project::{SessionRegistration, SessionRegistrationError, register_session_checkout};

/// Register the git checkout holding `dir` as a project, from the daemon.
///
/// Why: #10368 — the daemon is the one place every session start and every
/// adopted running session passes through, so registering here covers the
/// paths the client-side `tm connect` hook does not.
/// What: [`register_session_checkout`] against
/// `<framework_root>/project-registry` with the process home as the D50 guard.
/// A new row is logged at info; a row kept for another clone at warn (the issue's
/// "reported, not silently kept"); a directory outside any checkout or in a
/// linked worktree at debug; every error at warn with its cause. The result is
/// returned for tests and is never turned into a session failure.
/// Test: `a_registered_session_registers_its_checkout_as_a_project`,
/// `a_failed_registry_write_leaves_the_session_registered_and_warns`.
pub async fn register_seen_session_project(
    framework_root: &Path,
    dir: &Path,
) -> Result<SessionRegistration, SessionRegistrationError> {
    let registry_dir = crate::project::registry_data_dir_under(framework_root);
    let home = dirs::home_dir();
    let result = register_session_checkout(&registry_dir, dir, home.as_deref()).await;
    match &result {
        Ok(SessionRegistration::Registered { name }) => {
            tracing::info!(project = %name, dir = %dir.display(), "#10368: registered the session's checkout as a project");
        }
        Ok(SessionRegistration::AlreadyRegistered {
            name,
            checkout,
            root,
        }) if checkout.as_ref() != Some(root) => {
            let registered = checkout
                .as_deref()
                .map_or_else(|| "none".to_string(), |c| c.display().to_string());
            tracing::warn!(
                project = %name,
                registered = %registered,
                session_checkout = %root.display(),
                "#10368: project is registered with another checkout; the registry was not changed"
            );
        }
        Ok(outcome) => {
            tracing::debug!(dir = %dir.display(), ?outcome, "#10368: no project row written");
        }
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "#10368: the session's checkout was not registered as a project: {e}");
        }
    }
    result
}

/// Register the checkout of every tm session one discovery scan adopted.
///
/// Why: #10368 — a tm session already running when the daemon starts is first
/// seen by `discover_all`; without this its project stays unregistered. D50: a
/// registered checkout gates owner- and user-scope secret reads, so a plain
/// `claude` run (native, or in an external tmux session) must never register.
/// What: keeps adopted sessions that are tmux-hosted and carry a tm-managed
/// name (`is_managed_session_name`); each one's `project_path`, else its
/// non-empty `workdir`, goes through [`register_seen_session_project`]. Every
/// other session, and one with no directory, is skipped.
/// Test: `only_a_tm_launched_tmux_session_registers_when_adopted`,
/// `a_native_adopted_cwd_writes_no_project_row`.
pub async fn register_adopted_sessions(state: &DaemonState, adopted: &[String]) {
    let dirs: Vec<PathBuf> = state
        .list_sessions()
        .into_iter()
        .filter(|s| adopted.contains(&s.tmux_name))
        // #10368: D50 — only a tmux session tm launched, never a native cwd.
        .filter(|s| {
            s.origin == SessionHost::Tmux
                && crate::core::names::is_managed_session_name(&s.tmux_name)
        })
        .filter_map(|s| {
            s.project_path
                .or_else(|| (!s.workdir.is_empty()).then(|| PathBuf::from(&s.workdir)))
        })
        .collect();
    for dir in dirs {
        // #10368: the outcome is logged inside; a failure never stops the scan.
        let _ = register_seen_session_project(state.framework_root(), &dir).await;
    }
}

#[cfg(test)]
#[path = "session_project_tests.rs"]
mod tests;
