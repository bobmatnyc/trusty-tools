//! Default exclusion of superseded drawers, and the historical opt-in
//! (#10609, ADR-0074, owner ruling D83).
//!
//! Why: ADR-0028 D6 demoted a replaced fact and kept it in the answer. Field
//! bug #10609 showed an agent resuming from a retired checkpoint that ranked
//! 0.295 behind its replacement at 0.764, with nothing marking it stale. Owner
//! ruling D83 (Bob, 2026-10-31): "exclude them, except for historical
//! searches". Recall leaves replaced facts out; a caller that wants history
//! asks for it and gets each one tagged with its replacement.
//! What: [`settle_superseded`] decides, for every hit, whether it is
//! superseded and removes it unless `include_superseded` is set.
//! "Superseded" is the union of two sources. The local source is the
//! retirement marker on the drawer row ([`Drawer::retired_by`]), which needs no
//! lookup and so cannot fail. The KG source is the `superseded_by` edge map, for
//! supersessions no slot retirement made (dream, share, hand-written
//! `kg_assert`). When the KG read failed or ran late, those edges are unknown:
//! the response then carries `supersession_unverified: true`
//! ([`annotate_response`]) instead of silently passing the drawers. A kept
//! superseded drawer is tagged `superseded: true` and `superseded_by: <id>`.
//! Test: `tools::recall_supersede_view_tests`; end to end in
//! `tests/recall_superseded_excluded.rs`.

use serde_json::{Value, json};

use super::recall_supersede::{RankedHit, Supersessions};

/// Read the optional `include_superseded` argument (historical search).
///
/// Why: default recall excludes replaced facts (ADR-0074). The name says what
/// the caller gets, "include the superseded ones", and mirrors the existing
/// `include_creator_tags`, so the two opt-ins read alike on the tool schema.
/// What: `args["include_superseded"]` as a bool; absent or non-boolean is
/// `false`.
/// Test: `include_superseded_defaults_to_false`.
pub(crate) fn include_superseded_arg(args: &Value) -> bool {
    args.get("include_superseded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Fold each hit's local retirement marker into `sup`, then drop superseded
/// hits unless `include_superseded`.
///
/// Why: exclusion must not depend on the KG lookup for a slot retiree, the
/// #10609 case. The marker is read off the drawer row already in `results`.
/// What: for every hit with a marker, `sup[id] = replacement` (the marker wins
/// over an edge: it was written in the retirement commit and names the actual
/// replacement). With `include_superseded == false`, removes every hit whose
/// id is in `sup`, before any floor or `top_k` cut. With `true`, removes
/// nothing; `sup` then feeds the demotion and the response tags. Returns the
/// number removed.
/// Test: `a_marked_drawer_is_excluded_without_any_edge`,
/// `an_edge_only_supersession_is_excluded`,
/// `include_superseded_keeps_and_records_the_replacement`.
pub(crate) fn settle_superseded<T: RankedHit>(
    results: &mut Vec<T>,
    sup: &mut Supersessions,
    include_superseded: bool,
) -> usize {
    for r in results.iter() {
        let d = &r.hit().drawer;
        if let Some(replacement) = d.retired_by() {
            sup.insert(d.id, replacement);
        }
    }
    if include_superseded || sup.is_empty() {
        return 0;
    }
    let before = results.len();
    results.retain(|r| !sup.contains_key(&r.hit().drawer.id));
    before - results.len()
}

/// Tag the kept superseded rows and flag an unverified lookup on `out`.
///
/// Why: a historical search must say which rows are replaced facts and by
/// what, and a recall whose edge lookup failed must say it could not check.
/// What: for each `out["results"]` row whose `drawer_id` is in `sup`, sets
/// `superseded: true` and `superseded_by: "<id>"`. When `unverified`, sets the
/// top-level `supersession_unverified: true`. Both keys are absent otherwise,
/// so a response with nothing to report keeps its prior shape.
/// Test: `include_superseded_keeps_and_records_the_replacement`,
/// `an_unverified_lookup_is_flagged_on_the_response`.
pub(crate) fn annotate_response(out: &mut Value, sup: &Supersessions, unverified: bool) {
    if let Some(rows) = out.get_mut("results").and_then(Value::as_array_mut) {
        for row in rows {
            let replacement = row
                .get("drawer_id")
                .and_then(Value::as_str)
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .and_then(|id| sup.get(&id));
            if let (Some(replacement), Some(obj)) = (replacement, row.as_object_mut()) {
                obj.insert("superseded".into(), json!(true));
                obj.insert("superseded_by".into(), json!(replacement.to_string()));
            }
        }
    }
    if unverified && let Some(obj) = out.as_object_mut() {
        obj.insert("supersession_unverified".into(), json!(true));
    }
}

#[cfg(test)]
#[path = "recall_supersede_view_tests.rs"]
mod recall_supersede_view_tests;
