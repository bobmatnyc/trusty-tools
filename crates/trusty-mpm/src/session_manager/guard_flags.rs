//! Write path for `pm_guard`'s daemon-held kill-switch flags (issue #3981
//! Part 2).
//!
//! Why: mirrors `hook_sync.rs`'s `set_claude_session_id` precedent exactly —
//! a single, isolated write-path method keeps `manager.rs`/`lifecycle.rs`
//! under their SLOC budgets and gives the spawn/resume call sites one place
//! to persist the operator-captured flags.
//! What: one inherent method `SessionManager::set_guard_flags` — looks up
//! the record, writes both flags, and persists atomically.
//! Test: `guard_flags_persist_on_session` in `super::tests`.

use super::manager::{ManagedError, SessionManager};
use super::record::ManagedSessionId;

impl SessionManager {
    /// Store the `pm_guard` kill-switch flags on a managed session record.
    ///
    /// Why (#3981): `tm sessions new`/`start`/`resume` capture
    /// `TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED` from the CLI
    /// process's OWN environment — the operator's launching shell — ONCE, at
    /// spawn/resume time, before this session's PM process exists. This is
    /// the only write path: `pm_guard` itself never calls it, so there is no
    /// mid-session flip (Bob's directive — disabling the guard requires an
    /// actual `tm sessions resume` with the flag set, not a live edit).
    /// What: looks up the record, sets `disable_hooks`/`pm_unrestricted`, and
    /// persists. No tmux I/O.
    /// Test: `guard_flags_persist_on_session` in `super::tests`.
    pub async fn set_guard_flags(
        &self,
        id: &ManagedSessionId,
        disable_hooks: bool,
        pm_unrestricted: bool,
    ) -> Result<(), ManagedError> {
        let mut r = self.get(id).await?;
        r.disable_hooks = disable_hooks;
        r.pm_unrestricted = pm_unrestricted;
        self.store.write().await.upsert(r).await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::session_manager::tests::make_manager;

    #[tokio::test]
    async fn guard_flags_persist_on_session() {
        // Why (#3981): set_guard_flags must survive a store reload so that
        // pm_guard's daemon round-trip reads the operator-captured flags
        // back correctly even after a daemon restart.
        // What: create a session (both flags default false), write both
        // flags true via `set_guard_flags`, reload from disk, assert both
        // survive.
        let dir = crate::test_support::hermetic_temp_dir();
        let (mgr, _fake) = make_manager(&dir).await;

        let record = mgr
            .create(
                "task".into(),
                Some(PathBuf::from("/tmp/wt")),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        assert!(!record.disable_hooks);
        assert!(!record.pm_unrestricted);

        mgr.set_guard_flags(&record.id, true, true)
            .await
            .expect("set_guard_flags");

        let reloaded = mgr.get(&record.id).await.expect("get after set");
        assert!(
            reloaded.disable_hooks,
            "disable_hooks must survive a store reload (#3981)"
        );
        assert!(
            reloaded.pm_unrestricted,
            "pm_unrestricted must survive a store reload (#3981)"
        );
    }
}
