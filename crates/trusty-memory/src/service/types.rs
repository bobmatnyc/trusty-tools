//! Wire types shared between the trusty-memory HTTP handlers and the service
//! layer.
//!
//! Why: both the axum handlers and the `MemoryService` business layer need the
//! same serializable request/response shapes; hosting them in one submodule
//! keeps the wire contract single-source (split out of the former monolithic
//! `service.rs`, issue #607).
//! What: the `PalaceInfo`/`*Body`/`*Query`/`*Payload` structs plus the
//! `ServiceError` enum and `ServiceResult` alias, moved verbatim.
//! Test: covered indirectly via the HTTP tests in `web::tests` and
//! `service::tests`.

use serde::{Deserialize, Serialize};
use trusty_common::memory_core::dream::PersistedDreamStats;
use trusty_common::memory_core::store::kg::Triple;

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
    /// Whether the palace's handle was resident in the registry's open-handle
    /// cache when this row was built (issue #4637).
    ///
    /// The full-registry list route no longer force-opens every palace — at
    /// 5,794 palaces that was ~90 minutes of cold disk I/O per request. When
    /// this is `false`, `drawer_count` / `vector_count` / `kg_triple_count` /
    /// `wing_count` / `node_count` / `edge_count` / `community_count` are `0`
    /// because they are **unknown**, not because they are empty; fetch
    /// `GET /api/v1/palaces/{id}` for live counts on a specific palace.
    /// `#[serde(default)]` keeps the field omissible for older clients.
    #[serde(default)]
    pub cached: bool,
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
    /// Fading high-value memories recorded by the last dream cycle (issue
    /// #2352). Populated from a per-palace snapshot via `From<PersistedDreamStats>`;
    /// left empty on the cross-palace aggregate endpoint (no single palace to
    /// attribute). `#[serde(default)]` keeps it omissible for older clients.
    #[serde(default)]
    pub fading: Vec<trusty_common::memory_core::dream::FadingMemory>,
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
            fading: p.stats.fading,
        }
    }
}

/// `POST /api/v1/palaces` body — service-facing version.
///
/// Why: Change 2 — the optional `cwd` field lets HTTP callers pass the
/// filesystem path of the project they are operating from. When present,
/// `validate_palace_name` uses it as the `start` for pin-file-based
/// validation instead of the daemon's own cwd (which is `~` or `/` and
/// rarely meaningful). When absent the existing daemon-cwd fallback applies
/// so older clients continue to work.
/// What: `name` is required; `description`, `cwd`, and `force` are optional.
/// Test: `create_palace_accepts_pinned_slug_via_cwd`,
///       `create_palace_rejects_mismatch_when_cwd_given`,
///       `create_palace_force_bypasses_validation`.
#[derive(Deserialize, Clone, Debug)]
pub struct CreatePalaceBody {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Optional caller working directory used for palace-name enforcement.
    /// When present, `validate_palace_name` uses this path instead of the
    /// daemon's process cwd. Useful when the daemon is launched from `~/`
    /// but the caller is inside a project tree.
    #[serde(default)]
    pub cwd: Option<String>,
    /// When `true`, bypass project-slug validation so an application can
    /// create a palace under an arbitrary slug (spec-001: trusty-memory as a
    /// chat session manager). Defaults to `false`, preserving the issue #88
    /// "palace name must match the project slug" gate for ordinary callers.
    #[serde(default)]
    pub force: bool,
}

/// `POST /api/v1/palaces/{id}/drawers` body — service-facing version.
///
/// Why: `content` normally goes through `PalaceHandle::remember`'s signal/
/// noise quality gate (noise patterns, short-content, non-alphabetic ratio —
/// see `trusty_common::memory_core::filter`), which is correct for
/// human/LLM-authored prose but rejects legitimate structured payloads (a
/// caller storing `{"score":0.42,"tier":3}`-shaped content is mostly
/// punctuation/digits, so `non_alphabetic_ratio` trips the gate). Issue
/// #3225 (trusty-agents `TrustyMemoryClient` writing JSON-serialized
/// payloads as `content` to preserve losslessness) needed a bypass that
/// mirrors the QUALITY-gate-only `force` semantics `RememberOptions`
/// already exposes internally — MCP/CLI callers never had a way to reach
/// it over HTTP.
/// What: `force` mirrors `RememberOptions::force` (issue #61/#2520):
/// `Some(true)` skips the noise/short-content/non-alphabetic QUALITY gates
/// only — the secret-detection gate (`check_secret`) still runs
/// unconditionally, so this can never be used to smuggle credential-shaped
/// content past screening. Defaults to `false` (`None`/absent), preserving
/// the existing gate for ordinary prose callers (the admin UI, MCP
/// `memory_remember`, `memory_note`).
/// Test: `create_drawer_rejects_json_content_without_force`,
/// `create_drawer_force_bypasses_quality_gate_for_json_content`.
#[derive(Deserialize, Clone, Debug)]
pub struct CreateDrawerBody {
    pub content: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub importance: Option<f32>,
    #[serde(default)]
    pub force: Option<bool>,
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
///
/// Why: the count fields are computed over the FULL in-memory adjacency while
/// `triples` is capped at `KG_GRAPH_MAX_TRIPLES`. Before issue #4670 nothing
/// in the payload said so, so a client rendering 5,000 triples while the badge
/// read "9,311 nodes" had no way to know it was looking at a partial graph —
/// and because `list_active` orders by `valid_from` DESC, the dropped triples
/// were silently the oldest ones. `returned_triple_count` / `active_triple_count`
/// / `truncated` make the gap explicit and machine-checkable.
/// What: the triple window plus both the true totals and what was actually
/// returned.
/// Test: `kg_graph_signals_truncation`, `kg_graph_returns_active_triples`.
#[derive(Serialize, Clone, Debug)]
pub struct KgGraphPayload {
    pub triples: Vec<Triple>,
    /// Distinct entities in the whole palace — NOT the node count of `triples`.
    pub node_count: u64,
    /// Directed edges in the whole palace — NOT `triples.len()`.
    pub edge_count: u64,
    pub community_count: u64,
    // #4670: the three fields below exist so no client can mistake a truncated
    // graph for a complete one.
    /// How many triples this response actually carries.
    pub returned_triple_count: u64,
    /// How many active triples exist in the palace.
    pub active_triple_count: u64,
    /// `returned_triple_count < active_triple_count`.
    pub truncated: bool,
}

/// One node in a progressive-exploration response.
///
/// Why (issue #4670): the client must be able to say "48 edges, 3 shown" —
/// that requires the node's degree in the WHOLE graph, not in the fragment it
/// was handed.
/// What: entity name plus its graph-wide in/out/total degree.
/// Test: `kg_graph_seed_ranks_by_degree`.
#[derive(Serialize, Clone, Debug)]
pub struct KgNodeView {
    pub id: String,
    pub degree: u64,
    pub in_degree: u64,
    pub out_degree: u64,
}

impl From<trusty_common::memory_core::store::kg::SeedNode> for KgNodeView {
    fn from(n: trusty_common::memory_core::store::kg::SeedNode) -> Self {
        Self {
            id: n.entity,
            degree: n.degree as u64,
            in_degree: n.in_degree as u64,
            out_degree: n.out_degree as u64,
        }
    }
}

/// Payload for `GET /api/v1/palaces/{id}/kg/graph/seed`.
///
/// Why (issue #4670): first paint should show the graph's skeleton (the
/// highest-degree nodes and the edges among them), never the whole hairball.
/// Carrying the palace-wide totals alongside the returned slice is what lets
/// the header read "75 of 9,311 nodes shown" instead of implying completeness.
/// What: the seed nodes, the induced edges as `Triple`s (same wire shape as
/// `/kg/graph`'s `triples`, so the client merges without a second parser), the
/// palace-wide totals, and the `limit` actually applied after clamping.
/// Test: `kg_graph_seed_ranks_by_degree`, `kg_graph_seed_clamps_limit`.
#[derive(Serialize, Clone, Debug)]
pub struct KgSeedPayload {
    pub nodes: Vec<KgNodeView>,
    pub triples: Vec<Triple>,
    pub node_count: u64,
    pub edge_count: u64,
    pub community_count: u64,
    pub returned_node_count: u64,
    pub returned_triple_count: u64,
    /// The clamped limit this response was built with.
    pub limit: u64,
    /// `returned_node_count < node_count`.
    pub truncated: bool,
}

/// Payload for `GET /api/v1/palaces/{id}/kg/graph/neighbors`.
///
/// Why (issue #4670): click-to-expand merges this into the rendered set, so it
/// needs the same node/triple shape as the seed. `community_count` is
/// deliberately absent — Louvain is the expensive part of the full-graph call
/// and the client already has the palace's community count from the seed load.
/// What: the reached nodes (origin first, so the client can anchor new nodes
/// on it), the traversed edges, and the echoed traversal parameters.
/// Test: `kg_neighbors_returns_incoming_edges`, `kg_neighbors_clamps_max_hops`.
#[derive(Serialize, Clone, Debug)]
pub struct KgNeighborsPayload {
    pub origin: String,
    pub nodes: Vec<KgNodeView>,
    pub triples: Vec<Triple>,
    pub returned_node_count: u64,
    pub returned_triple_count: u64,
    /// Echoed back after clamping so the client can see what actually ran.
    pub direction: String,
    pub max_hops: u64,
}

/// Status payload returned by `GET /api/v1/status`.
///
/// Issue #4637 changed the meaning of the three `total_*` fields: they are now
/// summed over the palaces resident in the registry's open-handle cache, not
/// over every palace on disk, because summing over disk meant force-opening
/// ~5,730 cold palaces per request. `palace_count` still reports the true
/// on-disk total; `cached_palace_count` reports how many of those the totals
/// actually cover.
#[derive(Serialize, Clone, Debug)]
pub struct StatusPayload {
    pub version: String,
    /// Every palace on disk.
    pub palace_count: usize,
    pub default_palace: Option<String>,
    pub data_root: String,
    /// Summed over cache-resident palaces only — see the type docs (#4637).
    pub total_drawers: usize,
    /// Summed over cache-resident palaces only — see the type docs (#4637).
    pub total_vectors: usize,
    /// Summed over cache-resident palaces only — see the type docs (#4637).
    pub total_kg_triples: usize,
    /// How many of `palace_count` palaces the three totals above cover
    /// (issue #4637). `#[serde(default)]` keeps it omissible for older clients.
    #[serde(default)]
    pub cached_palace_count: usize,
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
    /// 403 Forbidden — an authorization check failed.
    ///
    /// Why (issue #1714): `palace_create force=true` in multi-tenant mode
    /// fails the (currently fail-closed) `authz::authorize_force_palace_
    /// create` check. `BadRequest` would be misleading (the request is
    /// well-formed) and `Conflict` implies a state clash, not a permissions
    /// gate.
    #[error("{0}")]
    Forbidden(String),
}

impl ServiceError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
    /// Build a 403 Forbidden service error (issue #1714).
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
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
