//! `MemoryService` — pure business-logic facade over `AppState`.
//!
//! Why: `web.rs` previously hosted ~5700 lines that mingled axum extraction,
//! JSON wire shapes, and business logic. Moving the logic into a struct with
//! `anyhow::Result<Value>` methods lets the HTTP handlers stay one-liners
//! and lets non-HTTP callers (chat tool dispatch, future RPC bridges) reuse
//! the same code paths without dragging axum types around.
//! What: A zero-cost wrapper around [`AppState`] exposing one async method
//! per logical operation. Each method returns either `anyhow::Result<Value>`
//! (for handlers that already wrap errors with `ApiError::internal`) or a
//! domain-specific result the handler maps into JSON.
//! Test: Each method is covered indirectly via the corresponding HTTP test in
//! `web::tests` (the handlers delegate here verbatim).
//!
//! Hard constraint (issue #151): no behaviour change. Every method's success
//! and failure shapes match what the handler used to produce inline.

use crate::attribution::CreatorInfo;
use crate::{ActivityFilter, ActivitySource, AppState, DaemonEvent};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use trusty_common::memory_core::dream::{DreamConfig, Dreamer, PersistedDreamStats};
use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall_across_palaces_with_default_embedder, recall_deep_with_default_embedder,
    recall_with_default_embedder, RecallResult,
};
use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Wire types shared between HTTP handlers and the service layer.
// ---------------------------------------------------------------------------

/// Serializable palace summary used by `GET /api/v1/palaces` and
/// `GET /api/v1/palaces/{id}`.
///
/// Why: Both endpoints return the same enriched shape; centralising the
/// type in the service layer keeps the wire contract single-source.
/// What: Mirrors the legacy `PalaceInfo` struct verbatim — counts, timestamps,
/// graph stats, and the `is_compacting` flag.
/// Test: `palace_list_includes_richer_counts`, `palace_list_includes_graph_counts`.
#[derive(Serialize, Clone, Debug)]
pub struct PalaceInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub drawer_count: usize,
    pub vector_count: usize,
    pub kg_triple_count: usize,
    pub wing_count: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_write_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub node_count: u64,
    #[serde(default)]
    pub edge_count: u64,
    #[serde(default)]
    pub community_count: u64,
    #[serde(default)]
    pub is_compacting: bool,
}

/// Dream statistics wire shape used by both per-palace and aggregate endpoints.
///
/// Why: Lifted out of `web.rs` so the service layer owns the type the chat
/// dispatcher and HTTP handlers both serialise. Stays identical to the
/// pre-refactor shape.
/// What: All fields are saturating sums across one or more palaces; the
/// `last_run_at` is the max across them (or `None` when no palace has run).
/// Test: `dream_status_aggregates_across_palaces`, `dream_run_aggregates_stats`.
#[derive(Serialize, Default, Clone, Debug)]
pub struct DreamStatusPayload {
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub merged: usize,
    pub pruned: usize,
    pub compacted: usize,
    pub closets_updated: usize,
    pub duration_ms: u64,
}

impl From<PersistedDreamStats> for DreamStatusPayload {
    fn from(p: PersistedDreamStats) -> Self {
        Self {
            last_run_at: Some(p.last_run_at),
            merged: p.stats.merged,
            pruned: p.stats.pruned,
            compacted: p.stats.compacted,
            closets_updated: p.stats.closets_updated,
            duration_ms: p.stats.duration_ms,
        }
    }
}

/// `POST /api/v1/palaces` body — service-facing version.
#[derive(Deserialize, Clone, Debug)]
pub struct CreatePalaceBody {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `POST /api/v1/palaces/{id}/drawers` body — service-facing version.
#[derive(Deserialize, Clone, Debug)]
pub struct CreateDrawerBody {
    pub content: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub importance: Option<f32>,
}

/// `GET /api/v1/palaces/{id}/drawers` query — service-facing version.
///
/// Why: the TUI activity panel (#184) needs paged access to a palace's
/// drawers in newest-first order. Adding `offset` and `sort` to the existing
/// query struct keeps the surface compatible (both fields default to absent)
/// while letting the panel walk through arbitrarily many drawers.
/// What: optional `room` / `tag` filters, a `limit` (default 50 in the
/// handler), an `offset` for pagination, and a `sort` selector — `importance`
/// (the legacy default, descending) or `created_desc` (newest first).
/// Test: `list_drawers_creates_desc_paginates` in `service::tests`.
#[derive(Deserialize, Default, Clone, Debug)]
pub struct ListDrawersQuery {
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of drawers to skip before returning results. Combined with
    /// `limit` this paginates the result set. Defaults to 0.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Sort selector: `"importance"` (default — importance descending,
    /// preserving legacy behaviour) or `"created_desc"` (creation date
    /// descending, newest first — used by the TUI activity panel).
    #[serde(default)]
    pub sort: Option<String>,
}

/// `POST /api/v1/palaces/{id}/kg` body — service-facing version.
#[derive(Deserialize, Clone, Debug)]
pub struct KgAssertBody {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub provenance: Option<String>,
}

/// Knowledge-graph "graph payload" used by `GET /api/v1/palaces/{id}/kg/graph`.
#[derive(Serialize, Clone, Debug)]
pub struct KgGraphPayload {
    pub triples: Vec<Triple>,
    pub node_count: u64,
    pub edge_count: u64,
    pub community_count: u64,
}

/// Status payload returned by `GET /api/v1/status`.
#[derive(Serialize, Clone, Debug)]
pub struct StatusPayload {
    pub version: String,
    pub palace_count: usize,
    pub default_palace: Option<String>,
    pub data_root: String,
    pub total_drawers: usize,
    pub total_vectors: usize,
    pub total_kg_triples: usize,
}

/// Service-level error type that maps cleanly onto HTTP status codes.
///
/// Why: handlers want to render 400/404/409/500 from a single point; the
/// service methods produce a typed error so the binding layer can pick the
/// right status without parsing strings.
/// What: four variants matching the legacy `ApiError` constructors plus a
/// dedicated `Conflict` for state-clash errors (issue #180: deleting a
/// non-empty palace without `force`).
/// Test: indirectly via the HTTP tests for the corresponding endpoints.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

impl ServiceError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
    /// Build a 409 Conflict service error.
    ///
    /// Why: palace-delete (issue #180) needs to surface a distinct
    /// "state precondition failed" status when the caller asks to delete a
    /// non-empty palace without `force=true`. 400 would be misleading
    /// (the request itself is well-formed) and 404 would lie about the
    /// resource's existence.
    /// What: wraps the message in `ServiceError::Conflict`.
    /// Test: `delete_palace_refuses_when_drawers_present` in `web::tests`.
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

/// Result alias used across the service layer.
pub type ServiceResult<T> = std::result::Result<T, ServiceError>;

/// Hard cap on triples returned by the per-palace graph endpoint.
const KG_GRAPH_MAX_TRIPLES: usize = 5_000;

// ---------------------------------------------------------------------------
// MemoryService — pure business logic facade.
// ---------------------------------------------------------------------------

/// Wraps [`AppState`] and exposes one async method per logical operation.
///
/// Why: see module docs. Lets HTTP handlers stay thin and lets non-HTTP
/// callers (chat tool dispatch, RPC bridges) reuse the same code paths.
/// What: `Clone` (cheap — only the inner `AppState` is shared); construct
/// with `MemoryService::new(state)`.
/// Test: every method is covered by the corresponding handler test in
/// `web::tests`.
#[derive(Clone)]
pub struct MemoryService {
    state: AppState,
}

impl MemoryService {
    /// Construct a new service wrapper.
    ///
    /// Why: handlers cheaply re-wrap their `AppState` on every request; the
    /// cost is just an `Arc` clone, so we don't bother caching the wrapper.
    /// What: stores the `AppState` for later method calls.
    /// Test: trivial — covered indirectly by every handler test.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Borrow the inner [`AppState`].
    ///
    /// Why: some handlers still need direct access (SSE broadcaster, session
    /// store, etc.) while we incrementally extract code into the service.
    /// What: returns a borrowed reference to the wrapped `AppState`.
    /// Test: not directly tested; surface-level accessor.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    // -----------------------------------------------------------------
    // Status / config
    // -----------------------------------------------------------------

    /// Build the aggregate `/api/v1/status` payload.
    ///
    /// Why: dashboard widgets and the MCP `get_status` tool need the same
    /// roll-up; centralising avoids drift between the two surfaces.
    /// What: walks every persisted palace, sums drawer/vector/triple counts,
    /// and returns the [`StatusPayload`].
    /// Test: `status_endpoint_returns_payload`.
    pub async fn status(&self) -> StatusPayload {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root).unwrap_or_default();
        let palace_count = palaces.len();
        let (mut total_drawers, mut total_vectors, mut total_kg_triples): (usize, usize, usize) =
            (0, 0, 0);
        for p in &palaces {
            if let Ok(handle) = self
                .state
                .registry
                .open_palace(&self.state.data_root, &p.id)
            {
                total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
                total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
                total_kg_triples =
                    total_kg_triples.saturating_add(handle.kg.count_active_triples());
            }
        }
        StatusPayload {
            version: self.state.version.clone(),
            palace_count,
            default_palace: self.state.default_palace.clone(),
            data_root: self.state.data_root.display().to_string(),
            total_drawers,
            total_vectors,
            total_kg_triples,
        }
    }

    /// Compute the aggregate `StatusChanged` event used by SSE consumers.
    ///
    /// Why: mutating handlers push a refreshed status snapshot so dashboards
    /// stay in sync without an extra `/api/v1/status` request.
    /// What: same math as `status()` but returns a `DaemonEvent::StatusChanged`.
    /// Test: indirectly via SSE integration tests.
    pub fn aggregate_status_event(&self) -> DaemonEvent {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root).unwrap_or_default();
        let (mut total_drawers, mut total_vectors, mut total_kg_triples): (usize, usize, usize) =
            (0, 0, 0);
        for p in &palaces {
            if let Ok(handle) = self
                .state
                .registry
                .open_palace(&self.state.data_root, &p.id)
            {
                total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
                total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
                total_kg_triples =
                    total_kg_triples.saturating_add(handle.kg.count_active_triples());
            }
        }
        DaemonEvent::StatusChanged {
            total_drawers,
            total_vectors,
            total_kg_triples,
        }
    }

    // -----------------------------------------------------------------
    // Palaces
    // -----------------------------------------------------------------

    /// List every palace on disk, enriched with live handle stats.
    ///
    /// Why: shared between the HTTP handler and the chat tool dispatcher;
    /// both want the same `PalaceInfo` shape. Issue #185 added the
    /// reserved-prefix filter so internal "system" palaces (e.g. the
    /// `__health_probe__` palace used by `/health`) never surface in the
    /// admin UI, TUI, or any user-facing roster.
    /// What: walks the registry, drops any palace whose id starts with the
    /// reserved `__` prefix, and builds a `PalaceInfo` per remaining row.
    /// Test: `palace_list_includes_richer_counts`, `palace_list_includes_graph_counts`,
    /// `health_probe_palace_is_invisible` (in `web::tests`).
    pub async fn list_palaces(&self) -> ServiceResult<Vec<PalaceInfo>> {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root)
            .map_err(|e| ServiceError::internal(format!("list palaces: {e:#}")))?;
        let mut out = Vec::with_capacity(palaces.len());
        for p in palaces {
            if is_reserved_system_palace(&p.id) {
                continue;
            }
            let handle = self
                .state
                .registry
                .open_palace(&self.state.data_root, &p.id)
                .ok();
            out.push(palace_info_from(&p, handle.as_ref()));
        }
        Ok(out)
    }

    /// Create a new palace and emit the corresponding activity event.
    ///
    /// Why: trims duplicated work between the HTTP handler and any future
    /// non-HTTP creation flow.
    /// What: validates the name, builds the `Palace` row, calls
    /// `PalaceRegistry::create_palace`, and emits `PalaceCreated`. Returns
    /// the new palace id.
    /// Test: covered indirectly by `palace_list_includes_richer_counts` (which
    /// posts a palace through the HTTP layer then reads it back).
    pub async fn create_palace(
        &self,
        body: CreatePalaceBody,
        source: ActivitySource,
    ) -> ServiceResult<String> {
        let name = body.name.trim().to_string();
        if name.is_empty() {
            return Err(ServiceError::bad_request("name is required"));
        }
        let id = PalaceId::new(&name);
        let palace = Palace {
            id: id.clone(),
            name: name.clone(),
            description: body.description.filter(|s| !s.is_empty()),
            created_at: chrono::Utc::now(),
            data_dir: self.state.data_root.join(&name),
        };
        self.state
            .registry
            .create_palace(&self.state.data_root, palace)
            .map_err(|e| ServiceError::internal(format!("create palace: {e:#}")))?;
        self.state.emit(DaemonEvent::PalaceCreated {
            id: name.clone(),
            name: name.clone(),
            source,
        });
        Ok(name)
    }

    /// Delete a palace from disk, optionally rejecting non-empty palaces.
    ///
    /// Why: Issue #180 — operators need a way to drop an entire palace
    /// without going through drawer-by-drawer deletion. Defaulting to a
    /// "must be empty" guard prevents fat-finger destruction of populated
    /// palaces; `force=true` is the explicit opt-in to the destructive path.
    /// What: 1) confirms the palace exists on disk (else `NotFound`),
    /// 2) when `!force`, lists drawers via the live handle and returns
    /// `BadRequest("Palace has drawers; pass force=true to delete")` if
    /// the palace is non-empty, 3) drops the in-memory registry entry so
    /// future opens hit the (now-missing) disk state, 4) removes
    /// `<data_root>/<palace_id>/` recursively via `tokio::fs::remove_dir_all`,
    /// and 5) emits an aggregate `StatusChanged` so dashboards refresh.
    /// Test: `delete_palace_removes_dir_when_empty`,
    /// `delete_palace_refuses_when_drawers_present`,
    /// `delete_palace_force_removes_populated_palace`,
    /// `delete_palace_returns_not_found_for_missing_id` in `web::tests`.
    pub async fn delete_palace(&self, palace_id: &str, force: bool) -> ServiceResult<()> {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root)
            .map_err(|e| ServiceError::internal(format!("list palaces: {e:#}")))?;
        if !palaces.iter().any(|p| p.id.0 == palace_id) {
            return Err(ServiceError::not_found(format!(
                "palace not found: {palace_id}"
            )));
        }
        if !force {
            // Open the palace just long enough to count its drawers; we don't
            // hold the handle past this check because the caller is about to
            // delete the on-disk directory.
            if let Ok(handle) = self
                .state
                .registry
                .open_palace(&self.state.data_root, &PalaceId::new(palace_id))
            {
                if !handle.drawers.read().is_empty() {
                    return Err(ServiceError::conflict(
                        "Palace has drawers; pass force=true to delete",
                    ));
                }
            }
        }
        // Drop the cached `Arc<PalaceHandle>` and gap cache before unlinking
        // the directory so subsequent reads can't be served from the stale
        // in-memory state. The registry's `remove` is a no-op when the entry
        // is absent (lazy-open palaces that no caller has touched yet).
        self.state.registry.remove(&PalaceId::new(palace_id));
        let palace_dir = self.state.data_root.join(palace_id);
        tokio::fs::remove_dir_all(&palace_dir).await.map_err(|e| {
            ServiceError::internal(format!("remove palace dir {}: {e}", palace_dir.display()))
        })?;
        // Recompute aggregate totals so dashboards drop the deleted palace's
        // counts. There's no dedicated `PalaceDeleted` event variant yet;
        // `StatusChanged` is enough to keep the UI in sync.
        self.state.emit(self.aggregate_status_event());
        Ok(())
    }

    /// Rename a palace's display name without touching its data.
    ///
    /// Why: Operators need to fix typos and rebrand palaces without dropping
    /// the underlying drawers / vectors / KG. The palace id (the directory
    /// name on disk) is immutable — only the human-readable `name` field in
    /// `palace.json` changes — so cached `PalaceHandle`s stay valid and no
    /// registry invalidation is required.
    /// What: 1) loads the palace via `PalaceStore::load_palace` (404 when the
    /// directory or `palace.json` is missing), 2) trims the new name and
    /// returns `BadRequest` when empty, 3) mutates `palace.name` and writes
    /// the metadata back through the atomic `PalaceStore::save_palace`
    /// (tmp file + rename), 4) emits an aggregate `StatusChanged` so
    /// dashboards re-render the relabelled palace, 5) returns the updated
    /// palace as JSON (enriched with the live handle stats, so callers see
    /// drawer/vector/KG counts in the same shape as `GET /palaces/{id}`).
    /// Test: `update_palace_name_renames_palace`,
    /// `update_palace_name_rejects_empty_name`,
    /// `update_palace_name_returns_not_found_for_missing_id` in `web::tests`.
    pub async fn update_palace_name(&self, palace_id: &str, name: &str) -> Result<Value> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("name must be non-empty after trimming"));
        }
        let palace_dir = self.state.data_root.join(palace_id);
        let mut palace = trusty_common::memory_core::store::PalaceStore::load_palace(&palace_dir)
            .map_err(|e| anyhow!("palace not found: {palace_id} ({e})"))?;
        palace.name = trimmed.to_string();
        trusty_common::memory_core::store::PalaceStore::save_palace(&palace)
            .with_context(|| format!("save palace metadata for {palace_id}"))?;
        let handle = self
            .state
            .registry
            .open_palace(&self.state.data_root, &palace.id)
            .ok();
        let info = palace_info_from(&palace, handle.as_ref());
        self.state.emit(self.aggregate_status_event());
        serde_json::to_value(info).context("serialize palace info")
    }

    /// Typed variant of [`Self::update_palace_name`] used by the HTTP handler.
    ///
    /// Why: HTTP needs to distinguish 400 (empty name) from 404 (missing
    /// palace) so the right status code is emitted; the chat / MCP tool
    /// only cares about a `Result<Value>` because both errors are surfaced
    /// as opaque MCP error strings. Keeping a typed variant alongside the
    /// untyped one keeps the wire shape correct on both surfaces without
    /// asking either caller to parse error strings.
    /// What: same as [`Self::update_palace_name`] but returns
    /// `ServiceError::BadRequest` for empty names and
    /// `ServiceError::NotFound` for missing palace metadata.
    /// Test: `update_palace_name_renames_palace`,
    /// `update_palace_name_rejects_empty_name`,
    /// `update_palace_name_returns_not_found_for_missing_id`.
    pub async fn update_palace_name_typed(
        &self,
        palace_id: &str,
        name: &str,
    ) -> ServiceResult<Value> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(ServiceError::bad_request(
                "name must be non-empty after trimming",
            ));
        }
        let palace_dir = self.state.data_root.join(palace_id);
        let mut palace = trusty_common::memory_core::store::PalaceStore::load_palace(&palace_dir)
            .map_err(|e| {
            ServiceError::not_found(format!("palace not found: {palace_id} ({e})"))
        })?;
        palace.name = trimmed.to_string();
        trusty_common::memory_core::store::PalaceStore::save_palace(&palace).map_err(|e| {
            ServiceError::internal(format!("save palace metadata for {palace_id}: {e}"))
        })?;
        let handle = self
            .state
            .registry
            .open_palace(&self.state.data_root, &palace.id)
            .ok();
        let info = palace_info_from(&palace, handle.as_ref());
        self.state.emit(self.aggregate_status_event());
        serde_json::to_value(info)
            .map_err(|e| ServiceError::internal(format!("serialize palace info: {e}")))
    }

    /// Look up a single palace by id and enrich with live handle stats.
    ///
    /// Why: distinct 404 vs. 500 path is needed by both HTTP and chat callers.
    /// What: returns `NotFound` when the id is unknown, otherwise a fully
    /// populated `PalaceInfo`.
    /// Test: indirectly via `health_endpoint_round_trip_with_palace_is_ok`.
    pub async fn get_palace(&self, id: &str) -> ServiceResult<PalaceInfo> {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root)
            .map_err(|e| ServiceError::internal(format!("list palaces: {e:#}")))?;
        let palace = palaces
            .into_iter()
            .find(|p| p.id.0 == id)
            .ok_or_else(|| ServiceError::not_found(format!("palace not found: {id}")))?;
        let handle = self
            .state
            .registry
            .open_palace(&self.state.data_root, &palace.id)
            .ok();
        Ok(palace_info_from(&palace, handle.as_ref()))
    }

    // -----------------------------------------------------------------
    // Drawers
    // -----------------------------------------------------------------

    /// List drawers in a palace with optional room/tag filters and pagination.
    ///
    /// Why: deduplicates the open-handle + listing path between HTTP and chat,
    /// and (issue #184) lets the TUI activity panel page through drawers in
    /// creation-date order without breaking the importance-sorted default the
    /// legacy callers rely on.
    /// What: opens the palace handle, fetches a window of drawers, optionally
    /// re-sorts by `created_at` descending when `sort = "created_desc"`
    /// (leaving the importance-desc default untouched), then drops the
    /// leading `offset` rows and keeps `limit`. For `created_desc` the
    /// window must cover the full filtered set (otherwise the importance
    /// pre-sort hides truly-recent low-importance drawers), so the window
    /// is widened to a sane ceiling (`MAX_DRAWER_WINDOW`); the default
    /// importance path keeps a tight `limit+offset` window.
    /// Returns the serialised JSON array.
    /// Test: `service::tests::list_drawers_creates_desc_paginates`.
    pub async fn list_drawers(&self, id: &str, q: ListDrawersQuery) -> ServiceResult<Value> {
        const MAX_DRAWER_WINDOW: usize = 10_000;
        let handle = self.open_handle(id)?;
        let room = q.room.as_deref().map(RoomType::parse);
        let limit = q.limit.unwrap_or(50);
        let offset = q.offset.unwrap_or(0);
        let by_created = matches!(q.sort.as_deref(), Some("created_desc"));
        // For created_desc the importance pre-sort would hide low-importance
        // drawers that happen to be the most recent, so we need to fetch the
        // full filtered set (capped at MAX_DRAWER_WINDOW). For importance
        // ordering the legacy `limit + offset` window is sufficient.
        let window = if by_created {
            MAX_DRAWER_WINDOW
        } else {
            limit.saturating_add(offset).min(MAX_DRAWER_WINDOW)
        };
        let mut drawers = handle.list_drawers(room, q.tag.clone(), window);
        if by_created {
            drawers.sort_by_key(|d| std::cmp::Reverse(d.created_at));
        }
        let page: Vec<_> = drawers.into_iter().skip(offset).take(limit).collect();
        // Issue #202: enrich every row with a short `snippet` derived from
        // the drawer's content so the TUI activity panel can render a
        // glanceable summary without re-parsing the full body. The
        // snippet is whitespace-collapsed and bounded at
        // `DRAWER_SNIPPET_MAX_CHARS` (60) — shorter than the SSE preview
        // because the activity panel renders it on a single narrow row.
        let payload: Vec<Value> = page
            .into_iter()
            .map(|drawer| {
                let snippet = drawer_snippet(&drawer.content);
                let mut value = serde_json::to_value(&drawer).unwrap_or_else(|_| json!({}));
                if let Value::Object(ref mut map) = value {
                    // `null` when the drawer has no usable content so
                    // clients can distinguish "no body" from "empty body
                    // after whitespace collapse".
                    let snippet_value = if snippet.is_empty() {
                        Value::Null
                    } else {
                        Value::String(snippet)
                    };
                    map.insert("snippet".to_string(), snippet_value);
                }
                value
            })
            .collect();
        Ok(Value::Array(payload))
    }

    /// Store a new drawer and emit the matching activity events.
    ///
    /// Why: HTTP and chat both need the auto-KG-extraction follow-up; this
    /// method keeps that side-effect chain in one place.
    /// What: opens the palace, stores the drawer via `PalaceHandle::remember`,
    /// emits `DrawerAdded` + `StatusChanged`, then triggers
    /// `tools::auto_extract_and_assert`. Returns the new drawer id.
    /// Test: `http_create_drawer_runs_auto_kg_extraction`.
    pub async fn create_drawer(
        &self,
        id: &str,
        body: CreateDrawerBody,
        creator: CreatorInfo,
        source: ActivitySource,
    ) -> ServiceResult<Uuid> {
        let handle = self.open_handle(id)?;
        let room = body
            .room
            .as_deref()
            .map(RoomType::parse)
            .unwrap_or(RoomType::General);
        let importance = body.importance.unwrap_or(0.5);
        let content_preview = drawer_content_preview(&body.content);
        let mut tags_with_creator = body.tags;
        // Issue #202: project a bare-UUID session tag (when the caller
        // passed one in the request body) into the reserved
        // `creator:session=<first-8>` slot so the activity panel can
        // surface session attribution without bespoke parsing.
        if let Some(session_tag) = crate::attribution::session_tag_from_tags(&tags_with_creator) {
            tags_with_creator.push(session_tag);
        }
        creator.merge_into(&mut tags_with_creator);
        let content_for_kg = body.content.clone();
        let tags_for_kg = tags_with_creator.clone();
        let room_label_for_kg = crate::tools::room_label(&room);
        let drawer_id = handle
            .remember(body.content, room, tags_with_creator, importance)
            .await
            .map_err(|e| ServiceError::internal(format!("remember: {e:#}")))?;
        let drawer_count = handle.drawers.read().len();
        let palace_name = PalaceRegistry::list_palaces(&self.state.data_root)
            .ok()
            .and_then(|ps| ps.into_iter().find(|p| p.id.0 == id).map(|p| p.name))
            .unwrap_or_else(|| id.to_string());
        self.state.emit(DaemonEvent::DrawerAdded {
            palace_id: id.to_string(),
            palace_name,
            drawer_count,
            timestamp: chrono::Utc::now(),
            content_preview,
            source,
        });
        self.state.emit(self.aggregate_status_event());
        crate::tools::auto_extract_and_assert(
            &handle,
            drawer_id,
            &content_for_kg,
            &tags_for_kg,
            room_label_for_kg.as_deref(),
        )
        .await;
        Ok(drawer_id)
    }

    /// Forget (delete) a drawer and emit the matching events.
    ///
    /// Why: same dedup story as `create_drawer`.
    /// What: parses the drawer UUID, calls `PalaceHandle::forget`, emits
    /// `DrawerDeleted` + `StatusChanged`.
    /// Test: indirectly via the drawer-related HTTP tests.
    pub async fn delete_drawer(
        &self,
        id: &str,
        drawer_id: &str,
        source: ActivitySource,
    ) -> ServiceResult<()> {
        let handle = self.open_handle(id)?;
        let uuid = Uuid::parse_str(drawer_id)
            .map_err(|_| ServiceError::bad_request("drawer_id must be a UUID"))?;
        handle
            .forget(uuid)
            .await
            .map_err(|e| ServiceError::internal(format!("forget: {e:#}")))?;
        let drawer_count = handle.drawers.read().len();
        self.state.emit(DaemonEvent::DrawerDeleted {
            palace_id: id.to_string(),
            drawer_count,
            source,
        });
        self.state.emit(self.aggregate_status_event());
        Ok(())
    }

    // -----------------------------------------------------------------
    // Recall
    // -----------------------------------------------------------------

    /// Per-palace recall (semantic search), optionally with deep retrieval.
    ///
    /// Why: HTTP and chat tools both perform the same fan-out logic.
    /// What: opens the palace handle and dispatches to the shallow or deep
    /// recall helper. Returns a JSON array of flattened drawer rows (the
    /// `recall_entry_json` shape from issue #69).
    /// Test: `recall_entry_json_hoists_drawer_fields`.
    pub async fn recall(
        &self,
        id: &str,
        query: &str,
        top_k: usize,
        deep: bool,
    ) -> ServiceResult<Value> {
        let handle = self.open_handle(id)?;
        let results = if deep {
            recall_deep_with_default_embedder(&handle, query, top_k).await
        } else {
            recall_with_default_embedder(&handle, query, top_k).await
        }
        .map_err(|e| ServiceError::internal(format!("recall: {e:#}")))?;
        let payload: Vec<Value> = results.into_iter().map(recall_entry_json).collect();
        Ok(json!(payload))
    }

    /// Cross-palace recall.
    ///
    /// Why: shared between `/api/v1/recall` and the `memory_recall_all` chat
    /// tool. Encapsulating the open-everything-fanout-merge dance avoids
    /// drift.
    /// What: lists every palace, opens handles (skipping failures with a
    /// `tracing::warn!`), delegates to
    /// `recall_across_palaces_with_default_embedder`. Returns a JSON array.
    /// Test: indirectly via `recall_across_palaces_merges_results` and the
    /// MCP `memory_recall_all` integration paths.
    pub async fn recall_all(&self, query: &str, top_k: usize, deep: bool) -> Value {
        let palaces = match PalaceRegistry::list_palaces(&self.state.data_root) {
            Ok(v) => v,
            Err(e) => return json!({ "error": format!("list palaces: {e:#}") }),
        };
        let mut handles = Vec::with_capacity(palaces.len());
        for p in &palaces {
            match self
                .state
                .registry
                .open_palace(&self.state.data_root, &p.id)
            {
                Ok(h) => handles.push(h),
                Err(e) => {
                    tracing::warn!(palace = %p.id, "recall_all: open failed: {e:#}");
                }
            }
        }
        if handles.is_empty() {
            return json!([]);
        }
        match recall_across_palaces_with_default_embedder(&handles, query, top_k, deep).await {
            Ok(results) => json!(results
                .into_iter()
                .map(|r| json!({
                    "palace_id": r.palace_id,
                    "drawer_id": r.result.drawer.id.to_string(),
                    "content": r.result.drawer.content,
                    "importance": r.result.drawer.importance,
                    "tags": r.result.drawer.tags,
                    "score": r.result.score,
                    "layer": r.result.layer,
                }))
                .collect::<Vec<_>>()),
            Err(e) => json!({ "error": format!("recall_across_palaces: {e:#}") }),
        }
    }

    // -----------------------------------------------------------------
    // Knowledge graph
    // -----------------------------------------------------------------

    /// Query the KG for all active triples whose subject matches.
    pub async fn kg_query(&self, id: &str, subject: &str) -> ServiceResult<Vec<Triple>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .query_active(subject)
            .await
            .map_err(|e| ServiceError::internal(format!("kg query: {e:#}")))
    }

    /// Assert a triple in the KG.
    pub async fn kg_assert(&self, id: &str, body: KgAssertBody) -> ServiceResult<()> {
        let handle = self.open_handle(id)?;
        let triple = Triple {
            subject: body.subject,
            predicate: body.predicate,
            object: body.object,
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: body.confidence.unwrap_or(1.0),
            provenance: body.provenance,
        };
        handle
            .kg
            .assert(triple)
            .await
            .map_err(|e| ServiceError::internal(format!("kg assert: {e:#}")))
    }

    /// List distinct subjects in the KG.
    pub async fn kg_list_subjects(&self, id: &str, limit: usize) -> ServiceResult<Vec<String>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .list_subjects(limit)
            .map_err(|e| ServiceError::internal(format!("kg list_subjects: {e:#}")))
    }

    /// List distinct subjects in the KG paired with their active-triple count.
    pub async fn kg_list_subjects_with_counts(
        &self,
        id: &str,
        limit: usize,
    ) -> ServiceResult<Vec<(String, u64)>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .list_subjects_with_counts(limit)
            .map_err(|e| ServiceError::internal(format!("kg list_subjects_with_counts: {e:#}")))
    }

    /// Page through every active triple.
    pub async fn kg_list_all(
        &self,
        id: &str,
        limit: usize,
        offset: usize,
    ) -> ServiceResult<Vec<Triple>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .list_active(limit, offset)
            .await
            .map_err(|e| ServiceError::internal(format!("kg list_active: {e:#}")))
    }

    /// Return the count of currently-active triples.
    pub async fn kg_count(&self, id: &str) -> ServiceResult<usize> {
        let handle = self.open_handle(id)?;
        Ok(handle.kg.count_active_triples())
    }

    /// Build the per-palace visual graph payload.
    pub async fn kg_graph(&self, id: &str) -> ServiceResult<KgGraphPayload> {
        let handle = self.open_handle(id)?;
        let triples = handle
            .kg
            .list_active(KG_GRAPH_MAX_TRIPLES, 0)
            .await
            .map_err(|e| ServiceError::internal(format!("kg list_active: {e:#}")))?;
        Ok(KgGraphPayload {
            triples,
            node_count: handle.kg.node_count() as u64,
            edge_count: handle.kg.edge_count() as u64,
            community_count: handle.kg.community_count() as u64,
        })
    }

    // -----------------------------------------------------------------
    // Dream cycle
    // -----------------------------------------------------------------

    /// Aggregate dream stats across every persisted palace.
    pub async fn dream_status_aggregate(&self) -> DreamStatusPayload {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root).unwrap_or_default();
        let mut out = DreamStatusPayload::default();
        let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
        for p in palaces {
            let data_dir = self.state.data_root.join(p.id.as_str());
            let snap = match PersistedDreamStats::load(&data_dir) {
                Ok(Some(s)) => s,
                _ => continue,
            };
            out.merged = out.merged.saturating_add(snap.stats.merged);
            out.pruned = out.pruned.saturating_add(snap.stats.pruned);
            out.compacted = out.compacted.saturating_add(snap.stats.compacted);
            out.closets_updated = out
                .closets_updated
                .saturating_add(snap.stats.closets_updated);
            out.duration_ms = out.duration_ms.saturating_add(snap.stats.duration_ms);
            latest = match latest {
                Some(t) if t >= snap.last_run_at => Some(t),
                _ => Some(snap.last_run_at),
            };
        }
        out.last_run_at = latest;
        out
    }

    /// Per-palace dream stats snapshot.
    pub async fn dream_status_for_palace(&self, id: &str) -> ServiceResult<DreamStatusPayload> {
        let data_dir = self.state.data_root.join(id);
        if !data_dir.exists() {
            return Err(ServiceError::not_found(format!("palace not found: {id}")));
        }
        match PersistedDreamStats::load(&data_dir) {
            Ok(Some(s)) => Ok(s.into()),
            Ok(None) => Ok(DreamStatusPayload::default()),
            Err(e) => Err(ServiceError::internal(format!("read dream stats: {e:#}"))),
        }
    }

    /// Run a dream cycle across every palace.
    pub async fn dream_run(&self) -> ServiceResult<DreamStatusPayload> {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root)
            .map_err(|e| ServiceError::internal(format!("list palaces: {e:#}")))?;
        let dreamer = Dreamer::new(DreamConfig::default());
        let mut out = DreamStatusPayload::default();
        for p in palaces {
            let handle = match self
                .state
                .registry
                .open_palace(&self.state.data_root, &p.id)
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(palace = %p.id, "dream_run: open failed: {e:#}");
                    continue;
                }
            };
            match dreamer.dream_cycle(&handle).await {
                Ok(stats) => {
                    out.merged = out.merged.saturating_add(stats.merged);
                    out.pruned = out.pruned.saturating_add(stats.pruned);
                    out.compacted = out.compacted.saturating_add(stats.compacted);
                    out.closets_updated = out.closets_updated.saturating_add(stats.closets_updated);
                    out.duration_ms = out.duration_ms.saturating_add(stats.duration_ms);
                }
                Err(e) => tracing::warn!(palace = %p.id, "dream_run: cycle failed: {e:#}"),
            }
            refresh_gaps_cache(&self.state, &handle).await;
        }
        out.last_run_at = Some(chrono::Utc::now());
        self.state.emit(DaemonEvent::DreamCompleted {
            palace_id: None,
            merged: out.merged,
            pruned: out.pruned,
            compacted: out.compacted,
            closets_updated: out.closets_updated,
            duration_ms: out.duration_ms,
            source: ActivitySource::Http,
        });
        self.state.emit(self.aggregate_status_event());
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Activity log
    // -----------------------------------------------------------------

    /// Paginated activity-log read.
    pub async fn list_activity(
        &self,
        filter: ActivityFilter,
        limit: usize,
        offset: usize,
    ) -> ServiceResult<(Vec<crate::ActivityEntry>, u64)> {
        let entries = self
            .state
            .activity_log
            .list(&filter, limit, offset)
            .map_err(|e| ServiceError::internal(format!("activity list: {e:#}")))?;
        let total = self
            .state
            .activity_log
            .count()
            .map_err(|e| ServiceError::internal(format!("activity count: {e:#}")))?;
        Ok((entries, total))
    }

    // -----------------------------------------------------------------
    // Internal helper — open a palace handle or return 404.
    // -----------------------------------------------------------------

    /// Open the named palace, returning `ServiceError::NotFound` on failure.
    pub fn open_handle(&self, id: &str) -> ServiceResult<Arc<PalaceHandle>> {
        self.state
            .registry
            .open_palace(&self.state.data_root, &PalaceId::new(id))
            .map_err(|e| ServiceError::not_found(format!("palace not found: {id} ({e:#})")))
    }
}

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
