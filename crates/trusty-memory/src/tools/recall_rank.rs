//! Typed temporal ranking for recall: stale session snapshots rank below
//! current rulings (#8246 "typed temporal ranking", #9142 criterion 3).
//!
//! Why: the 2026-10-03 memory health review found outdated session snapshots
//! ("state as of", "Builder band as of", pause states) in 8% of top-5 recall
//! hits, two of them above the current answer. Relevance alone cannot tell a
//! 19-day-old status line from today's ruling on the same subject: both match
//! the query, and the 90-day importance decay barely moves either. ADR-0028 D1
//! names the property that does separate them — a point-in-time fact stops
//! being true, a ruling does not — and D4 gives point-in-time facts a half-life
//! of hours. Existing unkeyed drawers never became Tier C (ADR-0028 migration
//! step 2), so this pass applies that half-life at read time instead. Nothing
//! is deleted or rewritten (D6): a snapshot only loses score.
//! What: [`temporal_weight`] classifies one drawer and returns its score
//! multiplier; [`demote_stale_snapshots`] and [`demote_stale_snapshots_across`]
//! apply it to a single-palace and a cross-palace result list and re-sort.
//! [`ranking_window`] sizes the candidate set the lanes fetch, so demotion
//! re-ranks more than the final `top_k`. [`is_ruling`] is shared with the
//! cross-palace rulings leg (`super::recall_rulings`), so both agree on what a
//! ruling is.
//! Test: `tools::recall_rank_tests`; end to end in
//! `tests/recall_temporal_rank.rs`.

use chrono::{DateTime, Utc};
use trusty_common::memory_core::palace::{Drawer, DrawerType};
use trusty_common::memory_core::retrieval::{CrossPalaceResult, RecallResult};

use super::recall_projection::{candidate_window, MAX_CANDIDATE_WINDOW};

/// Tags that mark a drawer as an owner ruling or decision.
///
/// Why: drawers carry no ruling type yet (#9138 O5), so tags are the only
/// signal. These are the spellings the review counted in the Architect's
/// rulings palace (`bob-ruling` 36, `standing-rule` 11) and ADR-0028 §C4
/// (`bob-decision`), plus their generic forms. `standing-instruction` is left
/// out on purpose: §C4 found it on one-shot actions as well as rules.
/// What: compared exactly against each stored tag, so `decisionless` or
/// `Decision` is not a ruling.
/// Test: `ruling_tags_exempt_a_drawer_that_is_also_tagged_status`,
/// `tags_match_exactly_never_by_substring`.
pub(crate) const RULING_TAGS: &[&str] = &[
    "bob-ruling",
    "owner-ruling",
    "ruling",
    "bob-decision",
    "decision",
    "standing-rule",
];

/// Tags that mark a drawer as a session snapshot — a point-in-time fact.
///
/// Why: `status` and `resume-target` are the two snapshot tags the review
/// measured in one project palace (1,482 and 1,318 drawers, almost none keyed).
/// What: compared exactly against each stored tag.
/// Test: `a_stale_snapshot_loses_score_and_a_fresh_one_does_not`,
/// `tags_match_exactly_never_by_substring`.
pub(crate) const SNAPSHOT_TAGS: &[&str] =
    &["status", "resume-target", "snapshot", "session-snapshot"];

/// Score multiplier a snapshot approaches as it ages.
///
/// Why: demotion, not exclusion (ADR-0028 D6). A floor keeps an old snapshot
/// recallable as history when nothing current competes with it.
/// What: `0.5` — an old snapshot keeps half its relevance score.
/// Test: `snapshot_weight_halves_toward_the_floor`.
pub(crate) const SNAPSHOT_FLOOR: f32 = 0.5;

/// Half-life, in hours, of a snapshot's excess weight above the floor.
///
/// Why: ADR-0028 D4 gives point-in-time facts "a half-life of hours, not
/// 90 days". 24 hours leaves today's snapshot near full weight, so a fresh
/// state line still answers "where are we now".
/// What: the weight is `FLOOR + (1 - FLOOR) * 0.5^(age_hours / 24)`.
/// Test: `snapshot_weight_halves_toward_the_floor`.
pub(crate) const SNAPSHOT_HALF_LIFE_HOURS: f32 = 24.0;

/// True when `drawer` carries one of [`RULING_TAGS`].
pub(crate) fn is_ruling(drawer: &Drawer) -> bool {
    drawer
        .tags
        .iter()
        .any(|t| RULING_TAGS.contains(&t.as_str()))
}

/// True when `drawer` is a session snapshot by tag or by type.
fn is_snapshot(drawer: &Drawer) -> bool {
    drawer.drawer_type == DrawerType::SessionEvent
        || drawer
            .tags
            .iter()
            .any(|t| SNAPSHOT_TAGS.contains(&t.as_str()))
}

/// Score multiplier for one recalled drawer at time `now`.
///
/// Why: see the module doc. The two exemptions keep the pass from demoting the
/// drawers the epic wants on top: a ruling (by tag, which wins over a snapshot
/// tag on the same drawer, because §C4 shows authors tag rulings `status` too),
/// and a live Tier C fact (a `fact_key` that has not expired), which is current
/// by construction — its slot retires the previous occupant on write.
/// What: `1.0` for a ruling, a live keyed drawer, or a non-snapshot. For a
/// snapshot, `SNAPSHOT_FLOOR + (1 - SNAPSHOT_FLOOR) * 0.5^(age / half-life)`,
/// with a future `created_at` (clock skew) treated as age zero. Always in
/// `[SNAPSHOT_FLOOR, 1.0]`.
/// Test: `a_stale_snapshot_loses_score_and_a_fresh_one_does_not`,
/// `a_live_keyed_snapshot_is_exempt`,
/// `ruling_tags_exempt_a_drawer_that_is_also_tagged_status`,
/// `a_future_created_at_is_age_zero_not_a_boost`.
pub(crate) fn temporal_weight(drawer: &Drawer, now: DateTime<Utc>) -> f32 {
    if is_ruling(drawer) || !is_snapshot(drawer) {
        return 1.0;
    }
    if drawer.fact_key.is_some() && !drawer.is_expired_at(now) {
        return 1.0;
    }
    // #8246: clock skew — a future `created_at` is age zero, never a boost.
    let age_hours = (now - drawer.created_at).num_seconds().max(0) as f32 / 3600.0;
    let weight = SNAPSHOT_FLOOR
        + (1.0 - SNAPSHOT_FLOOR) * 0.5_f32.powf(age_hours / SNAPSHOT_HALF_LIFE_HOURS);
    debug_assert!((SNAPSHOT_FLOOR..=1.0).contains(&weight));
    weight
}

/// Recall rank order: score descending, then layer, then drawer id.
///
/// Why: `partial_cmp(..).unwrap_or(Equal)` makes NaN equal to every score,
/// which is not a total order — the sort may then leave finite scores out of
/// order, or panic on newer std sorts that detect the violation. #9280: equal
/// scores kept the lane's order, which a rebuilt vector index can change, so
/// two opens of one palace could rank tied drawers differently.
/// What: `b.score.total_cmp(&a.score)`, so a positive NaN sorts first and
/// finite scores stay descending; ties go to the lower layer (L0/L1 are pinned
/// identity/essentials), then the lower drawer id. A total order over distinct
/// drawers, so the result depends on the set of hits, not their input order.
/// Shared with the BM25 fusion sort.
/// Test: `a_nan_score_never_unorders_the_finite_scores`,
/// `tied_scores_rank_by_drawer_id_whatever_the_input_order`.
pub(crate) fn by_score_desc(a: &RecallResult, b: &RecallResult) -> std::cmp::Ordering {
    // #8246: total order, so a NaN cannot make the comparator inconsistent.
    b.score
        .total_cmp(&a.score)
        .then(a.layer.cmp(&b.layer))
        // #9280: id tiebreak, so tied scores rank the same on every open.
        .then_with(|| a.drawer.id.cmp(&b.drawer.id))
}

/// Candidates a recall lane fetches before demotion re-ranks them.
///
/// Why (#8246): demotion only re-sorts what the lanes returned. Fetching
/// exactly `top_k` means a fresh drawer or ruling at rank `top_k + 1` can never
/// be lifted above a demoted snapshot, because it was never fetched.
/// What: the floor's [`candidate_window`], widened to `2 * top_k` when that is
/// larger, capped at [`MAX_CANDIDATE_WINDOW`] (never below `top_k`). The
/// handler cuts back to `top_k` after demotion. Beyond the window the limit
/// stands: a drawer ranked below `2 * top_k` by relevance is not lifted.
/// Test: `ranking_window_doubles_top_k_and_caps`;
/// `a_fresh_drawer_just_past_top_k_is_lifted_above_stale_snapshots`.
pub(crate) fn ranking_window(top_k: usize, min_score: Option<f32>) -> usize {
    let doubled = top_k.saturating_mul(2).min(MAX_CANDIDATE_WINDOW.max(top_k));
    candidate_window(top_k, min_score).max(doubled)
}

/// Demote stale snapshots in a single-palace recall and re-sort.
///
/// Why: the handlers sort by score once the lanes are fused; a weight applied
/// after that sort must re-sort or it changes numbers without changing order.
/// What: multiplies every result's score by [`temporal_weight`], then
/// sorts by [`by_score_desc`], so equal scores rank by layer, then id. The list
/// length is unchanged; the caller's floor and `top_k` cut run after this.
/// Demotion can only lift what the lane fetched: callers fetch a
/// [`ranking_window`] (about `2 * top_k`) so a hit just past `top_k` can rise.
/// Test: `demotion_reorders_a_stale_snapshot_below_a_ruling`.
pub(crate) fn demote_stale_snapshots(results: &mut [RecallResult], now: DateTime<Utc>) {
    for r in results.iter_mut() {
        r.score *= temporal_weight(&r.drawer, now);
    }
    results.sort_by(by_score_desc);
}

/// Cross-palace counterpart of [`demote_stale_snapshots`].
///
/// Why: the review saw three outdated "Builder band as of" snapshots at ranks
/// 3-5 of `memory_recall_all`; the merged list needs the same weight.
/// What: identical rule over each hit's inner [`RecallResult`].
/// Test: `cross_palace_demotion_reorders_the_merged_list`.
pub(crate) fn demote_stale_snapshots_across(results: &mut [CrossPalaceResult], now: DateTime<Utc>) {
    for r in results.iter_mut() {
        r.result.score *= temporal_weight(&r.result.drawer, now);
    }
    results.sort_by(|a, b| by_score_desc(&a.result, &b.result));
}

#[cfg(test)]
#[path = "recall_rank_tests.rs"]
mod recall_rank_tests;
