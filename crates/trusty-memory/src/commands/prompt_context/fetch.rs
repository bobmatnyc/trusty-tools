//! HTTP fetch helpers for `prompt-context`.
//!
//! Why: isolates the three network calls (global hot facts, per-palace recall,
//! per-palace KG triples) so each can be read, tested, and extended without
//! touching the orchestration logic in `mod.rs`.
//! What: exports `fetch_global_prompt_context`, `fetch_palace_recall`, and
//! `fetch_palace_kg_triples` — each a best-effort HTTP GET that degrades
//! gracefully to empty/None on any failure.
//! Test: all three are exercised by the integration tests in
//! `prompt_context::tests` (e.g. `prompt_context_recalls_palace_drawers`).

use super::filter::{RawTriple, RecalledDrawer};
use super::{EMPTY_PLACEHOLDER, PALACE_KG_ALL_PATH, PALACE_RECALL_PATH, PROMPT_CONTEXT_PATH};
use serde_json::Value;

/// Fetch the global prompt-context block (workspace hot facts).
///
/// Why: keeps the legacy behaviour intact — workspace-level aliases and
/// conventions continue to surface even when the palace itself is empty.
/// What: `GET /api/v1/kg/prompt-context`; returns `Some(body)` only on a
/// 2xx with a non-empty, non-placeholder body. Any failure → `None`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers` and
/// `prompt_context_empty_palace_falls_back_to_global`.
pub(super) async fn fetch_global_prompt_context(
    client: &reqwest::Client,
    base: &str,
) -> Option<String> {
    let url = format!("{base}{PROMPT_CONTEXT_PATH}");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed == EMPTY_PLACEHOLDER {
        None
    } else {
        Some(body)
    }
}

/// Fetch up to `top_k` recalled drawer entries from the palace.
///
/// Why (issue #134): the entire value of automatic context injection is
/// surfacing relevant memories; this is the real recall hop the prior
/// implementation lacked. Uses the existing `recall_handler` endpoint so
/// no new wire surface is introduced.
/// What: `GET /api/v1/palaces/{slug}/recall?q=<prompt>&top_k=<k>`; parses
/// the JSON array, returns `Vec<RecalledDrawer>`. Empty prompt or 4xx/5xx
/// returns an empty vec. Failures are swallowed.
/// Test: `prompt_context_recalls_palace_drawers`.
pub(super) async fn fetch_palace_recall(
    client: &reqwest::Client,
    base: &str,
    palace: &str,
    prompt: &str,
    top_k: usize,
) -> Vec<RecalledDrawer> {
    if prompt.is_empty() {
        return Vec::new();
    }
    let path = PALACE_RECALL_PATH.replace("{slug}", palace);
    let url = format!("{base}{path}");
    let resp = match client
        .get(&url)
        .query(&[("q", prompt.to_string()), ("top_k", top_k.to_string())])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = body.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(RecalledDrawer::from_recall_entry)
        // Drop the synthetic L0 identity drawer — it leaks the palace
        // bootstrap message which is noise in the injection.
        .filter(|d| d.layer.unwrap_or(0) > 0)
        .take(top_k)
        .collect()
}

/// Fetch active KG triples from the palace.
///
/// Why (issue #134): subject-anchored triples (`tga is_alias_for trusty-
/// git-analytics`, `rust is-a language`) are exactly the kind of ambient
/// facts the model benefits from when the prompt mentions one of those
/// subjects. We fetch up to 200 to keep the in-memory filter cheap.
/// What: `GET /api/v1/palaces/{slug}/kg/all?limit=200`; returns the raw
/// triple array, empty on any failure.
/// Test: `prompt_context_recalls_palace_drawers` (asserts KG section
/// appears when prompt mentions a known subject).
pub(super) async fn fetch_palace_kg_triples(
    client: &reqwest::Client,
    base: &str,
    palace: &str,
) -> Vec<RawTriple> {
    let path = PALACE_KG_ALL_PATH.replace("{slug}", palace);
    let url = format!("{base}{path}");
    let resp = match client.get(&url).query(&[("limit", "200")]).send().await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = body.as_array() else {
        return Vec::new();
    };
    arr.iter().filter_map(RawTriple::from_value).collect()
}
