//! The daemon's implementation of the automatic-resume runtime relaunch
//! (#8233, owner ruling 2026-09-18).
//!
//! Why: `session_manager` sits below the daemon and cannot resolve a pinned
//! `gh` identity (that needs the project registry) or build a runtime adapter
//! against daemon-resolved inputs. The manager therefore owns only the SEAM
//! (`session_manager::relaunch::RuntimeRelauncher`); this module is the body,
//! and it deliberately runs the same two steps `resume_managed` does —
//! `RuntimeAdapter::spawn_resume`, then `launch_verify` — so an auto-resume and
//! an operator resume cannot diverge in what they put in the pane.
//!
//! What: [`DaemonRelauncher`] holds a `Weak<DaemonState>` (the state owns the
//! session manager, which owns this, so a strong handle would be a cycle that
//! never drops). A dropped state answers `Err`, which the manager turns into an
//! errored record rather than a silently-Active one.
//!
//! Test: `crates/trusty-mpm/src/daemon/managed_routes/auto_relaunch_tests.rs`.

use std::sync::{Arc, Weak};

use crate::daemon::state::DaemonState;
use crate::session_manager::SessionRecord;
use crate::session_manager::relaunch::RuntimeRelauncher;

/// Relaunch a session's runtime through the daemon's own adapter path.
///
/// Why/What: see this module's header.
/// Test: `auto_relaunch_tests.rs`.
pub(crate) struct DaemonRelauncher {
    state: Weak<DaemonState>,
}

impl DaemonRelauncher {
    /// Wrap a daemon state as a relauncher.
    ///
    /// Test: `install_auto_relauncher` is exercised by the boot path.
    pub(crate) fn new(state: &Arc<DaemonState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

#[async_trait::async_trait]
impl RuntimeRelauncher for DaemonRelauncher {
    /// Put a verified runtime back into `record`'s pane.
    ///
    /// What: resolves the workspace the same way the record does
    /// (`workspace_path` → `cwd`), resolves the pinned `gh` identity, builds the
    /// adapter for the record's OWN runtime kind, calls `spawn_resume` into the
    /// record's OWN pane, and then runs the post-send verification. Every
    /// failure is an `Err(message)` — never a warning beside a record left
    /// `Active`, which is the defect this exists to close.
    /// Test: `a_relaunch_that_finds_no_runtime_reports_an_error` and
    /// `a_dropped_daemon_state_fails_the_relaunch` in `auto_relaunch_tests.rs`.
    async fn relaunch(&self, record: &SessionRecord) -> Result<(), String> {
        let Some(state) = self.state.upgrade() else {
            return Err("the daemon state is gone, so no runtime could be started".to_owned());
        };
        let mgr = state.session_manager().await;
        let workspace = record
            .workspace_path
            .clone()
            .unwrap_or_else(|| record.cwd.clone());
        let tmux = mgr.tmux_driver();
        let gh_env = super::lifecycle::resolve_gh_env(&state, &workspace).await;
        let adapter = crate::runtime::build_adapter(record.runtime, tmux.clone(), None);
        adapter
            .spawn_resume(
                &record.tmux_name,
                record.pane_id.as_deref(),
                &workspace,
                &record.task,
                record.claude_session_id.as_deref(),
                &record.id.to_string(),
                &gh_env,
            )
            .map_err(|e| format!("runtime adapter spawn_resume failed: {e}"))?;
        // The SAME verification the interactive resume runs: a record becomes
        // `Active` only behind a runtime this actually saw.
        match super::launch_verify::record_resume_outcome(&mgr, tmux.as_ref(), record, &workspace)
            .await
        {
            Some(msg) => Err(msg),
            None => Ok(()),
        }
    }
}

/// Install [`DaemonRelauncher`] on this daemon's session manager.
///
/// Why: the manager is built lazily, so the install happens the first time the
/// daemon has both halves in hand. Idempotent — `install_relauncher` keeps the
/// first.
/// What: no-op when the manager already has one.
/// Test: `installing_a_relauncher_twice_keeps_the_first` in
/// `session_manager::relaunch`'s tests covers the idempotence.
pub async fn install_auto_relauncher(state: &Arc<DaemonState>) {
    let mgr = state.session_manager().await;
    mgr.install_relauncher(Arc::new(DaemonRelauncher::new(state)));
}

#[cfg(test)]
#[path = "auto_relaunch_tests.rs"]
mod tests;
