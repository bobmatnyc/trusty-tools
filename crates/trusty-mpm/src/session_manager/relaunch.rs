//! The runtime relaunch the AUTOMATIC resume paths were missing (#8233, owner
//! ruling 2026-09-18).
//!
//! Why: `resume_managed` — the interactive path — recreates or reuses the pane
//! and then calls `RuntimeAdapter::spawn_resume` to put a runtime in it. The two
//! automatic paths never did. `supervisor::poller::run_tick` and the boot
//! reconcile tail both call [`SessionManager::resume_auto`], which reached
//! `resume_inner`, got a pane, marked the record `Active`, and stopped. A
//! session whose pane had died was therefore auto-resumed into a BARE SHELL
//! with an `Active` record, which is exactly the fleet state the owner observed
//! after a daemon restart: records marked active, and every later operator
//! resume refused with "cannot resume a session in state 'active'".
//!
//! What: [`RuntimeRelauncher`] is the one-method seam the daemon installs on the
//! manager. It is a trait rather than a direct call because resolving the
//! pinned `gh` identity needs the daemon's project registry, which
//! `session_manager` sits below; the daemon supplies an implementation that
//! runs the SAME adapter path and the SAME post-send verification the
//! interactive resume uses. With no relauncher installed — every hermetic test,
//! and any embedder that drives resumes itself — behaviour is exactly as before.
//!
//! The manager never marks a record `Active` and walks away: `resume_auto`
//! demotes it to `Errored` when the relaunch does not produce a verified
//! runtime, so `Active` means a runtime was seen.
//!
//! Test: `crates/trusty-mpm/src/session_manager/relaunch_tests.rs`.

use std::sync::Arc;

use super::record::SessionRecord;

/// Put a runtime back into an already-prepared pane.
///
/// Why: see this module's header.
/// What: implemented by `daemon::managed_routes::auto_relaunch::DaemonRelauncher`
/// over `runtime::build_adapter` + `launch_verify`. `Err(message)` means no
/// verified runtime came up, and the message is what the record is errored with.
/// Test: `relaunch_tests.rs` drives both arms with a scripted relauncher.
#[async_trait::async_trait]
pub trait RuntimeRelauncher: Send + Sync {
    /// Relaunch `record`'s runtime in its pane, returning why not on failure.
    async fn relaunch(&self, record: &SessionRecord) -> Result<(), String>;
}

impl super::SessionManager {
    /// Install the relauncher the automatic resume paths use.
    ///
    /// Why: the manager is built before the daemon has a `DaemonState` to hand
    /// it, so this is a post-construction install rather than a constructor
    /// argument. Once only — a second install is ignored, which keeps the
    /// installed implementation immutable for the process lifetime and means a
    /// double-initialised `OnceCell` cannot swap the launch path underneath a
    /// sweep already running.
    /// What: sets the `OnceLock`; returns whether this call was the one that set
    /// it.
    /// Test: `installing_a_relauncher_twice_keeps_the_first`.
    pub fn install_relauncher(&self, relauncher: Arc<dyn RuntimeRelauncher>) -> bool {
        self.relauncher.set(relauncher).is_ok()
    }

    /// The installed relauncher, if any.
    ///
    /// Test: `an_auto_resume_without_a_relauncher_behaves_as_before`.
    pub(crate) fn relauncher(&self) -> Option<Arc<dyn RuntimeRelauncher>> {
        self.relauncher.get().cloned()
    }
}

#[cfg(test)]
#[path = "relaunch_tests.rs"]
mod tests;
