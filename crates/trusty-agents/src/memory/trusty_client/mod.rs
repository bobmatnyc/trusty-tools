//! JSON-RPC client for an external `trusty-memory` daemon, plus a
//! `MemoryBackend` enum that picks between Trusty and the local
//! `RedbUsearchStore` at runtime.
//!
//! Why: trusty-agents ships an embedded memory store (redb + usearch) so it
//! works out of the box with no external services. When operators run the
//! `trusty-memory` daemon locally, however, they get a richer Palace/Room/
//! Drawer model that is worth using transparently. Auto-detection at startup
//! avoids forcing a config flag — if the daemon is up, we use it; otherwise we
//! fall back to the embedded store with no behavior change for the user.
//!
//! What: `TrustyMemoryClient` implements `MemoryStore` against trusty-memory's
//! Unix socket, through the one shared client `trusty_common::memory_rpc`
//! (#6286). It used to hit `/health` and four `/api/v1/palaces/*` routes with
//! its own `reqwest::Client`; ADR-0032 retired that listener, and the routes map
//! onto methods one for one:
//!   - `memory.health` — liveness, replacing the bare `GET /health`.
//!   - `palace_create` — one palace per `Segment`, named
//!     `trusty-agents-{segment-prefix}`. `force: true` bypasses the
//!     project-slug naming gate, which is meant for user-facing palaces tied to
//!     a project checkout, not this adapter's synthetic per-segment palaces.
//!   - `memory.drawer_create` — trusty-memory computes its own embedding
//!     server-side (ONNX, via the shared embedder) from `content`; no method
//!     accepts a caller-supplied vector, so `insert`'s `vector` argument is
//!     taken for trait-compatibility and not transmitted. `content` is the
//!     JSON-serialized `payload` (not a text summary) so `get` can losslessly
//!     round-trip it — `CreateDrawerBody` has no side-channel metadata field.
//!   - `memory.drawers_list` with `tag` — trusty-agents' string ids are tagged
//!     onto the drawer at insert time (`ns_id(segment, id)`) because drawers are
//!     keyed by server-generated UUID, not caller id; `get`/`delete` resolve the
//!     UUID via an exact tag-match lookup first.
//!   - `memory.drawer_delete` — delete by resolved UUID.
//!
//! **Not-found is a clean empty result, not a failure.** The REST client read
//! that off a 404 status; it now reads `MemoryRpcError::is_not_found`, so a
//! never-created palace still answers `None` rather than a spurious error while
//! a transport failure still propagates.
//!
//! `search(segment, query_vec, top_k)` is still unsupported — see that method's
//! doc comment for why a raw vector cannot be bridged onto trusty-memory's
//! text-embedding recall.
//!
//! Test: See `tests` below — `health_check_false_when_daemon_absent`;
//! `auto_detect` falls back to local when unreachable; a real trusty-memory
//! socket bound by the test proves `auto_detect` selects the RPC backend when
//! the daemon *is* reachable, and exercises the insert/get/delete round trip.

#![allow(dead_code)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::memory_rpc::{MemoryRpcError, call_memory_tool_at_with_timeout};
use uuid::Uuid;

use crate::memory::redb_usearch::RedbUsearchStore;
use crate::memory::store::{MemoryResult, MemoryStore, Segment};

/// The socket the trusty-memory daemon serves on.
///
/// Why (#3336, then #6286): this was a standalone `7775` literal that had
/// silently drifted from the daemon's real bind port, so auto-detection probed
/// a port nothing listens on and every install fell back to the embedded store.
/// The fix pointed it at `trusty_memory::DEFAULT_HTTP_PORT`; ADR-0032 removed
/// both the port and the constant. The path is derived by the same
/// `trusty_common::daemon_socket_path` call the daemon makes, so there is no
/// value left that can drift.
///
/// What: `trusty_common::memory_rpc::resolve_memory_socket_or_unreachable` —
/// fail-open, because a data directory that cannot be resolved must land on the
/// embedded store rather than abort startup.
/// Test: `default_trusty_socket_is_the_derived_daemon_path`.
pub fn default_trusty_socket() -> PathBuf {
    trusty_common::memory_rpc::resolve_memory_socket_or_unreachable()
}

/// Connect timeout for the auto-detect health probe. Kept short so startup
/// never noticeably stalls when the daemon is down.
const HEALTH_TIMEOUT: Duration = Duration::from_millis(500);

/// Per-call timeout for normal CRUD requests (longer than the health probe
/// because actual operations may be slower than a TCP handshake).
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Client wrapping the trusty-memory daemon's JSON-RPC surface.
///
/// Why: threading raw RPC calls through the codebase would couple callers to
/// method names and JSON shapes. Wrapping them in a typed client keeps the
/// `MemoryStore` impl self-contained.
/// What: holds the daemon's socket path and a small in-memory cache of palace
/// ids already confirmed to exist (`ensure_palace` skips the create round-trip
/// once a segment's palace is known-present for this client's lifetime). The
/// `MemoryStore` impl maps our segment-scoped ids to `mem:{segment}:{id}` tags
/// so different segments — and different records within a segment — don't
/// collide in trusty-memory's UUID-keyed drawer namespace.
/// Test: see `health_check_false_when_daemon_absent`,
/// `auto_detect_falls_back_to_local`, and the live-socket round-trip tests.
pub struct TrustyMemoryClient {
    socket: PathBuf,
    /// Palace ids this client has already created/confirmed. Avoids a
    /// `palace_create` round-trip on every `insert`.
    ensured_palaces: RwLock<HashSet<String>>,
}

impl TrustyMemoryClient {
    /// Construct a client dialling `socket`.
    ///
    /// Why: a test drives a daemon it bound on a temp path, and an operator can
    /// pin one with `TRUSTY_MEMORY_SOCKET`; taking the path here keeps the rest
    /// of the code parameterless.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            ensured_palaces: RwLock::new(HashSet::new()),
        }
    }

    /// One RPC call on this client's socket.
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        call_memory_tool_at_with_timeout(&self.socket, method, params, timeout).await
    }

    /// Probe `memory.health`. Returns true when the daemon answers.
    ///
    /// Why: used by `MemoryBackend::auto_detect` to decide whether the daemon is
    /// reachable. Any error (timeout, refused, unresolvable path) is mapped to
    /// false so callers don't have to handle transport errors at startup.
    /// Issue #3225 fixed this probing a route the router never mounted; #6286
    /// moved it onto the method `trusty-console` and `tctl` dial, so all three
    /// now agree by construction.
    pub async fn health_check(&self) -> bool {
        self.call("memory.health", json!({}), HEALTH_TIMEOUT)
            .await
            .is_ok()
    }

    /// Build the namespaced id used to address a record on the daemon.
    fn ns_id(segment: Segment, id: &str) -> String {
        format!("mem:{}:{}", segment.prefix(), id)
    }

    /// Deterministic palace id for `segment` — one palace per segment.
    ///
    /// Why: mirrors the `trusty-agents-{prefix}` naming convention already
    /// established by `TrustyBackedMemoryStore::palace_id_for` for the
    /// in-process Palace adapter, so an operator browsing the daemon's admin
    /// UI sees a consistent naming scheme regardless of which trusty-agents
    /// adapter wrote the data.
    fn palace_id_for(segment: Segment) -> String {
        format!("trusty-agents-{}", segment.prefix())
    }

    /// Ensure `segment`'s palace exists on the daemon, creating it on first
    /// use and caching the result for the lifetime of this client.
    ///
    /// Why: `POST /api/v1/palaces/{id}/drawers` 404s against a palace that
    /// was never created. `force: true` bypasses trusty-memory's
    /// project-slug naming gate (`validate_palace_name`), which is meant for
    /// user-facing palaces tied to a project checkout, not this adapter's
    /// synthetic per-segment palaces.
    /// What: Checks the in-memory cache first; on a miss, POSTs a create
    /// request (idempotent in effect — trusty-memory's `create_palace`
    /// overwrites `palace.json` rather than erroring when the directory
    /// already exists) and caches the id on success.
    /// Test: `insert_get_delete_round_trip_against_mock_daemon` (mock-server
    /// test — `insert` calls this on every write, so the round trip exercises
    /// palace auto-creation as a side effect).
    async fn ensure_palace(&self, segment: Segment) -> Result<()> {
        let palace_id = Self::palace_id_for(segment);
        {
            let cache = self
                .ensured_palaces
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if cache.contains(&palace_id) {
                return Ok(());
            }
        }

        self.call(
            "palace_create",
            json!({
                "name": palace_id,
                "description": "Auto-created by trusty-agents TrustyMemoryClient",
                "force": true,
            }),
            CALL_TIMEOUT,
        )
        .await
        .with_context(|| format!("trusty palace_create({palace_id})"))?;

        self.ensured_palaces
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(palace_id);
        Ok(())
    }

    /// Resolve the drawer(s) tagged with `tag` inside `palace_id`.
    ///
    /// Why: shared by `get`, `delete`, and `insert`'s upsert-cleanup step —
    /// all three need to translate our caller-supplied string id into the
    /// server-generated UUID that `DELETE .../drawers/{uuid}` requires.
    /// What: `GET /api/v1/palaces/{palace_id}/drawers?tag=<tag>&limit=1`. A
    /// 404 (unknown palace) is treated as "no matches" rather than an error
    /// so callers see a clean `None`/empty result instead of a spurious
    /// failure on a not-yet-created palace.
    /// Test: `get_and_delete_are_clean_when_absent`, plus
    /// `insert_upserts_existing_id` for the upsert-cleanup lookup path.
    async fn find_by_tag(&self, palace_id: &str, tag: &str) -> Result<Vec<DrawerRow>> {
        let raw = match self
            .call(
                "memory.drawers_list",
                json!({ "palace_id": palace_id, "tag": tag, "limit": 1 }),
                CALL_TIMEOUT,
            )
            .await
        {
            Ok(v) => v,
            Err(e) if is_not_found(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.context("trusty memory.drawers_list")),
        };
        serde_json::from_value(raw).context("parsing drawers response")
    }

    /// Delete a drawer by its resolved server UUID. 404 is treated as
    /// success (already gone).
    async fn delete_by_uuid(&self, palace_id: &str, drawer_id: Uuid) -> Result<()> {
        match self
            .call(
                "memory.drawer_delete",
                json!({ "palace_id": palace_id, "drawer_id": drawer_id.to_string() }),
                CALL_TIMEOUT,
            )
            .await
        {
            Ok(_) => Ok(()),
            // Already gone is the outcome the caller wanted.
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.context("trusty memory.drawer_delete")),
        }
    }
}

/// Did the daemon answer "no such thing" rather than fail?
///
/// Why: `get` and `delete` against a palace this client never created must be
/// clean empty results, and `find_by_tag` is the lookup all three share. The
/// REST predecessor read this off a 404 status; the typed error carries the same
/// distinction over the socket (#6286).
/// Test: `get_and_delete_are_clean_when_absent`.
fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MemoryRpcError>()
        .is_some_and(MemoryRpcError::is_not_found)
}

/// Minimal shape we read back from `memory.drawers_list` — trusty-memory's
/// `Drawer` struct (`trusty_common::memory_core::palace::Drawer`) has many
/// more fields; we only need `id` and `content` to resolve/round-trip a
/// record, plus `tags` for defensive filtering client-side.
#[derive(Deserialize)]
struct DrawerRow {
    id: Uuid,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[async_trait]
impl MemoryStore for TrustyMemoryClient {
    /// Upsert: drawers are keyed by server-generated UUID, not our string
    /// id, so a naive `insert` for an id that already exists would create a
    /// duplicate drawer rather than overwrite the first (unlike the
    /// redb-keyed local store). This creates the NEW drawer first and only
    /// deletes the old one (best-effort) after the create succeeds — never
    /// the reverse — so a failed create can never destroy the prior record;
    /// worst case on a create failure is the stale drawer survives untouched
    /// and `insert` returns `Err`, which is the correct "nothing changed"
    /// outcome for the caller to retry.
    ///
    /// Known limitation (single-writer assumption): the find-old →
    /// create-new → delete-old sequence is not atomic. Two concurrent
    /// `insert` calls for the SAME `(segment, id)` can both pass the
    /// find-old lookup before either creates, race to create their own new
    /// drawer, and then race to delete what they each believe is "the old
    /// one" — leaving two drawers tagged with the same `ns_id` (a duplicate,
    /// not data loss) until the next `insert`/`delete` for that id cleans
    /// one up via `find_by_tag`'s `limit=1` lookup. This mirrors the
    /// workstream's documented model elsewhere in this crate (e.g.
    /// `TrustyBackedMemoryStore`'s in-memory sidecar) of assuming a single
    /// writer per `(segment, id)` rather than providing cross-request
    /// locking; a proper fix would need a server-side compare-and-swap
    /// primitive trusty-memory's drawer API does not have.
    /// Test: `insert_upserts_existing_id`, `insert_get_delete_round_trip_against_mock_daemon`
    /// and `insert_sends_force_true_for_realistic_low_prose_payload` cover
    /// the upsert-overwrite, create-succeeds, and force-transmission paths
    /// respectively. The concurrent-race window itself is documented, not
    /// mechanically tested — see the limitation above.
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: Value,
    ) -> Result<()> {
        // trusty-memory computes its own embedding server-side (ONNX, via
        // the shared embedder) from `content` on `POST .../drawers`; there
        // is no endpoint that accepts a caller-precomputed vector, so it is
        // intentionally not transmitted. See the module doc comment.
        let _ = vector;

        self.ensure_palace(segment).await?;
        let palace_id = Self::palace_id_for(segment);
        let ns_id = Self::ns_id(segment, id);

        let existing = self
            .find_by_tag(&palace_id, &ns_id)
            .await?
            .into_iter()
            .next();

        // Serialize the full payload (rather than a text summary) as `content`
        // so `get` can losslessly round-trip it — `CreateDrawerBody` carries no
        // separate metadata field. That makes `content` JSON-shaped, which
        // trusty-memory's signal/noise QUALITY gate (`non_alphabetic_ratio`)
        // rejects by design for structured payloads, so `force: true` is
        // required on every write this client makes. The daemon's secret gate
        // (`check_secret`) runs regardless of `force`.
        let content = serde_json::to_string(&payload).context("serialize payload as content")?;

        // Create failing leaves the prior drawer untouched, so there is nothing
        // to roll back on this arm.
        self.call(
            "memory.drawer_create",
            json!({
                "palace_id": palace_id,
                "content": content,
                "tags": [ns_id],
                "force": true,
            }),
            CALL_TIMEOUT,
        )
        .await
        .context("trusty memory.drawer_create")?;

        // Create succeeded — now it's safe to best-effort clean up the old
        // drawer. A failure here leaves a harmless stale duplicate (caught
        // by `find_by_tag`'s next lookup) rather than data loss, so errors
        // are logged, not propagated.
        if let Some(existing) = existing
            && let Err(e) = self.delete_by_uuid(&palace_id, existing.id).await
        {
            tracing::warn!(
                %palace_id, drawer_id = %existing.id,
                "trusty insert: best-effort cleanup of superseded drawer failed: {e:#}"
            );
        }

        Ok(())
    }

    /// Not supported over HTTP against a real trusty-memory daemon.
    ///
    /// Why: `MemoryStore::search` takes a pre-computed `query_vec: &[f32]`
    /// (the contract the local `RedbUsearchStore`/`TrustyBackedMemoryStore`
    /// adapters satisfy by querying an in-process HNSW index directly).
    /// trusty-memory's only recall surface is text — `memory_recall` and
    /// `memory_recall_all` take a `q` string and the daemon computes the query
    /// embedding itself, server-side
    /// (`trusty_common::memory_core::retrieval::recall_with_default_embedder`).
    /// No method accepts a raw vector, and embeddings are not invertible, so a
    /// caller-supplied vector cannot be turned into the `q` text those methods
    /// require. Adding a vector-accepting recall method to trusty-memory was
    /// judged out of scope for issue #3225 (a route-correctness fix, not a new
    /// daemon capability) and #6286 (a transport migration).
    /// What: Returns a descriptive `Err` rather than silently calling the wrong
    /// method or fabricating empty results.
    /// Test: `search_returns_descriptive_unsupported_error`.
    async fn search(
        &self,
        _segment: Segment,
        _query_vec: &[f32],
        _top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        Err(anyhow!(
            "TrustyMemoryClient::search is not supported: trusty-memory exposes only \
             text-query recall (memory_recall / memory_recall_all take a `q` string), which \
             computes its own embedding server-side. No method accepts a pre-computed \
             vector, so a MemoryStore::search() call cannot be bridged onto it. Use \
             MemoryBackend::Local, or extend the daemon/trait with a text-aware recall \
             path, for vector/semantic search against the daemon backend."
        ))
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<Value>> {
        let palace_id = Self::palace_id_for(segment);
        let ns_id = Self::ns_id(segment, id);
        let Some(row) = self
            .find_by_tag(&palace_id, &ns_id)
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        // `content` is the JSON-serialized payload written by `insert`.
        // Fall back to a plain string wrap for any drawer written by a
        // different producer (e.g. hand-authored via the admin UI) whose
        // content isn't valid JSON.
        match serde_json::from_str::<Value>(&row.content) {
            Ok(v) => Ok(Some(v)),
            Err(_) => Ok(Some(Value::String(row.content))),
        }
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        let palace_id = Self::palace_id_for(segment);
        let ns_id = Self::ns_id(segment, id);
        let Some(row) = self
            .find_by_tag(&palace_id, &ns_id)
            .await?
            .into_iter()
            .next()
        else {
            // Nothing tagged with this id — caller wanted it gone, it's gone.
            return Ok(());
        };
        self.delete_by_uuid(&palace_id, row.id).await
    }
}

/// Runtime-selected memory backend.
///
/// Why: We want `auto_detect` to pick the best available backend without
/// every caller branching on configuration. Wrapping both options in an
/// enum lets us implement `MemoryStore` once and have callers stay
/// transport-agnostic.
/// What: `Local` holds an `Arc<RedbUsearchStore>` (shareable across tasks);
/// `Trusty` holds a socket-dialling client.
/// Test: `auto_detect_falls_back_to_local` exercises the selection path;
/// `auto_detect_selects_trusty_when_daemon_reachable` exercises the happy path
/// against a real trusty-memory socket the test binds.
pub enum MemoryBackend {
    Local(Arc<RedbUsearchStore>),
    Trusty(TrustyMemoryClient),
}

impl MemoryBackend {
    /// Auto-detect at the derived daemon socket.
    pub async fn auto_detect(local_store: Arc<RedbUsearchStore>) -> Self {
        Self::auto_detect_at(default_trusty_socket(), local_store).await
    }

    /// Auto-detect at `socket`. Falls back to `local_store` if the daemon is
    /// unreachable within the health timeout.
    pub async fn auto_detect_at(
        socket: impl Into<PathBuf>,
        local_store: Arc<RedbUsearchStore>,
    ) -> Self {
        let socket = socket.into();
        let client = TrustyMemoryClient::new(socket.clone());
        if client.health_check().await {
            tracing::info!(socket = %socket.display(), "trusty-memory daemon reachable; using the daemon backend");
            MemoryBackend::Trusty(client)
        } else {
            tracing::debug!(
                socket = %socket.display(),
                "trusty-memory daemon not reachable; using embedded local backend"
            );
            MemoryBackend::Local(local_store)
        }
    }
}

#[async_trait]
impl MemoryStore for MemoryBackend {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: Value,
    ) -> Result<()> {
        match self {
            MemoryBackend::Local(s) => s.insert(segment, id, vector, payload).await,
            MemoryBackend::Trusty(c) => c.insert(segment, id, vector, payload).await,
        }
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        match self {
            MemoryBackend::Local(s) => s.search(segment, query_vec, top_k).await,
            MemoryBackend::Trusty(c) => c.search(segment, query_vec, top_k).await,
        }
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<Value>> {
        match self {
            MemoryBackend::Local(s) => s.get(segment, id).await,
            MemoryBackend::Trusty(c) => c.get(segment, id).await,
        }
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        match self {
            MemoryBackend::Local(s) => s.delete(segment, id).await,
            MemoryBackend::Trusty(c) => c.delete(segment, id).await,
        }
    }
}

#[cfg(test)]
mod tests;
