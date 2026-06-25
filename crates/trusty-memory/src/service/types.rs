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
