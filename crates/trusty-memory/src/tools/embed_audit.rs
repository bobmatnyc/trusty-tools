//! Answer "is this findable?" directly, per id and per estate (#5000, #4786).
//!
//! Why: `palace_reembed` reports one palace's whole missing set, and
//! `console_metrics` reports at most [`MAX_PALACES_IN_REPORT`] of them from
//! cache. Neither answers the two questions that actually get asked.
//!
//! A deletion-bearing workflow asks about ITS OWN ids — #4834 deleted 72 source
//! files after a content-recall check, and recall can hit lexically, so a drawer
//! can be present, durable and permanently unfindable while that check passes.
//! The only way to ask before this module was to pull the full missing-set dump
//! and diff it caller-side. [`handle_palace_verify_embedded`] is the direct
//! answer.
//!
//! A health sweep asks about EVERY palace. `console_metrics` cannot serve as
//! one: it caps its report, and an uncached palace reports 0/0, which reads as
//! healthy. A live sweep found palace `localLLM` fully unembedded — 15 drawers,
//! zero vectors — invisible to it, and #4786 reports three palaces at
//! `drawer_count: 0` that nothing flagged. [`handle_palace_embed_sweep`]
//! enumerates from DISK, uncapped, and opens each palace rather than peeking at
//! a cache.
//!
//! ## The signal, and the one #5000 struck out
//!
//! Neither handler compares `drawer_count` to `vector_count`. #5005 disproved
//! that gap in both directions: palace `trusty-tools` read a gap of 4 with 0
//! drawers missing (id aliasing, nothing absent), and orphan vectors can mask a
//! real hole the other way. What is reported instead is
//! [`EmbedHealth::missing_vector_ids`] plus the alias audit's `key_rows` versus
//! `distinct_vector_ids` — the pair that catches both classes. An audit that
//! could not RUN is never a pass: it reports `unavailable`, and a caller gating
//! a deletion on it must treat that as a block.
//!
//! Test: `verify_embedded_names_the_unembedded_id`,
//! `verify_embedded_separates_an_unknown_id_from_an_unembedded_one`,
//! `embed_sweep_sees_a_palace_the_cache_never_opened`.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use super::helpers::{open_palace_handle, resolve_palace};
use crate::AppState;

/// Read a palace's coverage into the shape both handlers report.
///
/// Why: the two handlers differ in what they enumerate, never in how they judge
/// a palace. Sharing the judgement is what stops the sweep and the per-id check
/// disagreeing about the same palace.
/// What: `(drawer_count, vector_count, missing_ids, audit_state, key_rows,
/// distinct_ids)`. `key_rows`/`distinct_ids` are `None` — never `0` — when the
/// audit could not run, because a zero standing in for "I could not tell" is the
/// false all-clear #5005 documents.
fn coverage(handle: &trusty_common::memory_core::PalaceHandle) -> Value {
    let health = handle.embed_health();
    json!({
        "palace": health.palace_id,
        "drawer_count": health.drawer_count,
        "vector_count": health.vector_count,
        "missing": health.missing_vector_ids.len(),
        "alias_audit": audit_state(&health.alias_audit),
        "alias_audit_error": health.alias_audit.unavailable_reason(),
        "vector_key_rows": health.alias_audit.counts().map(|(rows, _)| rows),
        "distinct_vector_ids": health.alias_audit.counts().map(|(_, ids)| ids),
        "aliased": health.alias_audit.aliased_drawer_ids().map(<[Uuid]>::len),
        "embedder_ready": health.embedder_ready,
        "healthy": health.is_healthy(),
    })
}

/// The alias audit as one of three words the caller can branch on.
///
/// `unavailable` is deliberately not `false`: the scan failed and nothing is
/// known, which is a block for a deletion workflow, not a soft warning.
fn audit_state(audit: &trusty_common::memory_core::retrieval::AliasAudit) -> &'static str {
    if audit.unavailable_reason().is_some() {
        "unavailable"
    } else if audit.is_clean() {
        "clean"
    } else {
        "aliased"
    }
}

/// `palace_verify_embedded` — are THESE drawer ids findable?
///
/// Why (#5000 resolution item 2): a migration or deletion workflow holds a list
/// of ids and needs a yes or no about exactly those. `palace_reembed` answers a
/// different question — the palace's entire missing set — so the caller had to
/// fetch that dump and diff it itself, and `memory_recall` is not a substitute
/// because it can hit lexically and pass on a drawer no vector search will ever
/// return.
///
/// What: partitions the requested ids three ways. `embedded` is findable now.
/// `missing` exists as a drawer with no vector. `unknown` is not a drawer in
/// this palace at all — a distinction a deletion workflow needs, since "already
/// gone" and "here but unfindable" call for opposite actions. `verified` is the
/// single boolean to gate on: every requested id embedded AND the alias audit
/// clean, so an id-collision victim (which HAS a vector key and is still
/// unreachable) cannot pass.
///
/// # Errors
///
/// When the palace cannot be resolved or opened, or when `drawer_ids` is absent,
/// empty, or holds a value that is not a UUID. A malformed id is refused rather
/// than skipped: a caller about to delete files must not have one of its ids
/// silently dropped from the answer.
///
/// Test: `verify_embedded_names_the_unembedded_id`,
/// `verify_embedded_separates_an_unknown_id_from_an_unembedded_one`,
/// `verify_embedded_refuses_a_malformed_id`.
pub(crate) async fn handle_palace_verify_embedded(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "palace_verify_embedded")?;
    let raw = args
        .get("drawer_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("palace_verify_embedded: missing 'drawer_ids' (array of UUIDs)"))?;
    if raw.is_empty() {
        return Err(anyhow!(
            "palace_verify_embedded: 'drawer_ids' is empty — nothing to verify"
        ));
    }
    let mut requested = Vec::with_capacity(raw.len());
    for value in raw {
        let text = value.as_str().ok_or_else(|| {
            anyhow!("palace_verify_embedded: every entry in 'drawer_ids' must be a UUID string")
        })?;
        // #5000: refused, not skipped. A caller about to delete source files
        // must not have an id quietly dropped from the answer it gates on.
        let id = Uuid::parse_str(text)
            .map_err(|e| anyhow!("palace_verify_embedded: '{text}' is not a UUID: {e}"))?;
        requested.push(id);
    }

    let handle = open_palace_handle(state, &palace)?;
    let health = handle.embed_health();
    let live: std::collections::HashSet<Uuid> =
        handle.drawers.read().iter().map(|d| d.id).collect();
    let no_vector: std::collections::HashSet<Uuid> =
        health.missing_vector_ids.iter().copied().collect();

    let (mut embedded, mut missing, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    for id in requested {
        if !live.contains(&id) {
            unknown.push(id.to_string());
        } else if no_vector.contains(&id) {
            missing.push(id.to_string());
        } else {
            embedded.push(id.to_string());
        }
    }

    let audit = audit_state(&health.alias_audit);
    Ok(json!({
        "palace": health.palace_id,
        "embedded": embedded,
        "missing": missing,
        "unknown": unknown,
        "alias_audit": audit,
        "alias_audit_error": health.alias_audit.unavailable_reason(),
        // The one field to gate a deletion on. An aliased or unreadable audit
        // fails it even when every requested id has a vector key (#5005).
        "verified": missing.is_empty() && unknown.is_empty() && audit == "clean",
    }))
}

/// `palace_embed_sweep` — coverage for every palace on disk, uncapped.
///
/// Why (#5000 resolution item 1, #4786): `console_metrics` cannot serve as a
/// health sweep. It caps its report at 20 palaces, and an uncached palace
/// reports 0/0 — which reads as healthy, so a fully-unembedded palace is
/// invisible rather than alarming. #4786's three palaces at `drawer_count: 0`
/// went unexplained for the same reason: nothing enumerated the estate and said
/// what it found.
///
/// What: enumerates palaces from DISK via `PalaceRegistry::list_palaces` rather
/// than from the handle cache, opens each, and reports the #5005-corrected
/// signal per palace. A palace that cannot be opened is reported with its error
/// instead of being omitted — an absent row and a healthy row must not look the
/// same. `unhealthy` counts the rows a caller should act on, `unreadable` the
/// ones nothing is known about; both are non-zero blocks for a deletion
/// workflow.
///
/// # Errors
///
/// Only when the palace directory itself cannot be listed. A per-palace failure
/// is reported in its row.
///
/// Test: `embed_sweep_sees_a_palace_the_cache_never_opened`,
/// `embed_sweep_reports_a_palace_it_could_not_open`.
pub(crate) async fn handle_palace_embed_sweep(state: &AppState, _args: Value) -> Result<Value> {
    let root = state.data_root.clone();
    // #4786: from disk, not `state.registry.list()`. The cache holds only what
    // this process has already opened, which is the blind spot that let three
    // active palaces report zero drawers with nothing raising it.
    let palaces = tokio::task::spawn_blocking(move || {
        trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
    })
    .await
    .map_err(|e| anyhow!("join list_palaces: {e}"))?
    .map_err(|e| anyhow!("list palaces: {e:#}"))?;

    let mut rows = Vec::with_capacity(palaces.len());
    let (mut unhealthy, mut unreadable) = (0usize, 0usize);
    for palace in &palaces {
        let id = palace.id.as_str();
        match open_palace_handle(state, id) {
            Ok(handle) => {
                let row = coverage(&handle);
                if row.get("healthy").and_then(Value::as_bool) != Some(true) {
                    unhealthy += 1;
                }
                rows.push(row);
            }
            Err(e) => {
                unreadable += 1;
                tracing::warn!(palace = %id, "embed sweep could not open palace: {e:#}");
                rows.push(json!({ "palace": id, "error": format!("{e:#}") }));
            }
        }
    }

    Ok(json!({
        "palaces": rows,
        "palace_count": palaces.len(),
        "unhealthy": unhealthy,
        "unreadable": unreadable,
    }))
}
