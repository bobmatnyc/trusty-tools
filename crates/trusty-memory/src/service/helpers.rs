//! Free helper functions for the trusty-memory service layer.
//!
//! Why: thin no-IO transforms (preview/snippet/recall-entry JSON), palace-stat
//! aggregation, palace-info enrichment, and gaps-cache refresh are shared by
//! the HTTP handlers, the chat dispatcher, and the `MemoryService` core (split
//! out of the former monolithic `service.rs`, issue #607). User-config
//! loading + `DreamConfig` derivation live in the sibling `user_config`
//! module (split out of this file, issue #2593 follow-up, to stay under the
//! 500-SLOC cap).
//! What: the free helpers + `service_result_to_anyhow`, moved verbatim. The
//! service-layer unit tests live here too (1500-SLOC test cap applies).
//! Test: `drawer_*`, `recall_entry_*`, and `list_drawers_*` in `service::tests`.

use crate::AppState;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::retrieval::RecallResult;
use trusty_common::memory_core::PalaceHandle;
use uuid::Uuid;

use super::types::{PalaceInfo, ServiceResult};

#[cfg(test)]
use super::core::MemoryService;
#[cfg(test)]
use super::types::ListDrawersQuery;

// ---------------------------------------------------------------------------
// Free helper functions kept module-public so `web.rs` and `chat.rs` can use
// them without going through the `MemoryService` wrapper. Each is a thin
// transform (no IO, no global state).
// ---------------------------------------------------------------------------

/// Maximum characters retained in a drawer's content preview.
pub const DRAWER_PREVIEW_MAX_CHARS: usize = 80;

/// Maximum characters retained in a drawer-row snippet (issue #202).
///
/// Why: the TUI activity panel renders the snippet inline at the end of a
/// narrow row (`<id> <ts> <creator>  <snippet>`); 60 chars is short
/// enough to keep the row readable while still showing the key phrase
/// of most drawers.
/// What: 60 characters; the trailing `…` from [`drawer_snippet`] counts
/// against this budget.
/// Test: `drawer_snippet_truncates_long_content`.
pub const DRAWER_SNIPPET_MAX_CHARS: usize = 60;

/// Build a single-line preview of drawer content for SSE events.
///
/// Why: the activity feed should show *what* was just stored; multiline /
/// whitespace-heavy bodies otherwise blow out the log row.
/// What: collapses whitespace, trims, truncates to
/// [`DRAWER_PREVIEW_MAX_CHARS`] with `…` when cut.
/// Test: `drawer_preview_collapses_whitespace_and_truncates`.
pub fn drawer_content_preview(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_PREVIEW_MAX_CHARS {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_PREVIEW_MAX_CHARS.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

/// Build a short snippet from a drawer's content for the TUI activity panel
/// row (issue #202).
///
/// Why: the activity panel renders one row per drawer at narrow column
/// width; a 60-char whitespace-collapsed snippet is long enough to convey
/// the gist but short enough to fit inline with the id / timestamp /
/// creator columns. Re-using the preview's whitespace-collapse rule keeps
/// SSE and `/drawers` snippets visually consistent.
/// What: collapses whitespace, trims, truncates to
/// [`DRAWER_SNIPPET_MAX_CHARS`] (60) with a trailing `…` when cut.
/// Returns the empty string for empty / whitespace-only content so the
/// caller can omit the `snippet` field entirely.
/// Test: `drawer_snippet_truncates_long_content`,
/// `drawer_snippet_handles_empty_content`.
pub fn drawer_snippet(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_SNIPPET_MAX_CHARS {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_SNIPPET_MAX_CHARS.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

/// Flatten a [`RecallResult`] into a single JSON object with the drawer's
/// fields hoisted to the top level (issue #69 shape).
///
/// Why: clients look for `content`/`tags`/`importance` at the top level of an
/// entry; nesting under `"drawer"` made recall appear empty.
/// What: serialises the drawer then inserts `score`/`layer`.
/// Test: `recall_entry_json_hoists_drawer_fields`.
pub fn recall_entry_json(r: RecallResult) -> Value {
    let mut obj = match serde_json::to_value(&r.drawer) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert("score".to_string(), json!(r.score));
    obj.insert("layer".to_string(), json!(r.layer));
    Value::Object(obj)
}

/// Reserved-prefix predicate for "system" palaces hidden from user listings.
///
/// Why: Issue #185 — the `/health` round-trip writes probe drawers into a
/// dedicated `__health_probe__` palace. That palace exists on disk but must
/// never appear in the admin UI, TUI, chat-tool palace roster, or any other
/// user-facing surface. Centralising the predicate here keeps the convention
/// (any palace id starting with `__`) in one place so future system palaces
/// inherit the same hidden-from-users behaviour automatically.
/// What: Returns `true` iff `id.as_str()` starts with the double-underscore
/// prefix. Pure function over the id — no I/O, no allocation.
/// Test: covered indirectly by `health_probe_palace_is_invisible` in
/// `web::tests` (drives a full `/health` round-trip and asserts the probe
/// palace does not appear in `MemoryService::list_palaces`).
pub(crate) fn is_reserved_system_palace(id: &PalaceId) -> bool {
    id.as_str().starts_with("__")
}

/// Aggregate counts summed across one or more palaces.
///
/// Why (issue #228): both `status()` (the `/api/v1/status` endpoint) and
/// `aggregate_status_event()` (the SSE `StatusChanged` payload) sum the same
/// three numbers across every persisted palace. The original implementation
/// inlined the same `for p in palaces` loop in both methods. Sharing a
/// single helper eliminates the byte-for-byte duplicate and makes future
/// changes (e.g. adding a `total_vectors_orphaned` field) land in one place.
/// What: saturating sums of `drawers.read().len()`, `vector_store.index_size()`,
/// and `kg.count_active_triples()` across the supplied palace ids, plus
/// `cached_palace_count` — how many of those ids actually contributed (issue
/// #4637). The three totals are therefore a **cache-resident** roll-up, not a
/// whole-disk one; `cached_palace_count` is what makes that legible.
/// Test: indirectly via `status_endpoint_returns_payload` and any SSE test
/// that observes `StatusChanged`; cache-only behaviour is pinned by
/// `status_does_not_open_uncached_palaces`.
pub(crate) struct PalaceStats {
    pub total_drawers: usize,
    pub total_vectors: usize,
    pub total_kg_triples: usize,
    /// Number of `ids` that were resident in the registry's LRU cache and so
    /// contributed real counts to the three totals above (issue #4637).
    pub cached_palace_count: usize,
}

/// Sum drawer / vector / KG-triple counts across the `ids` that are already
/// resident in the registry's open-handle cache.
///
/// Why (issue #228): centralises the previously-duplicated loop from
/// `status()` and `aggregate_status_event()`. Callers pass an iterator of
/// `PalaceId` so the helper works for both the on-disk view (used by
/// `status()`) and the in-memory registry view (used by
/// `aggregate_status_event()` on the SSE hot path).
/// Why (issue #4637): this used to call `registry.open_palace` per id. On a
/// daemon with 5,794 palaces on disk and a 64-slot LRU that is ~5,730 cold
/// opens of ~1s each — roughly 90 minutes of blocking disk I/O inline on the
/// async executor, which made `GET /api/v1/status` never respond. Switching
/// to `PalaceRegistry::peek` (a mutex lock + `Arc` clone, zero I/O, no LRU
/// promotion) makes the call O(n) in cheap lock acquisitions instead of O(n)
/// in cold palace opens. This mirrors the fix #1924 already landed in
/// `console_metrics.rs`.
/// What: for each id, `peek`s the registry and accumulates the three counts
/// via `saturating_add` so overflow is impossible. Ids that are not currently
/// cached contribute nothing and are counted only by the caller's own
/// `palace_count`. The totals are consequently a cache-resident
/// approximation — callers must surface `cached_palace_count` alongside them
/// so the wire shape says so.
/// Test: `status_does_not_open_uncached_palaces`, and indirectly through
/// `status_endpoint_returns_payload`.
pub(crate) fn collect_palace_stats<'a, I>(state: &AppState, ids: I) -> PalaceStats
where
    I: IntoIterator<Item = &'a PalaceId>,
{
    let (mut total_drawers, mut total_vectors, mut total_kg_triples): (usize, usize, usize) =
        (0, 0, 0);
    let mut cached_palace_count: usize = 0;
    for id in ids {
        // #4637: peek() not open_palace() — full-registry open is O(n) cold disk I/O
        if let Some(handle) = state.registry.peek(id) {
            total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
            total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
            total_kg_triples = total_kg_triples.saturating_add(kg_triple_count_or_zero(&handle));
            cached_palace_count += 1;
        }
    }
    PalaceStats {
        total_drawers,
        total_vectors,
        total_kg_triples,
        cached_palace_count,
    }
}

/// List every palace on disk without blocking the async executor.
///
/// Why (issue #4637): `PalaceRegistry::list_palaces` is a synchronous
/// directory walk that reads one `palace.json` per palace — at 5,794 palaces
/// that is thousands of `stat`+read syscalls. Every caller in the service
/// layer ran it inline on a tokio worker. `console_metrics.rs` already hops
/// to the blocking pool for exactly this call; this helper makes that the
/// shared behaviour instead of a one-off.
/// What: clones `data_root`, runs the walk on `spawn_blocking`, and folds
/// both the join error and the walk error into `anyhow`.
/// Test: exercised by every `list_palaces`/`status` service test.
pub(crate) async fn list_palaces_blocking(state: &AppState) -> Result<Vec<Palace>> {
    let root = state.data_root.clone();
    tokio::task::spawn_blocking(move || {
        trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
    })
    .await
    .map_err(|e| anyhow!("join list_palaces: {e}"))?
    .map_err(|e| anyhow!("list palaces: {e:#}"))
}

/// Open a handle for every palace in `palaces`, off the async executor.
///
/// Why (issue #4637): the cross-palace recall fan-out genuinely needs every
/// palace open — answering a recall from cache-resident palaces only would
/// silently drop ~98.9% of the corpus, which is a correctness regression, not
/// an optimisation. So `peek()` is the wrong tool here. What *was* wrong is
/// that the open loop ran inline on a tokio worker thread, parking it for the
/// full duration. This helper keeps the semantics (every palace is opened)
/// and moves the blocking work to the blocking pool where it belongs. Three
/// byte-identical copies of this loop previously existed (`MemoryService::recall_all`,
/// `chat::tools::execute_recall_all`, `tools::memory_ops::handle_memory_recall_all`).
/// What: clones the registry `Arc` + `data_root`, opens each palace serially
/// on `spawn_blocking` (serial on purpose — parallel cold opens would thrash
/// the 64-slot LRU), and skips failures with a `tracing::warn!` so one bad
/// palace cannot fail the whole fan-out. `label` names the caller in that
/// warning.
/// Test: `open_palaces_blocking_opens_every_palace` pins that the fan-out
/// still sees palaces that were never cached.
pub(crate) async fn open_palaces_blocking(
    state: &AppState,
    palaces: &[Palace],
    label: &'static str,
) -> Vec<Arc<PalaceHandle>> {
    let registry = Arc::clone(&state.registry);
    let root = state.data_root.clone();
    let ids: Vec<PalaceId> = palaces.iter().map(|p| p.id.clone()).collect();
    // #4637: open_palace is correct here (recall must see every palace) but must
    // not run inline on the async executor — hop to the blocking pool.
    tokio::task::spawn_blocking(move || {
        let mut handles = Vec::with_capacity(ids.len());
        for id in &ids {
            match registry.open_palace(&root, id) {
                Ok(h) => handles.push(h),
                Err(e) => tracing::warn!(palace = %id, "{label}: open failed: {e:#}"),
            }
        }
        handles
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("{label}: join open_palaces failed: {e}");
        Vec::new()
    })
}

/// Build a `PalaceInfo` from a `Palace` row plus an optional opened handle.
///
/// Why: both `list_palaces` and `get_palace` need the same enriched shape;
/// the helper avoids field-set drift between them.
/// What: reads drawer/vector/triple counts, distinct rooms, max
/// `created_at`, KG node/edge/community counts, and the `is_compacting` flag.
/// When `handle` is `None` every count is `0` and `cached` is `false` — since
/// issue #4637 that is the normal case on the full-registry list route, so
/// `cached` is what tells a client "these zeros mean unknown, not empty".
/// Test: `palace_list_includes_richer_counts`, `palace_list_includes_graph_counts`,
/// `list_palaces_does_not_open_uncached_palaces`.
/// Rooms registered in this palace's `ROOMS` table, or `None` on a read error.
///
/// Why (#4811 / ADR-0027 T8): `PalaceInfo` must report the same room set
/// `room_list` does. Kept as its own function so `palace_info_from` gains a
/// call rather than an implementation — `service/helpers.rs` sits close to the
/// 500-SLOC cap (ADR-0027 C5).
/// What: a scan of the (tiny — a few dozen rows) `ROOMS` table. `None` lets
/// the caller degrade to a drawer-derived lower bound instead of reporting 0.
/// Test: `palace_list_includes_richer_counts`.
fn room_registry_count(handle: &Arc<PalaceHandle>) -> Option<usize> {
    match handle.kg.store().list_rooms() {
        Ok(rooms) => Some(rooms.len()),
        Err(e) => {
            tracing::warn!(palace = %handle.id, "room_count unavailable: {e:#}");
            None
        }
    }
}

/// Wings registered in this palace's `WINGS` table, or `None` on a read error.
///
/// Why (#4809 / ADR-0027 T9): `wing_count` reported a hardcoded `1` while
/// `DEFAULT_WING_ID` was the only wing in the model — true then, and a lie the
/// moment T9 let a palace hold more than one. This makes it a real count, from
/// the same registry `wing_list` reads, so the two can never disagree.
/// What: a scan of the (tiny) `WINGS` table. `None` lets the caller degrade to
/// `1` — the default wing always exists once a palace has been opened — rather
/// than to a `0` that #4637's contract reserves for "unknown".
/// Test: `palace_list_includes_richer_counts`, `palace_info_reports_real_wings`.
fn wing_registry_count(handle: &Arc<PalaceHandle>) -> Option<usize> {
    match handle.kg.store().list_wings() {
        Ok(wings) => Some(wings.len()),
        Err(e) => {
            tracing::warn!(palace = %handle.id, "wing_count unavailable: {e:#}");
            None
        }
    }
}

/// Active-triple count for a diagnostic roll-up, or `0` when the read fails.
///
/// Why (#5384): `count_active_triples` is fallible so the callers whose answer
/// depends on it — `kg_query`'s `graph_state`, `kg_graph`'s `truncated`, the
/// REST count — surface a broken read instead of reporting an empty graph. The
/// status/metrics roll-ups have no field to carry "unknown", and #4637 already
/// fixed 0 as "unknown, never empty" for those totals, so they degrade. Keeping
/// the degrade in one named helper is what stops it from sinking back into the
/// store where no caller can see it. The read error this converts is raised,
/// and pinned, cross-crate in
/// `crates/trusty-common/src/memory_core/store/kg_redb/tests.rs` by
/// `count_active_triples_surfaces_read_failure`.
/// What: performs the read and hands it to [`triple_count_or_zero`], which
/// owns the degrade.
/// Test: `status_does_not_open_uncached_palaces` (success path, through the
/// status roll-up), `triple_count_or_zero_degrades_a_failed_read_to_zero` (the
/// error arm).
pub(crate) fn kg_triple_count_or_zero(handle: &Arc<PalaceHandle>) -> usize {
    triple_count_or_zero(&handle.id, handle.kg.count_active_triples())
}

/// Apply the degrade rule to an already-performed count read.
///
/// Why (#5489): `count_active_triples` fails only when its backing redb table
/// is unreadable, and nothing in this crate can force that — `KgStoreRedb::db()`
/// is `pub(super)` within trusty-common, so the failure cannot be induced from
/// here without widening that crate's public API. Taking the read as a
/// parameter puts the branch #5384 actually cares about — an error must become
/// a *logged* 0, never a silent one — under an in-crate test instead.
/// What: `Ok(n)` passes through; `Err` is logged at warn against the palace id
/// and becomes 0.
/// Test: `triple_count_or_zero_degrades_a_failed_read_to_zero`.
fn triple_count_or_zero(palace: &PalaceId, read: Result<usize>) -> usize {
    match read {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(palace = %palace, "kg_triple_count unavailable: {e:#}");
            0
        }
    }
}

pub fn palace_info_from(palace: &Palace, handle: Option<&Arc<PalaceHandle>>) -> PalaceInfo {
    let (
        drawer_count,
        vector_count,
        kg_triple_count,
        room_count,
        wing_count,
        last_write_at,
        node_count,
        edge_count,
        community_count,
        is_compacting,
    ) = if let Some(h) = handle {
        let drawers = h.drawers.read();
        let last_write = drawers.iter().map(|d| d.created_at).max();
        // #4811 / ADR-0027 T8: `room_count` comes from the ROOMS registry so it
        // agrees with `room_list`; on a read failure it degrades to the rooms
        // the drawers are actually in, which is a lower bound rather than a
        // zero that would read as "no rooms".
        let rooms = room_registry_count(h).unwrap_or_else(|| {
            drawers
                .iter()
                .map(|d| d.room_id)
                .collect::<HashSet<Uuid>>()
                .len()
        });
        // #4809 / ADR-0027 T9: `wing_count` is now a REAL count from the WINGS
        // registry. T8 set it to a hardcoded 1 because DEFAULT_WING_ID was the
        // only wing that could exist; that stopped being true when T9 shipped
        // `wing_create`, and a constant standing in for a count is the same
        // category of lie the field carried before T8. On a read failure it
        // degrades to 1 — every opened palace has at least the default wing —
        // preserving #4637's rule that 0 means unknown, never empty.
        let wings = wing_registry_count(h).unwrap_or(1);
        (
            drawers.len(),
            h.vector_store.index_size(),
            kg_triple_count_or_zero(h),
            rooms,
            wings,
            last_write,
            h.kg.node_count() as u64,
            h.kg.edge_count() as u64,
            h.kg.community_count() as u64,
            h.is_compacting(),
        )
    } else {
        (0, 0, 0, 0, 0, None, 0, 0, 0, false)
    };
    PalaceInfo {
        id: palace.id.0.clone(),
        name: palace.name.clone(),
        description: palace.description.clone(),
        drawer_count,
        vector_count,
        kg_triple_count,
        room_count,
        wing_count,
        created_at: palace.created_at,
        last_write_at,
        node_count,
        edge_count,
        community_count,
        is_compacting,
        cached: handle.is_some(),
    }
}

/// Recompute the gaps for `handle` and write them to the registry cache.
///
/// Why: the dream-run path needs this post-cycle bookkeeping; pulling it out
/// of `web.rs` keeps the dream code on one side of the wall.
/// What: calls `knowledge_gaps()`, optionally enriches via
/// `enrich_gap_exploration`, stores on `state.registry`. Logs gap count.
/// Test: indirectly via `kg_gaps_endpoint_returns_cached_gaps`.
pub async fn refresh_gaps_cache(state: &AppState, handle: &Arc<PalaceHandle>) {
    let mut gaps = handle.kg.knowledge_gaps();
    if let Ok(api_key) = std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY) {
        if !api_key.is_empty() {
            for gap in gaps.iter_mut() {
                if let Some(enriched) = enrich_gap_exploration(&api_key, gap).await {
                    gap.suggested_exploration = enriched;
                }
            }
        }
    }
    let gap_count = gaps.len();
    state.registry.set_gaps(handle.id.clone(), gaps);
    tracing::debug!(palace = %handle.id, gaps = gap_count, "community gaps updated");
}

/// Ask OpenRouter for a focused exploration question for a single gap.
///
/// Why: see `refresh_gaps_cache`.
/// What: builds a short user prompt, calls `openrouter_chat`, returns the
/// trimmed completion (or `None` on any failure).
/// Test: network-dependent — not unit-tested.
pub async fn enrich_gap_exploration(
    api_key: &str,
    gap: &trusty_common::memory_core::community::KnowledgeGap,
) -> Option<String> {
    let preview: Vec<&str> = gap.entities.iter().take(5).map(String::as_str).collect();
    if preview.is_empty() {
        return None;
    }
    let entities = preview.join(", ");
    let user = format!(
        "Given these related entities from a knowledge graph: {entities}. \
         Suggest one specific research question (single sentence, under 25 words) \
         that would help fill gaps in this knowledge cluster. Return only the question."
    );
    let messages = vec![trusty_common::ChatMessage {
        role: "user".to_string(),
        content: user,
        tool_call_id: None,
        tool_calls: None,
    }];
    #[allow(deprecated)]
    let res = trusty_common::openrouter_chat(api_key, "openai/gpt-4o-mini", messages).await;
    match res {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(e) => {
            tracing::debug!("openrouter gap enrichment failed (using template): {e:#}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// User config loading + DreamConfig derivation moved to `service::user_config`
// (split out, issue #2593 follow-up, to keep this file under the 500-SLOC
// cap): `LoadedUserConfig`, `load_user_config`, `dream_config_from_user_config`.
// `service::mod` re-exports them directly from there now — see that module's
// doc header. Nothing in this file references them anymore.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Convenience helpers for callers that want `anyhow::Result<Value>` shape.
// ---------------------------------------------------------------------------

/// Convert a `ServiceResult<T>` into `anyhow::Result<Value>` using a serializer.
///
/// Why: the chat tool dispatcher needs uniform `Result<Value>` returns to
/// shove into the LLM's `role: "tool"` message.
/// What: serialises `T` to JSON; on `Err`, returns the message as an
/// `anyhow::Error`. The HTTP layer does *not* go through this — it preserves
/// the `ServiceError` variant for status-code mapping.
/// Test: trivial wrapper; covered indirectly by the chat tests.
pub fn service_result_to_anyhow<T: serde::Serialize>(r: ServiceResult<T>) -> Result<Value> {
    match r {
        Ok(v) => serde_json::to_value(v).context("serialize service result"),
        Err(e) => Err(anyhow!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use trusty_common::memory_core::palace::{Drawer, Palace};

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Leak the TempDir guard so the directory survives the test body.
        std::mem::forget(tmp);
        AppState::new(root)
    }

    /// Issue #184 — `sort=created_desc` paginates newest-first and the
    /// importance default is preserved.
    ///
    /// Why: the TUI activity panel needs a stable creation-date ordering with
    /// offset pagination; the legacy importance-desc default must keep
    /// working for other callers (e.g. chat tool `list_drawers`).
    /// What: provisions a fresh palace, drops five drawers in with
    /// monotonically older `created_at` and shuffled importance, then drives
    /// `MemoryService::list_drawers` with two pages of `limit=2` and asserts
    /// the order is newest-first across both pages. Re-runs the same call
    /// with `sort` unset and confirms the order changes (importance-based).
    /// Test: this test.
    #[tokio::test]
    async fn list_drawers_creates_desc_paginates() {
        let state = test_state();
        // Provision a fresh palace via the registry.
        let palace = Palace {
            id: PalaceId::new("paging-test"),
            name: "paging-test".to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: state.data_root.join("paging-test"),
        };
        state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create_palace");

        // Open the handle and seed five drawers with staggered timestamps and
        // shuffled importance.
        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("paging-test"))
            .expect("open_palace");
        let room_id = Uuid::nil();
        let now = Utc::now();
        // Index 0 is newest; index 4 is oldest.
        for (i, importance) in [0.1f32, 0.9, 0.3, 0.7, 0.5].iter().enumerate() {
            // Built through `Drawer::new` rather than a struct literal: this
            // fixture only cares about importance and created_at, and a literal
            // made it fail to compile every time `Drawer` gained a field.
            let mut drawer = Drawer::new(room_id, format!("drawer-{i}"));
            drawer.importance = *importance;
            drawer.created_at = now - ChronoDuration::seconds(i as i64);
            drawer.tags = vec![format!("idx:{i}")];
            handle.add_drawer(drawer);
        }
        // The handle is `Arc<PalaceHandle>` and the registry caches it; drop
        // ours so the service can re-open from cache.
        drop(handle);

        let service = MemoryService::new(state.clone());

        // Page 1 (newest two) under created_desc — expects idx:0 then idx:1.
        let page1 = service
            .list_drawers(
                "paging-test",
                ListDrawersQuery {
                    limit: Some(2),
                    offset: Some(0),
                    sort: Some("created_desc".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("page 1");
        let arr = page1.as_array().expect("array");
        assert_eq!(arr.len(), 2, "page 1 must have 2 rows");
        assert_eq!(arr[0]["content"].as_str(), Some("drawer-0"));
        assert_eq!(arr[1]["content"].as_str(), Some("drawer-1"));

        // Page 2 — expects idx:2 then idx:3.
        let page2 = service
            .list_drawers(
                "paging-test",
                ListDrawersQuery {
                    limit: Some(2),
                    offset: Some(2),
                    sort: Some("created_desc".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("page 2");
        let arr = page2.as_array().expect("array");
        assert_eq!(arr.len(), 2, "page 2 must have 2 rows");
        assert_eq!(arr[0]["content"].as_str(), Some("drawer-2"));
        assert_eq!(arr[1]["content"].as_str(), Some("drawer-3"));

        // Page 3 — expects idx:4 alone.
        let page3 = service
            .list_drawers(
                "paging-test",
                ListDrawersQuery {
                    limit: Some(2),
                    offset: Some(4),
                    sort: Some("created_desc".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("page 3");
        let arr = page3.as_array().expect("array");
        assert_eq!(arr.len(), 1, "page 3 (tail) must have 1 row");
        assert_eq!(arr[0]["content"].as_str(), Some("drawer-4"));

        // Importance-desc default — first row is the highest-importance
        // drawer (idx:1 had importance 0.9), confirming we did not break
        // the legacy callers.
        let legacy = service
            .list_drawers(
                "paging-test",
                ListDrawersQuery {
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await
            .expect("legacy");
        let arr = legacy.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["content"].as_str(),
            Some("drawer-1"),
            "importance default should surface drawer with importance 0.9 first",
        );

        // Issue #202: every row carries an enriched `snippet` field
        // derived from the drawer body so the TUI activity panel can
        // render a glanceable summary without re-parsing.
        assert_eq!(
            arr[0]["snippet"].as_str(),
            Some("drawer-1"),
            "snippet must be populated for non-empty drawer content",
        );
    }

    /// Why: issue #202 — the snippet helper must collapse whitespace,
    /// trim, and cap at [`DRAWER_SNIPPET_MAX_CHARS`] with a trailing `…`
    /// when the body overflows, matching the SSE preview's shape but at
    /// a tighter width.
    /// What: feeds a multiline / whitespace-heavy body and asserts both
    /// the truncation and the collapse rule.
    /// Test: itself.
    #[test]
    fn drawer_snippet_truncates_long_content() {
        // Short content round-trips verbatim.
        assert_eq!(drawer_snippet("hello world"), "hello world");

        // Whitespace is collapsed.
        assert_eq!(
            drawer_snippet("first line\n\nsecond\tline   third"),
            "first line second line third",
        );

        // Padding is trimmed.
        assert_eq!(drawer_snippet("   padded   "), "padded");

        // A body longer than the cap is truncated and ends with `…`.
        let long = "a".repeat(200);
        let snippet = drawer_snippet(&long);
        assert_eq!(snippet.chars().count(), DRAWER_SNIPPET_MAX_CHARS);
        assert!(
            snippet.ends_with('…'),
            "long body must be truncated with ellipsis",
        );

        // A body sized exactly at the cap is preserved verbatim.
        let exact = "a".repeat(DRAWER_SNIPPET_MAX_CHARS);
        assert_eq!(drawer_snippet(&exact), exact);
    }

    /// Why: empty / whitespace-only bodies must produce an empty
    /// snippet so the `list_drawers` shaper can omit the `snippet`
    /// field (rendered as `null` on the wire) instead of an empty
    /// string. The TUI relies on this distinction to skip the snippet
    /// column entirely when the body has no usable preview.
    /// What: feeds empty and whitespace-only strings.
    /// Test: itself.
    #[test]
    fn drawer_snippet_handles_empty_content() {
        assert_eq!(drawer_snippet(""), "");
        assert_eq!(drawer_snippet("   \n\t  "), "");
    }

    /// Issue #5384 regression guard — a failed count read degrades to `0`,
    /// and a successful one is passed through untouched.
    ///
    /// Why: #4637 fixed `0` as "unknown, never empty" for the status,
    /// console-metrics and palace-info totals, which is only safe while the
    /// failure stays loud. Before #5384 the store swallowed the error and
    /// returned `0` itself, so no caller could tell an empty graph from a
    /// broken read; the degrade now survives at exactly one call site, and
    /// this pins it there. Without this test the whole error arm of
    /// `kg_triple_count_or_zero` is unexecuted in this crate — the upstream
    /// test in trusty-common proves the error is raised, not that this
    /// converts it.
    /// What: feeds an `Err` and asserts `0`, then an `Ok(n)` and asserts `n`,
    /// so the degrade is provably scoped to the error arm rather than
    /// flattening real counts.
    /// Test: this test.
    #[test]
    fn triple_count_or_zero_degrades_a_failed_read_to_zero() {
        let palace = PalaceId::new("degrade-guard");

        assert_eq!(
            triple_count_or_zero(
                &palace,
                Err(anyhow!("active_subject_counts: no such table"))
            ),
            0,
            "an unreadable count degrades to 0 rather than propagating or panicking"
        );
        assert_eq!(
            triple_count_or_zero(&palace, Ok(7)),
            7,
            "a successful read is passed through untouched"
        );
    }

    /// Build an `AppState` over a temp dir whose registry holds at most two
    /// open handles, with palaces `a`, `b`, `c` created in that order.
    ///
    /// Why: a capacity-2 LRU plus three creations gives a deterministic
    /// cached/evicted split — `a` is guaranteed evicted, `b` and `c` are
    /// guaranteed resident — which is exactly the shape the #4637 regression
    /// guards need. Mirrors the fixture the #1924 guard uses in
    /// `console_metrics.rs`.
    /// What: returns the `AppState`; the temp dir is leaked so it outlives the
    /// test body (same convention as `test_state`).
    /// Test: used by `list_palaces_does_not_open_uncached_palaces`,
    /// `status_does_not_open_uncached_palaces`, and
    /// `open_palaces_blocking_opens_every_palace`.
    fn state_with_one_evicted_palace() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();
        std::mem::forget(tmp);

        let registry = trusty_common::memory_core::PalaceRegistry::with_max_open(2);
        for name in ["a", "b", "c"] {
            let palace = Palace {
                id: PalaceId::new(name),
                name: name.to_string(),
                description: None,
                created_at: Utc::now(),
                data_dir: data_root.join(name),
            };
            registry
                .create_palace(&data_root, palace)
                .unwrap_or_else(|e| panic!("create_palace({name}) failed: {e:#}"));
        }
        assert_eq!(registry.len(), 2, "capacity-2 registry holds 2 of 3");
        assert!(
            registry.peek(&PalaceId::new("a")).is_none(),
            "'a' must be evicted before the route under test runs"
        );

        let mut state = AppState::new(data_root);
        state.registry = Arc::new(registry);
        state
    }

    /// Issue #4637 regression guard — `GET /api/v1/palaces` must never
    /// force-open a palace that isn't already in the registry's LRU cache.
    ///
    /// Why: the previous implementation called `open_palace` per row. On the
    /// live daemon (5,794 palaces, 64-slot LRU) that is ~5,730 cold opens of
    /// ~1s each per request — the route was unusable, and it evicted the
    /// entire working set every time it ran. A test that only checked the row
    /// count would not have caught that, so this asserts on the *cache*.
    /// What: builds the capacity-2 / three-palace fixture, calls
    /// `MemoryService::list_palaces`, and asserts (1) all three palaces are
    /// listed, (2) the evicted one is flagged `cached: false` with zero counts
    /// while the resident ones are `cached: true`, and (3) the registry's
    /// membership and size are byte-for-byte unchanged — nothing was opened,
    /// nothing was evicted.
    /// Test: this test.
    #[tokio::test]
    async fn list_palaces_does_not_open_uncached_palaces() {
        let state = state_with_one_evicted_palace();
        let svc = MemoryService::new(state.clone());

        let rows = svc.list_palaces().await.expect("list_palaces");

        assert_eq!(rows.len(), 3, "every on-disk palace is still listed");
        let row = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("row for '{id}' present"))
        };
        assert!(!row("a").cached, "'a' was evicted, so it is not cached");
        assert_eq!(
            row("a").drawer_count,
            0,
            "an uncached row reports 0 (unknown), not a live count"
        );
        assert!(row("b").cached, "'b' is resident");
        assert!(row("c").cached, "'c' is resident");

        assert_eq!(
            state.registry.len(),
            2,
            "list_palaces must not grow the LRU cache"
        );
        assert!(
            state.registry.peek(&PalaceId::new("a")).is_none(),
            "list_palaces must not reopen the evicted palace 'a'"
        );
        assert!(state.registry.peek(&PalaceId::new("b")).is_some());
        assert!(state.registry.peek(&PalaceId::new("c")).is_some());
    }

    /// Issue #4637 regression guard — `GET /api/v1/status` must never
    /// force-open a palace that isn't already cached.
    ///
    /// Why: `status()` fed every on-disk palace id through
    /// `collect_palace_stats`, which called `open_palace` per id. Measured
    /// live, the endpoint did not respond within 90s while `/health` returned
    /// in 36ms. The totals are now a cache-resident roll-up, which is only
    /// honest if `cached_palace_count` says so — this pins both halves.
    /// What: builds the capacity-2 / three-palace fixture, calls
    /// `MemoryService::status`, and asserts `palace_count` still reports all
    /// three on-disk palaces while `cached_palace_count` reports only the two
    /// resident ones, and that the cache is untouched afterwards.
    /// Test: this test.
    #[tokio::test]
    async fn status_does_not_open_uncached_palaces() {
        let state = state_with_one_evicted_palace();
        let svc = MemoryService::new(state.clone());

        let payload = svc.status().await;

        assert_eq!(
            payload.palace_count, 3,
            "palace_count still reflects every palace on disk"
        );
        assert_eq!(
            payload.cached_palace_count, 2,
            "the totals cover only the 2 cache-resident palaces"
        );

        assert_eq!(
            state.registry.len(),
            2,
            "status must not grow the LRU cache"
        );
        assert!(
            state.registry.peek(&PalaceId::new("a")).is_none(),
            "status must not reopen the evicted palace 'a'"
        );
    }

    /// Issue #4637 — the recall fan-out deliberately still opens every palace.
    ///
    /// Why: this is the other half of the fix and the more important half to
    /// pin. `peek()` is the right tool for stat/list routes and the WRONG tool
    /// here — a cross-palace recall answered from cache-resident palaces only
    /// would silently omit ~98.9% of the corpus. If someone later "optimises"
    /// `open_palaces_blocking` into a `peek`, this test fails.
    /// What: builds the capacity-2 / three-palace fixture (so `a` is evicted)
    /// and asserts `open_palaces_blocking` still returns three handles,
    /// including one for the palace that was not cached.
    /// Test: this test.
    #[tokio::test]
    async fn open_palaces_blocking_opens_every_palace() {
        let state = state_with_one_evicted_palace();
        let palaces = list_palaces_blocking(&state).await.expect("list palaces");
        assert_eq!(palaces.len(), 3);

        let handles = open_palaces_blocking(&state, &palaces, "test").await;

        assert_eq!(
            handles.len(),
            3,
            "recall fan-out must open every palace, including uncached ones"
        );
        assert!(
            handles.iter().any(|h| h.id == PalaceId::new("a")),
            "the evicted palace 'a' must still be opened and searched"
        );
    }
}
