//! Supersession demotion for recall: a drawer another drawer superseded ranks
//! below its replacement (#9421, ADR-0028 D6).
//!
//! Why: [`super::recall_rank`] demotes stale drawers only by tag (snapshot) or
//! by Tier C slot. A drawer that a newer fact superseded, with neither, ranked
//! on similarity alone and could sit above the drawer that replaced it. The
//! supersession itself is recorded: the dream cycle, the share path and a
//! hand-written `kg_assert` all write `drawer:<old> superseded_by drawer:<new>`
//! (trusty-common `share::supersede`). Recall now reads that edge.
//! What: [`supersessions_for`] and [`supersessions_across`] fetch the edges for
//! one result window in one KG read per palace, bounded by
//! [`LOOKUP_BUDGET`]. A failed or late read logs a warning and demotes nothing,
//! so recall still answers ([`fail_open`]). [`demote_superseded`] halves a
//! superseded drawer's score and, when its replacement is in the same list,
//! keeps it strictly below the replacement. Nothing is deleted (D6).
//! Test: `tools::recall_supersede_tests`; end to end in
//! `tests/recall_supersession.rs` and the `superseded_by` groups of
//! `tests/recall_eval.rs`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use trusty_common::memory_core::retrieval::{CrossPalaceResult, PalaceHandle, RecallResult};
use uuid::Uuid;

/// `superseded drawer id -> replacement drawer id` for one recall.
pub(crate) type Supersessions = HashMap<Uuid, Uuid>;

/// Score multiplier for a superseded drawer.
///
/// Why: demotion, not exclusion (ADR-0028 D6): with no replacement in the
/// window, the old drawer stays recallable as history. Same value as the
/// snapshot floor, so the two demotions weigh alike.
/// Test: `a_superseded_drawer_is_halved_when_its_replacement_is_absent`.
pub(crate) const SUPERSEDED_WEIGHT: f32 = 0.5;

/// Longest a recall waits for the `superseded_by` lookup.
///
/// Why: the lookup is one redb read transaction, measured in microseconds
/// (see `kg_supersession_latency_profile`). The budget only bounds a stalled
/// read: past it, the recall answers undemoted rather than waiting.
/// Test: `a_late_lookup_demotes_nothing`.
pub(crate) const LOOKUP_BUDGET: Duration = Duration::from_millis(200);

/// Why the `superseded_by` lookup produced no map.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SupersessionLookupError {
    /// The KG read failed.
    #[error("superseded_by lookup failed: {0:#}")]
    Read(anyhow::Error),
    /// The KG read did not finish within its budget.
    #[error("superseded_by lookup exceeded its {0:?} budget")]
    TimedOut(Duration),
}

/// Await `lookup` for at most `budget`.
///
/// Test: `a_failed_lookup_demotes_nothing`, `a_late_lookup_demotes_nothing`.
pub(crate) async fn lookup_within<F>(
    lookup: F,
    budget: Duration,
) -> Result<Supersessions, SupersessionLookupError>
where
    F: Future<Output = anyhow::Result<Supersessions>>,
{
    match tokio::time::timeout(budget, lookup).await {
        Ok(found) => found.map_err(SupersessionLookupError::Read),
        Err(_) => Err(SupersessionLookupError::TimedOut(budget)),
    }
}

/// Run `lookup` within `budget`; on any failure, log and demote nothing.
///
/// Why (#9421): a KG read error or a stalled read must never drop the recall
/// or turn it into an error. Recall without demotion is the pre-#9421 answer,
/// which is a safe answer.
/// What: [`lookup_within`]; an `Err` is logged at WARN with the palace and
/// mapped to an empty map.
/// Test: `a_failed_lookup_demotes_nothing`, `a_late_lookup_demotes_nothing`.
pub(crate) async fn fail_open<F>(palace: &str, lookup: F, budget: Duration) -> Supersessions
where
    F: Future<Output = anyhow::Result<Supersessions>>,
{
    match lookup_within(lookup, budget).await {
        Ok(found) => found,
        Err(e) => {
            // #9421: fail open — the recall answers, undemoted.
            tracing::warn!(
                palace,
                "#9421: {e}; recall answers without supersession demotion"
            );
            Supersessions::new()
        }
    }
}

/// The supersessions among one palace's recalled drawers.
///
/// Why/What: one batch KG read for the whole window, through [`fail_open`].
/// Test: `a_superseded_drawer_ranks_below_its_replacement_on_every_recall_surface`.
pub(crate) async fn supersessions_for(
    handle: &PalaceHandle,
    results: &[RecallResult],
) -> Supersessions {
    if results.is_empty() {
        return Supersessions::new();
    }
    let ids = results.iter().map(|r| r.drawer.id).collect();
    fail_open(
        handle.id.as_str(),
        handle.kg.superseded_by_many(ids),
        LOOKUP_BUDGET,
    )
    .await
}

/// The supersessions among a cross-palace batch's drawers.
///
/// Why: an edge lives in the KG of the palace that holds both drawers, so
/// each palace answers for its own hits.
/// What: groups `results` by palace id and runs [`supersessions_for`]'s read
/// against each matching handle. Drawer ids are UUIDs, so the per-palace
/// maps merge without collision.
/// Test: `a_superseded_drawer_ranks_below_its_replacement_on_every_recall_surface`.
pub(crate) async fn supersessions_across(
    handles: &[Arc<PalaceHandle>],
    results: &[CrossPalaceResult],
) -> Supersessions {
    let mut found = Supersessions::new();
    for handle in handles {
        let ids: Vec<Uuid> = results
            .iter()
            .filter(|r| r.palace_id == handle.id.as_str())
            .map(|r| r.result.drawer.id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        let lookup = handle.kg.superseded_by_many(ids);
        found.extend(fail_open(handle.id.as_str(), lookup, LOOKUP_BUDGET).await);
    }
    found
}

/// A ranked hit whose inner [`RecallResult`] demotion rewrites.
pub(crate) trait RankedHit {
    fn hit(&self) -> &RecallResult;
    fn hit_mut(&mut self) -> &mut RecallResult;
}

impl RankedHit for RecallResult {
    fn hit(&self) -> &RecallResult {
        self
    }
    fn hit_mut(&mut self) -> &mut RecallResult {
        self
    }
}

impl RankedHit for CrossPalaceResult {
    fn hit(&self) -> &RecallResult {
        &self.result
    }
    fn hit_mut(&mut self) -> &mut RecallResult {
        &mut self.result
    }
}

/// Demote every superseded drawer in `results`; the caller re-sorts.
///
/// Why (#9421 AC1): a weight alone cannot promise "below its replacement" —
/// an old drawer can out-score its replacement by more than any fixed factor.
/// What: multiplies each superseded drawer's score by [`SUPERSEDED_WEIGHT`].
/// Then, where the replacement is in `results` with a finite score, lowers the
/// superseded score to just below the replacement's. Chains (A by B, B by C)
/// settle within `results.len()` passes; a cycle stops there. A drawer with no
/// edge keeps its score exactly. Run after every other score weight, so no
/// later multiplier can undo the order.
/// Test: `a_superseded_drawer_ranks_below_a_replacement_it_outscored`,
/// `a_supersession_chain_ranks_in_order`,
/// `drawers_with_no_edge_keep_their_scores_and_order`.
pub(crate) fn demote_superseded<T: RankedHit>(results: &mut [T], sup: &Supersessions) {
    if sup.is_empty() {
        return;
    }
    for r in results.iter_mut() {
        if sup.contains_key(&r.hit().drawer.id) {
            r.hit_mut().score *= SUPERSEDED_WEIGHT;
        }
    }
    for _ in 0..results.len() {
        let scores: HashMap<Uuid, f32> = results
            .iter()
            .map(|r| (r.hit().drawer.id, r.hit().score))
            .collect();
        let mut changed = false;
        for r in results.iter_mut() {
            let Some(replacement) = sup.get(&r.hit().drawer.id) else {
                continue;
            };
            let Some(&ceiling) = scores.get(replacement).filter(|s| s.is_finite()) else {
                continue;
            };
            // Anything but "already below" is clamped, a NaN score included
            // (NaN sorts first under `by_score_desc`).
            if r.hit().score.partial_cmp(&ceiling) != Some(std::cmp::Ordering::Less) {
                r.hit_mut().score = ceiling.next_down();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

#[cfg(test)]
#[path = "recall_supersede_tests.rs"]
mod recall_supersede_tests;
