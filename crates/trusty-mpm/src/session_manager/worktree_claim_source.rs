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

use super::manager::SessionManager;
use super::worktree_reclaim_claim::{ClaimLiveness, LiveClaims, WorkspaceClaim};

impl SessionManager {
    /// Every stored workspace claim, with each one's session probed for life.
    ///
    /// Why: see the module doc — this is the single producer, so the liveness
    /// probe cannot be forgotten by a call site.
    /// What: one `tmux list-sessions` for the whole set, then one claim per
    /// record that has a `workspace_path`. `caller` is the invoking managed
    /// session id, passed straight through to [`LiveClaims::caller`].
    /// Test: as the module doc.
    pub(crate) async fn workspace_claims(&self, caller: Option<String>) -> LiveClaims {
        // #5856: `Err` means tmux could not be observed, which is not an empty
        // tmux. `.ok()` collapses it to `None`, and `liveness_of` reads `None`
        // as "every claim stays live".
        let live_names = self.observed_live_managed_names().ok();
        let claims = self
            .list()
            .await
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
        LiveClaims { claims, caller }
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
