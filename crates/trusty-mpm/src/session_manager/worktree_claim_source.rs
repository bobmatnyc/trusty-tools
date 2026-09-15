//! The one place a [`LiveClaims`] set is built from the session store (#7232).
//!
//! Why: gate 2's claim set was assembled independently at each call site — the
//! `prune-worktrees` route, the MCP disk survey — and each one read
//! [`SessionManager::list`] and turned every stored `workspace_path` into a
//! claim. That read has no liveness in it: the store tombstones records rather
//! than dropping them, so the claim set was a permanent ledger of every
//! directory any session ever occupied. One `deleted` record for the adopted
//! pane `tm-bobmatnyc`, holding the org-level path
//! `~/trusty-mpm-projects/bobmatnyc`, therefore blocked every worktree in every
//! project beneath it — seven repositories, "would remove 0", the same dead
//! session named on every candidate. Building the set in ONE place is what lets
//! the liveness probe be added once instead of at each site, and what stops the
//! next site from omitting it.
//!
//! What: [`SessionManager::workspace_claims`] pairs each record's
//! `workspace_path` with a [`ClaimLiveness`] derived from
//! [`SessionManager::observed_live_managed_names`] — the crate's existing tmux
//! probe, which propagates "tmux could not be observed" as `Err` rather than as
//! an empty set (#5856).
//!
//! # Fail direction
//!
//! Toward LIVE, which is toward refusing the removal. A probe that errored, a
//! record whose tmux name is not managed-shaped, and a name the probe reported
//! as present all yield [`ClaimLiveness::Live`]. Only a probe that ANSWERED and
//! did not list a managed name marks that record's claim
//! [`ClaimLiveness::SessionGone`]. The record's own `state` field is never
//! consulted: #2919 measured a live session occupying a directory whose record
//! read terminal, and that finding stands.
//!
//! Test: `dead_sessions_claims_are_discarded_and_live_ones_are_not`,
//! `an_unobservable_tmux_leaves_every_claim_live` in
//! `worktree_claim_source_tests.rs`.

use std::time::Duration;

use super::manager::SessionManager;
use super::worktree_reclaim_claim::{ClaimLiveness, LiveClaims, WorkspaceClaim};
use super::worktree_reclaim_ownership::SessionOwners;

/// Wall-clock ceiling for the tmux liveness probe this module runs (#7965).
///
/// Why: `tmux list-sessions` against a healthy server answers in single-digit
/// milliseconds, so 3 seconds is a hundredfold headroom and still finite. The
/// probe is a synchronous subprocess pair (`ensure_server_up`, then the list),
/// and before #7965 an unresponsive tmux server held the calling thread for as
/// long as it liked — on the `prune-worktrees` route that thread is a tokio
/// WORKER, i.e. one of the handful the daemon has to answer requests with.
/// What: on expiry the probe is treated exactly as an errored probe — see the
/// module's Fail direction section: every claim stays LIVE and every removal is
/// refused.
/// Test: `a_slow_tmux_probe_is_bounded_and_leaves_every_claim_live`.
pub(crate) const TMUX_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

impl SessionManager {
    /// Every stored workspace claim, with each one's session probed for life.
    ///
    /// Why: see the module doc — this is the single producer, so the liveness
    /// probe cannot be forgotten by a call site.
    /// What: one `tmux list-sessions` for the whole set, then one claim per
    /// record that has a `workspace_path`, and one [`LiveClaims::owners`] entry
    /// per record whether or not it has one (#7652). `caller` is the invoking
    /// managed session id, passed straight through to [`LiveClaims::caller`].
    /// Test: as the module doc, plus
    /// `worktree_7652_owners_include_a_record_with_no_workspace_path` and
    /// `worktree_7652_an_errored_tmux_probe_is_refused`.
    pub(crate) async fn workspace_claims(&self, caller: Option<String>) -> LiveClaims {
        // #5856: `Err` means tmux could not be observed, which is not an empty
        // tmux. `None` here — from an error OR from #7965's timeout — is read by
        // `liveness_of` as "every claim stays live".
        let live_names = self.bounded_live_managed_names().await;
        let records = self.list().await;
        // #7652: built from EVERY record. Gate 4 judges a sentinel's owner by its
        // record, and the claim list below drops a record with no workspace.
        let owners = SessionOwners::observed(records.iter().map(|r| {
            (
                r.id.to_string(),
                liveness_of(live_names.as_ref(), &r.tmux_name),
            )
        }));
        let claims = records
            .into_iter()
            .filter_map(|r| {
                let path = r.workspace_path?;
                Some(WorkspaceClaim::with_liveness(
                    r.id.to_string(),
                    path,
                    liveness_of(live_names.as_ref(), &r.tmux_name),
                ))
            })
            .collect();
        LiveClaims {
            claims,
            caller,
            owners,
        }
    }

    /// The tmux liveness probe, off the caller's thread and bounded (#7965).
    ///
    /// Why: [`super::SessionManager::observed_live_managed_names`] is two
    /// synchronous subprocesses. Running it inline blocked whatever thread asked
    /// — for the `prune-worktrees` route that is a tokio worker, and for the
    /// automatic sweep it is a blocking-pool thread that then never finished.
    /// Neither may depend on tmux answering.
    /// What: hands the probe to the blocking pool and waits at most
    /// [`TMUX_PROBE_TIMEOUT`]. `None` on ANY of error, panic or expiry — which
    /// `liveness_of` reads as "every claim stays live", the refusing direction.
    ///
    /// A `spawn_blocking` task cannot be cancelled, so an expired probe keeps its
    /// blocking thread until tmux returns. That is deliberate: the thread it no
    /// longer holds is the CALLER's, which is the one a request needs.
    /// Test: `a_slow_tmux_probe_is_bounded_and_leaves_every_claim_live`.
    async fn bounded_live_managed_names(&self) -> Option<std::collections::HashSet<String>> {
        let tmux = std::sync::Arc::clone(&self.tmux);
        let probe =
            tokio::task::spawn_blocking(move || super::dedup::live_managed_names_of(tmux.as_ref()));
        match tokio::time::timeout(TMUX_PROBE_TIMEOUT, probe).await {
            Ok(Ok(Ok(names))) => Some(names),
            Ok(Ok(Err(e))) => {
                tracing::warn!(
                    "workspace claims: tmux could not be observed ({e}); claims stay live"
                );
                None
            }
            Ok(Err(e)) => {
                tracing::warn!("workspace claims: the tmux probe panicked ({e}); claims stay live");
                None
            }
            Err(_) => {
                tracing::warn!(
                    "workspace claims: tmux did not answer within {}s; claims stay live (#7965)",
                    TMUX_PROBE_TIMEOUT.as_secs()
                );
                None
            }
        }
    }
}

/// Classify one record's tmux name against the probe's answer (#7232).
///
/// Why: three of the four inputs must produce `Live`, and writing that as a
/// single expression at the call site invites a future edit to invert one of
/// them. The one path to `SessionGone` is explicit here instead.
/// What: `SessionGone` only when the probe answered (`Some`), the record's name
/// is one the probe would have listed had it existed
/// ([`trusty_common::session_naming::is_managed_session_name`] — the same
/// filter `observed_live_managed_names` applies), and that name is absent.
/// Test: `dead_sessions_claims_are_discarded_and_live_ones_are_not`,
/// `an_unmanaged_tmux_name_is_never_read_as_dead`.
fn liveness_of(
    live_names: Option<&std::collections::HashSet<String>>,
    tmux_name: &str,
) -> ClaimLiveness {
    let Some(names) = live_names else {
        return ClaimLiveness::Live;
    };
    // A name the probe filters out could never appear in `names`, so its
    // absence proves nothing about the session.
    if !crate::core::names::is_managed_session_name(tmux_name) {
        return ClaimLiveness::Live;
    }
    if names.contains(tmux_name) {
        ClaimLiveness::Live
    } else {
        ClaimLiveness::SessionGone
    }
}

#[cfg(test)]
#[path = "worktree_claim_source_tests.rs"]
mod worktree_claim_source_tests;
