//! The `health` verb: a daemon health probe (`GET /health` + fleet summary).
//!
//! Why: operators (and the action-capable coordinator chat, #1496) routinely ask
//! "run a health check" / "is the daemon up?". The chat-core nucleus had no
//! health verb, so the coordinator deflected. This module is the single
//! dispatchable health probe every adapter shares — it reuses the existing
//! `GET /health` route (liveness + catalog-freshness flags) and the managed-list
//! call (fleet summary) rather than inventing a new daemon endpoint. Splitting it
//! into its own executor submodule keeps every file under the 500-SLOC cap.
//! What: the [`CommandExecutor::health`] method — it probes `GET /health` and the
//! managed list, composing a [`CommandResult::Health`]. A dead daemon yields a
//! `Health { reachable: false, status: "unreachable", .. }` result (NEVER a
//! panic), so an unreachable daemon stays renderable.
//! Test: `execute_health_against_test_daemon` and `execute_health_dead_daemon_renders`
//! in `tests.rs`.

use super::CommandExecutor;
use crate::client::result::{CommandResult, HealthReport};

/// The liveness word reported when the daemon's `/health` probe fails.
const STATUS_UNREACHABLE: &str = "unreachable";

impl CommandExecutor {
    /// `health` — report daemon reachability, catalog freshness, and a fleet
    /// summary.
    ///
    /// Why: the one health probe shared by every adapter and the action loop. It
    /// must be genuinely useful (liveness + catalog drift + how many sessions, how
    /// many blocked) yet degrade gracefully — a daemon that is entirely down is a
    /// renderable `reachable: false` result, not an error or a panic.
    /// What: GETs `GET /health` for the liveness word and the `catalog_stale` /
    /// `catalog_unknown` flags; on success, additionally fetches the managed list
    /// to count total sessions and those blocked on a pending decision. If the
    /// `/health` probe fails (daemon down), returns a `Health` with
    /// `reachable: false` and `status: "unreachable"`, leaving the remaining
    /// fields at their defaults; a managed-list failure after a healthy probe
    /// leaves the fleet counts at zero (the daemon is up; only the secondary call
    /// failed) rather than masking the up status.
    /// Test: `execute_health_against_test_daemon`, `execute_health_dead_daemon_renders`.
    pub(super) async fn health(&self) -> CommandResult {
        let url = self.client().base_url().to_string();
        let snapshot = match self.client().health_snapshot().await {
            Ok(snapshot) => snapshot,
            // The daemon is unreachable: convey that as a renderable Health result
            // (reachable=false) rather than a transport error or a panic.
            Err(_) => {
                return CommandResult::Health(HealthReport {
                    reachable: false,
                    url,
                    status: STATUS_UNREACHABLE.to_string(),
                    ..HealthReport::default()
                });
            }
        };

        // The daemon answered: summarise the managed fleet too. A failure of this
        // secondary call must not flip the report to "down" — the daemon IS up —
        // so the counts default to zero and the up status is preserved.
        let (managed_total, managed_pending_decisions) =
            match self.client().list_managed_sessions().await {
                Ok(sessions) => {
                    let total = sessions.len();
                    let pending = sessions
                        .iter()
                        .filter(|s| s.pending_decision.is_some())
                        .count();
                    (total, pending)
                }
                Err(_) => (0, 0),
            };

        CommandResult::Health(HealthReport {
            reachable: true,
            url,
            status: snapshot.status,
            catalog_stale: snapshot.catalog_stale,
            catalog_unknown: snapshot.catalog_unknown,
            managed_total,
            managed_pending_decisions,
        })
    }
}
