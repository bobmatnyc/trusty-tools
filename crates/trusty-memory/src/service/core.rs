//! `MemoryService` — the pure business-logic facade over `AppState`.
//!
//! Why: lets the axum HTTP handlers stay thin one-liners and lets non-HTTP
//! callers (chat tool dispatch, RPC bridges) reuse the same code paths without
//! dragging axum types around (split out of the former monolithic `service.rs`,
//! issue #607).
//! What: the `MemoryService` struct + its full async method surface, moved
//! verbatim. Each method returns `anyhow::Result<Value>` or a typed
//! `ServiceResult`.
//! Test: every method is covered by the corresponding handler test in
//! `web::tests`.

use crate::attribution::CreatorInfo;
use crate::{ActivitySource, AppState, DaemonEvent};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall_across_palaces_with_default_embedder, recall_deep_with_default_embedder,
    recall_with_default_embedder,
};
use trusty_common::memory_core::PalaceRegistry;
use uuid::Uuid;

use super::helpers::{
    collect_palace_stats, drawer_content_preview, drawer_snippet, is_reserved_system_palace,
    palace_info_from, recall_entry_json,
};
use super::types::{
    CreateDrawerBody, CreatePalaceBody, ListDrawersQuery, PalaceInfo, ServiceError, ServiceResult,
    StatusPayload,
};

/// Hard cap on triples returned by the per-palace graph endpoint.
pub(super) const KG_GRAPH_MAX_TRIPLES: usize = 5_000;

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
    pub(super) state: AppState,
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
        // The `/status` endpoint is the one place we still want a disk view —
        // an operator hitting this endpoint right after restart (before
        // `load_palaces_from_disk` finishes) should still see every persisted
        // palace counted, even if it isn't in the in-memory registry yet.
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root).unwrap_or_default();
        let palace_count = palaces.len();
        let stats = collect_palace_stats(&self.state, palaces.iter().map(|p| &p.id));
        StatusPayload {
            version: self.state.version.clone(),
            palace_count,
            default_palace: self.state.default_palace.clone(),
            data_root: self.state.data_root.display().to_string(),
            total_drawers: stats.total_drawers,
            total_vectors: stats.total_vectors,
            total_kg_triples: stats.total_kg_triples,
        }
    }

    /// Compute the aggregate `StatusChanged` event used by SSE consumers.
    ///
    /// Why: mutating handlers — and the periodic status ticker — push a
    /// refreshed status snapshot so dashboards stay in sync without an
    /// extra `/api/v1/status` request.
    /// Why (issue #228): this used to call `PalaceRegistry::list_palaces`
    /// (a synchronous disk walk) + `open_palace` (more disk I/O on first
    /// call) for every palace on every emit. Since every persisted palace
    /// is already loaded into the in-memory registry by
    /// `AppState::load_palaces_from_disk` at startup (and every `create_palace`
    /// keeps it in sync), iterating the in-memory registry returns the same
    /// counts without touching disk.
    /// What: iterates `state.registry.list()` (a `DashMap` snapshot) and
    /// sums the live handle stats via [`collect_palace_stats`]. Returns a
    /// `DaemonEvent::StatusChanged`. Palaces that fail to resolve in the
    /// registry (race during shutdown) are silently skipped — the next
    /// emit will catch them.
    /// Test: indirectly via SSE integration tests; the math is identical to
    /// the disk-walk implementation and the `status_endpoint_returns_payload`
    /// test still passes against `status()` (which keeps the disk view for
    /// the dedicated endpoint).
    pub fn aggregate_status_event(&self) -> DaemonEvent {
        let ids: Vec<PalaceId> = self.state.registry.list();
        let stats = collect_palace_stats(&self.state, ids.iter());
        DaemonEvent::StatusChanged {
            total_drawers: stats.total_drawers,
            total_vectors: stats.total_vectors,
            total_kg_triples: stats.total_kg_triples,
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
        // Issue #88 / Change 2: enforce palace = project mapping for
        // HTTP-originated palace creation. The validation cwd is, in order of
        // preference:
        //   a. `body.cwd` — the caller explicitly supplied their project path
        //      (correct for any client that is not the daemon itself).
        //   b. `std::env::current_dir()` — daemon's own cwd, the pre-Change-2
        //      fallback (rarely meaningful when the daemon is launched from ~).
        // This keeps older clients that omit `cwd` working without a breaking
        // change, while letting pin-file-aware clients get accurate validation.
        // spec-001: `force=true` lets an application bypass the project-slug
        // gate so it can create palaces under arbitrary slugs (e.g. one per
        // app/tenant for chat-session storage). The env-var bypass remains for
        // test contexts; both short-circuit the same validation call.
        //
        // KNOWN MVP LIMITATION (tracked follow-up): `force=true` bypasses slug
        // validation with NO authorization check — any caller that can reach
        // this endpoint can create a palace under an arbitrary slug, including
        // one that collides with another tenant's namespace. This is intended
        // for trusted / single-tenant callers only. Multi-tenant auth gating
        // (verifying the caller owns the requested slug) is a tracked follow-up;
        // do not expose `force` to untrusted clients until that lands.
        let skip_enforcement =
            std::env::var("TRUSTY_SKIP_PALACE_ENFORCEMENT").as_deref() == Ok("1");
        if !skip_enforcement && !body.force {
            let cwd = body
                .cwd
                .as_deref()
                .map(std::path::Path::new)
                .map(|p| p.to_path_buf())
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| self.state.data_root.clone());
            crate::project_root::validate_palace_name(&name, &cwd)
                .map_err(|e| ServiceError::bad_request(e.to_string()))?;
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
        // Issue #228: keep the in-memory palace-name cache in sync so writes
        // to this palace can resolve `Palace.name` without a disk walk.
        self.state.palace_names.insert(name.clone(), name.clone());
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
        // Issue #228: drop the palace-name cache entry so future writes never
        // resolve to a stale label.
        self.state.palace_names.remove(palace_id);
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
        // Issue #228: refresh the in-memory name cache so subsequent writes
        // surface the new label without a disk walk.
        self.state
            .palace_names
            .insert(palace_id.to_string(), trimmed.to_string());
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
        // Issue #228: refresh the in-memory name cache so subsequent writes
        // surface the new label without a disk walk.
        self.state
            .palace_names
            .insert(palace_id.to_string(), trimmed.to_string());
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
        // Issue #228: resolve from the in-memory cache instead of re-walking
        // the data root on every HTTP `create_drawer` call. Same cache the
        // MCP `lookup_palace_name` helper consults.
        let palace_name = self
            .state
            .palace_names
            .get(id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| id.to_string());
        self.state.emit(DaemonEvent::DrawerAdded {
            palace_id: id.to_string(),
            palace_name,
            drawer_count,
            timestamp: chrono::Utc::now(),
            content_preview,
            source,
        });
        // Issue #228: do NOT emit `StatusChanged` on every drawer create —
        // the periodic ticker (`run_http_on`) refreshes aggregate totals on
        // a fixed cadence so dashboards stay current without an O(N palaces)
        // recompute on the write hot path.
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
        // Issue #228: skip the per-write `StatusChanged` emit — the
        // periodic ticker handles aggregate roll-ups.
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
}
