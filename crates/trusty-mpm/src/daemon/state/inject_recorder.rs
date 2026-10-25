//! How the inject recorder reaches the session manager (#9167 AC5).
//!
//! Why: building the session manager reconciles the shared store
//! (`reconcile_on_boot`). A process that only wants its injects recorded, such
//! as a goals-only `tm sm serve --stdio` run, must not build it for that.
//! What: the recorder is kept on [`DaemonState`] until a manager exists;
//! [`DaemonState::session_manager`] hands it over when it builds one.
//! Test: `installing_the_recorder_does_not_build_the_session_manager_9167`.

use std::sync::Arc;

use super::DaemonState;
use crate::session_manager::SessionManager;
use crate::session_manager::inject_record::InjectRecorder;

impl DaemonState {
    /// Keep `recorder` for the session manager, without building it.
    ///
    /// What: `false` when a recorder is already kept. A manager already built
    /// takes it now.
    pub fn set_inject_recorder(&self, recorder: Arc<dyn InjectRecorder>) -> bool {
        if !self
            .action_events
            .set_inject_recorder(Arc::clone(&recorder))
        {
            return false;
        }
        if let Some(mgr) = self.managed_sessions.get() {
            mgr.install_inject_recorder(recorder);
        }
        true
    }

    /// Give `mgr` the kept recorder; a repeat is a no-op.
    ///
    /// What: runs after the manager cell is set, so an `install` racing the
    /// build either sees the manager or is seen here.
    pub(super) fn take_inject_recorder(&self, mgr: &SessionManager) {
        if let Some(recorder) = self.action_events.inject_recorder() {
            mgr.install_inject_recorder(Arc::clone(recorder));
        }
    }

    /// Whether the session manager has been built (test probe).
    #[cfg(test)]
    pub fn session_manager_built(&self) -> bool {
        self.managed_sessions.initialized()
    }
}
