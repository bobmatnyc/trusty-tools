//! Making the session store's read-failure fallback visible (#5007).
//!
//! Why: [`SessionManager::list`](super::manager::SessionManager::list) and
//! `get` deliberately serve their last-known in-memory set when the store
//! cannot be reloaded. That fallback is worth keeping — #1219's follow-up added
//! it so a transient read error could not make the daemon report an empty
//! fleet, which would mislead the supervisor into re-provisioning sessions that
//! are still running. What was wrong is that it was SILENT: on 2026-08-06 a
//! corrupt `sessions.json` blocked every write for an unknown period while
//! `tm ls` kept printing a healthy fleet from exactly this fallback. So the
//! resilience stays and the silence goes.
//! What: [`log_reload_fallback`], which logs corruption at ERROR (it never
//! clears on its own, and while it holds every mutation fails) and a
//! possibly-transient read failure at WARN; and
//! [`SessionManager::store_health`], the single accessor the list endpoint,
//! `tm ls`, and `tm doctor` all read the degradation from.
//! Test: `log_reload_fallback_separates_corruption_from_a_transient_error`,
//! `store_records_and_clears_a_reload_failure` (the degradation this reads).

use tracing::{error, warn};

use super::manager::SessionManager;
use super::store::{StoreDegradation, StoreError};

impl SessionManager {
    /// The store read failure this manager is currently serving cached data
    /// over, if any.
    ///
    /// Why: see the module doc — this is what turns the invisible fallback into
    /// a reportable one without removing it.
    /// What: clones the store's current [`StoreDegradation`], or `None` when
    /// the last reload succeeded.
    /// Test: `store_records_and_clears_a_reload_failure` pins the degradation
    /// this returns; `list_response_omits_store_health_when_healthy` pins how the
    /// list endpoint renders it.
    pub async fn store_health(&self) -> Option<StoreDegradation> {
        self.store.read().await.degradation().cloned()
    }
}

/// Log that a listing was served from stale memory, at the severity the cause
/// deserves.
///
/// Why: a corrupt store and a momentarily unreadable one produce the same
/// fallback but call for different operator responses. Corruption is permanent
/// and blocks every write, so it is an ERROR that says so; an I/O failure may
/// already have cleared by the next read, so it stays a WARN.
/// What: one log line either way, carrying the count actually served.
/// Test: `log_reload_fallback_separates_corruption_from_a_transient_error`.
pub(super) fn log_reload_fallback(e: &StoreError, served: usize) {
    if e.is_corruption() {
        error!(
            count = served,
            "session list: store is CORRUPT: {e}; serving the last-known in-memory set — \
             every write to the store is failing until it is repaired"
        );
    } else {
        warn!(
            count = served,
            "session list: reload failed: {e}; returning last-known in-memory set"
        );
    }
}
