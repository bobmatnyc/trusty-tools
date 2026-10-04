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
//! [`is_ruling`] is shared with the cross-palace rulings leg
//! (`super::recall_rulings`), so both agree on what a ruling is.
//! Test: `tools::recall_rank_tests`; end to end in
//! `tests/recall_temporal_rank.rs`.

use chrono::{DateTime, Utc};
use trusty_common::memory_core::palace::{Drawer, DrawerType};
use trusty_common::memory_core::retrieval::{CrossPalaceResult, RecallResult};

/// Tags that mark a drawer as an owner ruling or decision.
///
/// Why: drawers carry no ruling type yet (#9138 O5), so tags are the only
/// signal. These are the spellings the review counted in the `supervisor`
/// palace (`bob-ruling` 36, `standing-rule` 11) and ADR-0028 §C4
/// (`bob-decision`), plus their generic forms. `standing-instruction` is left
/// out on purpose: §C4 found it on one-shot actions as well as rules.
/// What: compared exactly against each stored tag.
/// Test: `ruling_tags_exempt_a_drawer_that_is_also_tagged_status`.
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
/// measured in trusty-tools (1,482 and 1,318 drawers, almost none keyed).
/// What: compared exactly against each stored tag.
/// Test: `a_stale_snapshot_loses_score_and_a_fresh_one_does_not`.
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
/// `ruling_tags_exempt_a_drawer_that_is_also_tagged_status`.
pub(crate) fn temporal_weight(drawer: &Drawer, now: DateTime<Utc>) -> f32 {
    if is_ruling(drawer) || !is_snapshot(drawer) {
        return 1.0;
    }
    if drawer.fact_key.is_some() && !drawer.is_expired_at(now) {
        return 1.0;
    }
    let age_hours = (now - drawer.created_at).num_seconds().max(0) as f32 / 3600.0;
    let weight = SNAPSHOT_FLOOR
        + (1.0 - SNAPSHOT_FLOOR) * 0.5_f32.powf(age_hours / SNAPSHOT_HALF_LIFE_HOURS);
    debug_assert!((SNAPSHOT_FLOOR..=1.0).contains(&weight));
    weight
}

/// Descending score order; NaN compares equal so the sort never panics.
fn by_score_desc(a: f32, b: f32) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

/// Demote stale snapshots in a single-palace recall and re-sort.
///
/// Why: the handlers sort by score once the lanes are fused; a weight applied
/// after that sort must re-sort or it changes numbers without changing order.
/// What: multiplies every result's score by [`temporal_weight`], then
/// stable-sorts descending, so equal scores keep the lane's order. The list
/// length is unchanged; the caller's floor and `top_k` cut run after this.
/// Test: `demotion_reorders_a_stale_snapshot_below_a_ruling`.
pub(crate) fn demote_stale_snapshots(results: &mut [RecallResult], now: DateTime<Utc>) {
    for r in results.iter_mut() {
        r.score *= temporal_weight(&r.drawer, now);
    }
    results.sort_by(|a, b| by_score_desc(a.score, b.score));
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
    results.sort_by(|a, b| by_score_desc(a.result.score, b.result.score));
}

#[cfg(test)]
#[path = "recall_rank_tests.rs"]
mod recall_rank_tests;
