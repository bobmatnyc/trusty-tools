//! Free helper functions + user-config loading for the trusty-memory service
//! layer.
//!
//! Why: thin no-IO transforms (preview/snippet/recall-entry JSON), palace-stat
//! aggregation, palace-info enrichment, gaps-cache refresh, and user-config
//! loading are shared by the HTTP handlers, the chat dispatcher, and the
//! `MemoryService` core (split out of the former monolithic `service.rs`,
//! issue #607).
//! What: the free helpers + `LoadedUserConfig`/`load_user_config` +
//! `service_result_to_anyhow`, moved verbatim. The service-layer unit tests
//! live here too (1500-SLOC test cap applies).
//! Test: `drawer_*`, `recall_entry_*`, and `list_drawers_*` in `service::tests`.

use crate::AppState;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
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
/// and `kg.count_active_triples()` across the supplied palace ids.
/// Test: indirectly via `status_endpoint_returns_payload` and any SSE test
/// that observes `StatusChanged`.
pub(crate) struct PalaceStats {
    pub total_drawers: usize,
    pub total_vectors: usize,
    pub total_kg_triples: usize,
}

/// Sum drawer / vector / KG-triple counts across `ids`, skipping palaces that
/// cannot be opened.
///
/// Why (issue #228): centralises the previously-duplicated loop from
/// `status()` and `aggregate_status_event()`. Callers pass an iterator of
/// `PalaceId` so the helper works for both the on-disk view (used by
/// `status()`) and the in-memory registry view (used by
/// `aggregate_status_event()` on the SSE hot path).
/// What: for each id, calls `registry.open_palace` (cheap when the handle is
/// already cached, slow only on first-ever open) and accumulates the three
/// counts via `saturating_add` so overflow is impossible. Palaces that fail
/// to open are silently skipped — one bad palace must not blank the
/// dashboard.
/// Test: indirectly through `status_endpoint_returns_payload`.
pub(crate) fn collect_palace_stats<'a, I>(state: &AppState, ids: I) -> PalaceStats
where
    I: IntoIterator<Item = &'a PalaceId>,
{
    let (mut total_drawers, mut total_vectors, mut total_kg_triples): (usize, usize, usize) =
        (0, 0, 0);
    for id in ids {
        if let Ok(handle) = state.registry.open_palace(&state.data_root, id) {
            total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
            total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
            total_kg_triples = total_kg_triples.saturating_add(handle.kg.count_active_triples());
        }
    }
    PalaceStats {
        total_drawers,
        total_vectors,
        total_kg_triples,
    }
}

/// Build a `PalaceInfo` from a `Palace` row plus an optional opened handle.
///
/// Why: both `list_palaces` and `get_palace` need the same enriched shape;
/// the helper avoids field-set drift between them.
/// What: reads drawer/vector/triple counts, distinct rooms, max
/// `created_at`, KG node/edge/community counts, and the `is_compacting` flag.
/// Test: `palace_list_includes_richer_counts`, `palace_list_includes_graph_counts`.
pub fn palace_info_from(palace: &Palace, handle: Option<&Arc<PalaceHandle>>) -> PalaceInfo {
    let (
        drawer_count,
        vector_count,
        kg_triple_count,
        wing_count,
        last_write_at,
        node_count,
        edge_count,
        community_count,
        is_compacting,
    ) = if let Some(h) = handle {
        let drawers = h.drawers.read();
        let distinct_rooms: HashSet<Uuid> = drawers.iter().map(|d| d.room_id).collect();
        let last_write = drawers.iter().map(|d| d.created_at).max();
        (
            drawers.len(),
            h.vector_store.index_size(),
            h.kg.count_active_triples(),
            distinct_rooms.len(),
            last_write,
            h.kg.node_count() as u64,
            h.kg.edge_count() as u64,
            h.kg.community_count() as u64,
            h.is_compacting(),
        )
    } else {
        (0, 0, 0, 0, None, 0, 0, 0, false)
    };
    PalaceInfo {
        id: palace.id.0.clone(),
        name: palace.name.clone(),
        description: palace.description.clone(),
        drawer_count,
        vector_count,
        kg_triple_count,
        wing_count,
        created_at: palace.created_at,
        last_write_at,
        node_count,
        edge_count,
        community_count,
        is_compacting,
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
    if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
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
// User config — moved from `web.rs` so chat and HTTP both load it cheaply.
// ---------------------------------------------------------------------------

/// Minimal mirror of the user-config schema.
#[derive(Deserialize, Default, Clone)]
struct UserConfigMin {
    #[serde(default)]
    openrouter: OpenRouterMin,
    #[serde(default)]
    local_model: LocalModelMin,
}

#[derive(Deserialize, Default, Clone)]
struct OpenRouterMin {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize, Clone)]
struct LocalModelMin {
    #[serde(default = "default_local_enabled")]
    enabled: bool,
    #[serde(default = "default_local_base_url")]
    base_url: String,
    #[serde(default = "default_local_model")]
    model: String,
}

fn default_local_enabled() -> bool {
    true
}
fn default_local_base_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_local_model() -> String {
    "llama3.2".to_string()
}

impl Default for LocalModelMin {
    fn default() -> Self {
        Self {
            enabled: default_local_enabled(),
            base_url: default_local_base_url(),
            model: default_local_model(),
        }
    }
}

/// Loaded user config (mirrors the public `LoadedUserConfig` from `web.rs`).
#[derive(Clone)]
pub struct LoadedUserConfig {
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    pub local_model: trusty_common::LocalModelConfig,
}

impl Default for LoadedUserConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig::default(),
        }
    }
}

/// Read the user's `~/.trusty-memory/config.toml`, falling back to defaults.
///
/// Why: shared between HTTP config endpoint, chat tool dispatch, and
/// provider auto-detection.
/// What: returns `Some(LoadedUserConfig)` even when the file is missing
/// (so callers see defaults consistently); `None` only when the home
/// directory itself can't be resolved.
/// Test: indirectly via `config_endpoint_returns_payload`.
pub fn load_user_config() -> Option<LoadedUserConfig> {
    let home = dirs::home_dir()?;
    let path = home.join(".trusty-memory").join("config.toml");
    if !path.exists() {
        return Some(LoadedUserConfig::default());
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: UserConfigMin = toml::from_str(&raw).unwrap_or_default();
    let model = if parsed.openrouter.model.is_empty() {
        "anthropic/claude-3-5-sonnet".to_string()
    } else {
        parsed.openrouter.model
    };
    Some(LoadedUserConfig {
        openrouter_api_key: parsed.openrouter.api_key,
        openrouter_model: model,
        local_model: trusty_common::LocalModelConfig {
            enabled: parsed.local_model.enabled,
            base_url: parsed.local_model.base_url,
            model: parsed.local_model.model,
        },
    })
}

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
            let drawer = Drawer {
                id: Uuid::new_v4(),
                room_id,
                content: format!("drawer-{i}"),
                importance: *importance,
                source_file: None,
                created_at: now - ChronoDuration::seconds(i as i64),
                tags: vec![format!("idx:{i}")],
                last_accessed_at: None,
                access_count: 0,
                drawer_type: Default::default(),
                expires_at: None,
            };
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
}
