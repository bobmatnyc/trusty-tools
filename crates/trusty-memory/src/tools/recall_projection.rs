//! What a recall hit shows its caller — the tag view and the score floor
//! (owner ruling 2026-09-14, token savings).
//!
//! Why: every drawer this daemon writes carries roughly four `creator:*`
//! attribution tags (client, version, source, cwd), and `memory_recall`
//! returned all of them on every hit — so most of a recall response's `tags`
//! bytes were provenance no caller reads. Separately, the L2 lane returns
//! loosely related drawers in the 0.38-0.46 band alongside relevant ones, and
//! the caller had no way to decline them: `top_k` is a count, not a relevance
//! bar. Both are projection concerns, not storage ones — the attribution stays
//! in redb and stays queryable through `memory_list` and the `tag` filter.
//! What: [`include_creator_tags_arg`] and [`min_score_arg`] parse the two new
//! optional arguments, [`apply_score_floor`] drops below-floor query-scored
//! hits before the `top_k` cut and reports how many it removed, and
//! [`project_tags`] hides the `creator:*` namespace using the same
//! [`crate::attribution::is_creator_tag`] predicate the prompt-context renderer
//! already filters with. [`serialize_recall`] applies both.
//! Test: `tools::tests::recall_projection_tests`.

use serde_json::{json, Value};
use trusty_common::memory_core::retrieval::RecallResult;

/// The lowest recall layer a `min_score` floor is allowed to drop a hit from.
///
/// Why: L0 (palace identity) and L1 (essential drawers) are the palace's
/// always-on grounding rather than search results. `retrieval::layers` scores
/// L0 at a flat 1.0 and L1 by importance, so neither number is a similarity and
/// comparing it to a relevance floor would strip the caller's baseline context
/// on a threshold that never described it. Layers 2 and up (L2 semantic, L3
/// deep HNSW, L4 lexical) are all query-relevance scores, so the floor covers
/// every one of them.
/// What: `2`, the first query-scored layer.
/// Test: `score_floor_keeps_identity_and_essential_layers`.
pub(crate) const FIRST_FILTERABLE_LAYER: u8 = 2;

/// The two projection facts a recall response carries beyond its hits.
///
/// Why: `serialize_recall` needs to know whether to show attribution tags and
/// how many hits the floor removed. Passing them as one value keeps the
/// serializer's signature readable and keeps the two facts travelling together
/// from the handler that computed them.
/// What: a plain data carrier; no defaults, so each handler states both.
/// Test: `tools::tests::recall_projection_tests`.
pub(crate) struct RecallProjection {
    /// `true` when the caller explicitly asked for the `creator:*` tags back.
    pub include_creator_tags: bool,
    /// How many hits [`apply_score_floor`] removed, reported to the caller so
    /// filtering is visible rather than inferred from a short result list.
    pub dropped_below_floor: usize,
}

/// Read the optional `include_creator_tags` argument.
///
/// Why: attribution is hidden by default, so a caller who wants it must say so
/// — the opposite default would put the pre-ruling byte cost back on everyone.
/// What: `args["include_creator_tags"]` as a bool; anything absent or
/// non-boolean is `false`.
/// Test: `creator_tags_are_hidden_by_default`,
/// `creator_tags_come_back_when_the_flag_is_set`.
pub(crate) fn include_creator_tags_arg(args: &Value) -> bool {
    args.get("include_creator_tags")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Read the optional `min_score` floor.
///
/// Why: `None` means no floor, which is what every caller written before this
/// argument existed gets — the filter must be opt-in or it changes results
/// nobody asked it to change.
/// What: `args["min_score"]` as an `f32`; a missing or non-numeric value yields
/// `None`. No finiteness guard: JSON has no NaN or infinity literal, so
/// `as_f64` on a JSON number is always finite.
/// Test: `min_score_arg_is_absent_by_default`,
/// `min_score_arg_ignores_a_non_numeric_floor`.
pub(crate) fn min_score_arg(args: &Value) -> Option<f32> {
    args.get("min_score")
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
}

/// Drop query-scored hits below `min_score`, then cut to `top_k`.
///
/// Why (owner ruling 2026-09-14): the L2 band that prompted this returns
/// loosely related drawers at 0.38-0.46 next to relevant ones, and a caller
/// that only wants strong matches could previously only shrink `top_k` — which
/// discards good hits and bad ones alike. Filtering before the `top_k` cut is
/// the ordering that matters: cutting first would let a below-floor hit consume
/// a slot and then vanish, so the caller pays for it twice.
/// What: with `min_score` set, removes every hit at [`FIRST_FILTERABLE_LAYER`]
/// or above whose score is below the floor, counting the removals; L0/L1 are
/// never examined. Then truncates to `top_k` — this function owns the
/// `len() <= top_k` postcondition rather than inheriting it from whichever lane
/// produced `results`. Returns the number of hits the floor removed; `0` when
/// `min_score` is `None`.
/// Test: `score_floor_drops_below_and_keeps_above`,
/// `score_floor_keeps_identity_and_essential_layers`,
/// `score_floor_applies_before_top_k`.
pub(crate) fn apply_score_floor(
    results: &mut Vec<RecallResult>,
    min_score: Option<f32>,
    top_k: usize,
) -> usize {
    let dropped = match min_score {
        None => 0,
        Some(floor) => {
            let before = results.len();
            results.retain(|r| r.layer < FIRST_FILTERABLE_LAYER || r.score >= floor);
            before - results.len()
        }
    };
    results.truncate(top_k);
    dropped
}

/// The tag list a recall hit shows, with `creator:*` hidden by default.
///
/// Why: see the module doc — provenance dominated the `tags` array on every
/// hit. Reusing [`crate::attribution::is_creator_tag`] rather than re-spelling
/// the prefix keeps this projection in lock-step with the TUI, dashboard and
/// prompt-context renderers that already hide the same namespace.
/// What: borrows from `tags` and filters; nothing is copied and nothing is
/// removed from storage. `include_creator_tags` returns the stored list intact.
/// Test: `creator_tags_are_hidden_by_default`,
/// `creator_tags_come_back_when_the_flag_is_set`.
pub(crate) fn project_tags(tags: &[String], include_creator_tags: bool) -> Vec<&str> {
    tags.iter()
        .map(String::as_str)
        .filter(|t| include_creator_tags || !crate::attribution::is_creator_tag(t))
        .collect()
}

/// Serialize `recall` results into a JSON shape the MCP client can render.
///
/// Why: the one place `memory_recall` and `memory_recall_deep` turn hits into
/// wire bytes, so the tag projection and the floor's `dropped_below_floor`
/// count land on both tools by construction rather than by two edits that can
/// drift. Moved here from `tools::bm25` (owner ruling 2026-09-14) — the
/// lexical lane was never its subject.
/// What: one object per hit with the drawer's fields hoisted, `tags` narrowed
/// by [`project_tags`], wrapped in the `palace`/`query` envelope.
/// `dropped_below_floor` is always present, so `0` states "the floor removed
/// nothing" rather than leaving the caller to guess from a missing key.
/// Test: `recall_response_reports_the_dropped_count`,
/// `creator_tags_are_hidden_by_default`.
pub(crate) fn serialize_recall(
    palace: &str,
    query: &str,
    results: Vec<RecallResult>,
    projection: &RecallProjection,
) -> Value {
    let payload: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "drawer_id": r.drawer.id.to_string(),
                "content":   r.drawer.content(),
                "score":     r.score,
                "layer":     r.layer,
                "tags":      project_tags(&r.drawer.tags, projection.include_creator_tags),
                "importance": r.drawer.importance,
                "drawer_type": r.drawer.drawer_type.as_str(),
            })
        })
        .collect();
    json!({
        "palace": palace,
        "query": query,
        "results": payload,
        "dropped_below_floor": projection.dropped_below_floor,
    })
}
