//! Live delegation records of unbound sessions, still waiting for the stale (#9007).
//!
//! Why: #8980 made SessionEnd staling fail closed. A session with no binding in
//! `session_claudes` (first SessionStart over HTTP, pre-upgrade, resumed by
//! hand) no longer gets the #6797 fast stale, so its records wait for the
//! age-based sweep (6 h) or `tm repair delegation`. The owner accepted that on
//! condition `tm doctor` shows it.
//! What: [`count_unbound_waiting`], read by `GET /health` for the `tm doctor`
//! row. Read-only, O(N) over the delegation map.
//! Test: `delegation_unbound_tests.rs`.

use crate::daemon::state::DaemonState;
use crate::daemon::state::sessions::live_stale_budget_secs;

/// Count live records whose session has no `claude` binding and that have not
/// yet reached their staling budget at `now`.
///
/// Why: see the module doc.
/// What: `Err` with the seal reason when the registry is sealed — every id then
/// reads as unbound, so a count would be noise, and the row must not pass. A
/// record past its budget is not counted: the next sweep stales it. The budget
/// is [`live_stale_budget_secs`], the one the sweep uses.
/// Test: `no_records_counts_zero_9007`,
/// `only_unbound_unstaled_live_records_count_9007`,
/// `a_sealed_registry_is_an_error_not_a_zero_9007`.
pub fn count_unbound_waiting(
    state: &DaemonState,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize, String> {
    // #9007: fail closed; `get` answers `None` for every id while sealed.
    if let Some(why) = state.session_claudes().sealed() {
        return Err(format!("the session-claude registry is sealed: {why}"));
    }
    Ok(state
        .all_delegations()
        .iter()
        .filter(|d| {
            d.status.is_live()
                && state.session_claudes().get(d.session).is_none()
                && now - d.started_at.unwrap_or(d.created_at)
                    <= chrono::Duration::seconds(live_stale_budget_secs(d))
        })
        .count())
}

#[cfg(test)]
#[path = "delegation_unbound_tests.rs"]
mod delegation_unbound_tests;
