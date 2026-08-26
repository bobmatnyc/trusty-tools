//! The three daemon calls `prompt-context` makes (#6286).
//!
//! Why: isolates the network hops — global hot facts, per-palace recall,
//! per-palace KG triples — so each can be read and extended without touching
//! the orchestration in `mod.rs`.
//!
//! What changed with ADR-0032: these were three `GET`s against
//! `/api/v1/kg/prompt-context`, `…/recall` and `…/kg/all`, built from a
//! `reqwest::Client` and a base URL read out of the `http_addr` discovery file.
//! They are now three [`crate::client::call_at`] frames against the daemon's
//! socket. Two of the three were already tool names the dispatcher routed —
//! `get_prompt_context` and `memory_recall` — so the REST routes were
//! duplicates; the third is the folded `memory.kg_all`.
//!
//! **Every one still degrades to empty on any failure.** This runs inside
//! Claude Code's `UserPromptSubmit` hook, so a degraded or absent daemon must
//! cost the user nothing but the missing context. That is why each helper
//! swallows its error rather than propagating it, and why the caller passes a
//! sub-second budget.
//!
//! Test: `prompt_context_recalls_palace_drawers`,
//! `prompt_context_empty_palace_falls_back_to_global`.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use super::filter::{RawTriple, RecalledDrawer};
use super::EMPTY_PLACEHOLDER;

/// How many triples to pull before filtering them in memory.
///
/// The filter is a substring scan over subjects, so 200 keeps it cheap while
/// covering any palace whose ambient facts are worth injecting at all.
const KG_TRIPLE_LIMIT: u64 = 200;

/// Fetch the global prompt-context block (workspace hot facts).
///
/// Why: keeps workspace-level aliases and conventions surfacing even when the
/// palace itself is empty.
/// What: calls `get_prompt_context`; returns `Some` only for a non-empty,
/// non-placeholder body. Any failure is `None`.
/// Test: `prompt_context_recalls_palace_drawers`,
/// `prompt_context_empty_palace_falls_back_to_global`.
pub(super) async fn fetch_global_prompt_context(
    socket: &Path,
    timeout: Duration,
) -> Option<String> {
    let result = crate::client::call_at(socket, "get_prompt_context", json!({}), timeout)
        .await
        .ok()?;
    // The tool answers a bare string when it has one; anything else is not a
    // block to inject.
    let body = match result {
        Value::String(s) => s,
        _ => return None,
    };
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed == EMPTY_PLACEHOLDER {
        None
    } else {
        Some(body)
    }
}

/// Fetch up to `top_k` recalled drawers from the palace.
///
/// Why (#134): surfacing relevant memories is the whole value of automatic
/// context injection.
/// What: calls `memory_recall`, which answers `{palace, query, results: [...]}`
/// — the REST route this replaces returned the bare array, so the entries are
/// read out of `results` here. An empty prompt, a refusal, or an unreachable
/// daemon all yield an empty vec.
/// Test: `prompt_context_recalls_palace_drawers`.
pub(super) async fn fetch_palace_recall(
    socket: &Path,
    timeout: Duration,
    palace: &str,
    prompt: &str,
    top_k: usize,
) -> Vec<RecalledDrawer> {
    if prompt.is_empty() {
        return Vec::new();
    }
    let Ok(result) = crate::client::call_at(
        socket,
        "memory_recall",
        json!({ "palace": palace, "query": prompt, "top_k": top_k }),
        timeout,
    )
    .await
    else {
        return Vec::new();
    };
    let Some(entries) = result.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(RecalledDrawer::from_recall_entry)
        // Drop the synthetic L0 identity drawer — it leaks the palace bootstrap
        // message, which is noise in the injection.
        .filter(|d| d.layer.unwrap_or(0) > 0)
        .take(top_k)
        .collect()
}

/// Fetch active KG triples from the palace.
///
/// Why (#134): subject-anchored triples — `tga is_alias_for
/// trusty-git-analytics`, `rust is-a language` — are exactly the ambient facts
/// a model benefits from when the prompt names one of those subjects.
/// What: calls the folded `memory.kg_all` with a [`KG_TRIPLE_LIMIT`] page;
/// returns the raw triples, empty on any failure.
/// Test: `prompt_context_recalls_palace_drawers`.
pub(super) async fn fetch_palace_kg_triples(
    socket: &Path,
    timeout: Duration,
    palace: &str,
) -> Vec<RawTriple> {
    let Ok(result) = crate::client::call_at(
        socket,
        "memory.kg_all",
        json!({ "palace_id": palace, "limit": KG_TRIPLE_LIMIT }),
        timeout,
    )
    .await
    else {
        return Vec::new();
    };
    let Some(entries) = result.as_array() else {
        return Vec::new();
    };
    entries.iter().filter_map(RawTriple::from_value).collect()
}
