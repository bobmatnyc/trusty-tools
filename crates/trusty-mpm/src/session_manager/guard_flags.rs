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
