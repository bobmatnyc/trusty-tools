//! Palace-level trusty-memory reads for the persona chat path (#7428).
//!
//! Why: until #7428 the chat path read exactly ONE palace, so the RPC helpers
//! could live inline in `persona_memory` and hardcode it. With one palace per
//! assistant plus an opt-in fan-out to other assistants' palaces, "which palace
//! did this drawer come from" became a fact the prompt has to carry — a drawer
//! from another assistant's memory is not this assistant's own recollection and
//! must never read as one. Splitting the palace-addressed reads out keeps that
//! provenance attached to the data from the moment it is deserialized, and
//! keeps `persona_memory` (the rendering and health half) under the 500-SLOC
//! cap.
//! What: [`Drawer`] is one drawer plus the palace it came from.
//! [`recall_across`] performs the turn's recalls — own palace first, then one
//! `memory_recall` RPC per fan-out palace — and merges them into one ranked
//! list. [`ensure_palace`] is the create-on-first-use path for a derived
//! palace. [`identity_drawers`] and [`memory_call`] are the tag-list and
//! one-call primitives, moved here verbatim from `persona_memory`.
//!
//! Fan-out is a READ grant only: nothing in this module writes, and
//! `persona_memory::spawn_persist_turn_with_activity` persists to the own
//! palace alone.
//!
//! Test: `persona_memory_tests.rs` — the `recall_across` / `ensure_palace`
//! cases against the mock daemon.

use std::path::Path;

use serde::Deserialize;
use serde_json::json;

use crate::assistants::PalacePlan;

/// One drawer, carrying the palace it was recalled from.
///
/// Why: `palace` is the provenance the rendered memory block has to state —
/// see this module's doc comment. `score` is trusty-memory's own ranking, kept
/// so drawers from several palaces can be merged into ONE ranked list rather
/// than concatenated per palace (which would rank a weak own-palace hit above a
/// strong fan-out one purely by position).
/// Test: `build_persona_memory_recalls_across_fan_out_palaces`.
#[derive(Debug, Clone)]
pub(super) struct Drawer {
    pub(super) palace: String,
    pub(super) content: String,
    pub(super) tags: Vec<String>,
    pub(super) score: f64,
}

/// Minimal projection of trusty-memory's `Drawer` — the recall and
/// list-drawers routes both return this shape. `score` is present on recall
/// rows only, so it defaults to `0.0` for the tag-list path.
#[derive(Debug, Deserialize)]
struct DrawerRow {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    score: f64,
}

/// Fetch drawers carrying an exact `tag` from `palace`.
///
/// Returns `Err` with a human-readable reason so the caller can surface it in
/// the introspection block rather than silently degrading to "no memory".
pub(super) async fn identity_drawers(
    socket: &Path,
    palace: &str,
    tag: &str,
    limit: usize,
) -> Result<Vec<Drawer>, String> {
    let raw = memory_call(
        socket,
        "memory.drawers_list",
        json!({ "palace_id": palace, "tag": tag, "limit": limit }),
    )
    .await?;
    let rows: Vec<DrawerRow> =
        serde_json::from_value(raw).map_err(|e| format!("unreadable drawer list: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|row| row.into_drawer(palace))
        .collect())
}

impl DrawerRow {
    fn into_drawer(self, palace: &str) -> Drawer {
        Drawer {
            palace: palace.to_string(),
            content: self.content,
            tags: self.tags,
            score: self.score,
        }
    }
}

/// Semantically recall `query` against ONE palace.
///
/// The parameter is `query`, not the `q` the retired REST route took (#6286).
/// The tool answers `{palace, query, results}` where the route answered a bare
/// array, so the rows come out of `results`. trusty-memory's `memory_recall`
/// takes exactly one palace, which is why fan-out is one call per palace rather
/// than one call with a list.
async fn recall_one(
    socket: &Path,
    palace: &str,
    query: &str,
    top_k: usize,
) -> Result<Vec<Drawer>, String> {
    let raw = memory_call(
        socket,
        "memory_recall",
        json!({ "palace": palace, "query": query, "top_k": top_k }),
    )
    .await?;
    let results = raw
        .get("results")
        .cloned()
        .ok_or_else(|| format!("recall from `{palace}` answered no results member"))?;
    let rows: Vec<DrawerRow> =
        serde_json::from_value(results).map_err(|e| format!("unreadable recall response: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|row| row.into_drawer(palace))
        .collect())
}

/// Recall `query` across the turn's whole [`PalacePlan`] — own palace first,
/// then each opted-in fan-out palace — merged into one ranked list.
///
/// Why: the user asked for these palaces to be read together, so they must be
/// RANKED together. Concatenating per palace would put a weak own-palace hit
/// above a strong fan-out one purely by position, and truncating per palace
/// would drop the best answer whenever it lived on the far side of the fan-out.
/// What: one `memory_recall` RPC per palace (trusty-memory takes exactly one
/// palace per call), merged and sorted by descending score. The sort is STABLE
/// and the own palace's drawers are inserted first, so an exact score tie
/// resolves to the assistant's own memory. The merged list is capped at `top_k`
/// — the same prompt budget one palace had, not one budget per palace.
///
/// The own palace decides the turn's health: its failure is returned as `Err`
/// and rendered as "temporarily unreachable". A FAN-OUT palace that fails is
/// logged and skipped, because another assistant's memory being unreadable does
/// not make this assistant's memory unavailable.
/// Test: `build_persona_memory_recalls_across_fan_out_palaces`,
/// `recall_without_fan_out_makes_exactly_one_recall_call`,
/// `recall_ranks_across_palaces_with_own_winning_ties`.
pub(super) async fn recall_across(
    socket: &Path,
    plan: &PalacePlan,
    query: &str,
    top_k: usize,
) -> Result<Vec<Drawer>, String> {
    let Some(own) = plan.own.as_deref() else {
        return Ok(Vec::new());
    };
    let mut merged = recall_one(socket, own, query, top_k).await?;
    for palace in &plan.fan_out {
        match recall_one(socket, palace, query, top_k).await {
            Ok(rows) => merged.extend(rows),
            Err(reason) => tracing::warn!(
                %palace,
                %reason,
                "fan-out palace did not answer; this assistant's own memory is unaffected"
            ),
        }
    }
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    Ok(merged)
}

/// Create `palace` if it does not exist yet.
///
/// Why: #7428 makes every assistant's palace default to its instance id, so the
/// first turn of a new assistant addresses a palace nobody has created. Creating
/// it here is the ONLY correct answer: falling back to a fan-out palace, a
/// shared palace or any default would write this assistant's memory into someone
/// else's, which is precisely what one-palace-per-assistant exists to prevent.
/// A creation failure is therefore reported as unavailable memory, never
/// substituted.
/// What: `palace_create` with `force: true` — idempotent, matching
/// `TrustyMemoryClient::ensure_palace` and `workstreams::create_tagged_drawer_at`,
/// so this is safe to issue on every turn. Only called for a DERIVED palace: a
/// palace named by a `[[stores]]` binding is operator-declared and already
/// exists (see `PalacePlan::own_is_bound`).
/// Test: `build_persona_memory_uses_the_instance_id_palace_without_a_binding`,
/// `build_persona_memory_reports_unavailable_when_palace_create_fails`.
pub(super) async fn ensure_palace(socket: &Path, palace: &str) -> Result<(), String> {
    memory_call(
        socket,
        "palace_create",
        json!({
            "name": palace,
            "description": "Assistant memory palace (auto-created by trusty-agents, #7428)",
            "force": true,
        }),
    )
    .await
    .map(|_| ())
}

/// One call on the trusty-memory daemon, with this path's error prose.
///
/// Why the reason strings are kept verbatim: they are rendered into the
/// persona's introspection block, so "trusty-memory unreachable" has to stay
/// distinguishable from a daemon that answered and refused.
pub(super) async fn memory_call(
    socket: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    trusty_common::memory_rpc::call_memory_tool_at(socket, method, params)
        .await
        .map_err(|e| format!("trusty-memory unreachable or refused: {e:#}"))
}
