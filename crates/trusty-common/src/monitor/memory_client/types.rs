//! Shared types and URL helpers for the trusty-memory client.
//!
//! Why: the wire shapes, public domain types, and URL-resolution helpers are
//! pure data — splitting them here keeps `client.rs` and `parsers.rs` free of
//! constant / struct declarations and makes unit-testing URL logic trivial.
//! What: constants, URL helpers, wire structs (`StatusWire`, `PalaceWire`),
//! and the public projection types (`RecallHit`, `DreamStats`, `MemoryEvent`,
//! `DrawerInfo`, `MemoryDetail`).
//! Test: URL helpers are tested in `tests.rs`; type defaults/derives are
//! implicitly exercised by the parser tests.

use std::time::Duration;

use serde::Deserialize;

/// Default trusty-memory daemon address used when discovery fails.
///
/// Why: trusty-memory binds a dynamic port, so the lock file is the primary
/// discovery path; the default only applies when no daemon has ever started.
/// What: a well-known local fallback base URL for trusty-memory.
/// Test: `default_memory_url_is_local`.
pub const DEFAULT_MEMORY_URL: &str = "http://127.0.0.1:7070";

/// Per-request timeout for trusty-memory probes.
///
/// Why: a hung daemon must not stall the dashboard refresh tick.
/// What: three seconds, matching `search_client`.
/// Test: exercised implicitly by every `MemoryClient::new` call in tests.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve the trusty-memory daemon base URL.
///
/// Why: trusty-memory's port is dynamic, so its address is read from the
/// service lock file; the default applies only when discovery yields nothing.
/// What: reads the `trusty-memory` daemon address; returns it `http://`-prefixed
/// when present, otherwise [`DEFAULT_MEMORY_URL`].
/// Test: `resolve_memory_url_returns_http_url`.
pub fn resolve_memory_url() -> String {
    match crate::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => normalize_url(&addr),
        _ => DEFAULT_MEMORY_URL.to_string(),
    }
}

/// Ensure a daemon address carries an `http://` scheme.
///
/// Why: the lock file stores a bare `host:port`; `reqwest` needs a full URL.
/// What: returns `raw` unchanged when it already has a scheme, else prefixes
/// `http://`.
/// Test: `normalize_url_adds_scheme`.
pub fn normalize_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

/// Wire shape of `GET /api/v1/status` from the trusty-memory daemon.
#[derive(Debug, Deserialize)]
pub(super) struct StatusWire {
    #[serde(default)]
    pub(super) version: String,
    #[serde(default)]
    pub(super) palace_count: u64,
    #[serde(default)]
    pub(super) total_drawers: u64,
    #[serde(default)]
    pub(super) total_vectors: u64,
    #[serde(default)]
    pub(super) total_kg_triples: u64,
}

/// Wire shape of one palace entry from `GET /api/v1/palaces`.
///
/// Why: the palace list response shape varies slightly between daemon
/// versions; all fields are optional with defaults so a partial payload still
/// deserializes rather than failing the whole poll.
#[derive(Debug, Default, Deserialize)]
pub(super) struct PalaceWire {
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default, alias = "vectors", alias = "total_vectors")]
    pub(super) vector_count: u64,
    #[serde(default)]
    pub(super) drawer_count: u64,
    #[serde(default)]
    pub(super) last_write_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub(super) description: Option<String>,
    /// Number of active KG triples; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    pub(super) kg_triple_count: u64,
    /// KG node count; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    pub(super) node_count: u64,
    /// KG edge count; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    pub(super) edge_count: u64,
    /// Detected community count; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    pub(super) community_count: u64,
    /// Whether a dream cycle is currently running; `#[serde(default)]` for
    /// forward-compat against pre-spinner daemon builds.
    #[serde(default)]
    pub(super) is_compacting: bool,
}

/// One recalled memory from a trusty-memory query, projected for the log.
///
/// Why: the memory TUI renders a compact one-line summary per recall hit; a
/// small typed struct keeps the renderer free of raw JSON.
/// What: the source palace id and a short content snippet with its score.
/// Test: `parse_recall_hits_projects_fields`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecallHit {
    /// The palace the memory was recalled from.
    pub palace_id: String,
    /// A short, single-line snippet of the recalled content.
    pub snippet: String,
    /// The relevance score of the recall (higher is closer).
    pub score: f32,
}

/// Aggregate counts returned by a `POST /api/v1/dream/run` cycle.
///
/// Why: the memory TUI shows what a dream cycle changed; a typed struct keeps
/// the renderer free of raw JSON.
/// What: the merged / pruned / compacted memory counts.
/// Test: `parse_dream_stats_reads_counts`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DreamStats {
    /// Memories merged into existing ones during the cycle.
    pub merged: u64,
    /// Memories pruned (forgotten) during the cycle.
    pub pruned: u64,
    /// Memories compacted during the cycle.
    pub compacted: u64,
}

/// One live event from the trusty-memory `/sse` stream.
///
/// Why: the memory TUI reacts to push events (dream cycles, drawer changes,
/// palace creation) in its activity log; a typed enum lets the renderer format
/// each distinctly without parsing raw JSON in the event loop.
/// What: mirrors the daemon's `DaemonEvent` — the `type`-tagged variants the
/// TUI displays. Unknown / housekeeping frames (`connected`, `lag`) are
/// dropped by [`super::parsers::parse_memory_event`].
/// Test: `parse_memory_event_maps_type_tag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryEvent {
    /// A new palace was created.
    PalaceCreated {
        /// The new palace's friendly name.
        name: String,
    },
    /// A drawer was added to a palace.
    DrawerAdded {
        /// The palace the drawer belongs to.
        palace_id: String,
        /// The palace's drawer count after the addition.
        drawer_count: u64,
        /// Short preview of the drawer's content (whitespace-collapsed,
        /// truncated to ~80 chars). Empty when the daemon did not provide
        /// the field (older daemons predate the wire field).
        content_preview: String,
    },
    /// A drawer was deleted from a palace.
    DrawerDeleted {
        /// The palace the drawer belonged to.
        palace_id: String,
        /// The palace's drawer count after the deletion.
        drawer_count: u64,
    },
    /// A dream cycle completed.
    DreamCompleted {
        /// Memories merged during the cycle.
        merged: u64,
        /// Memories pruned during the cycle.
        pruned: u64,
        /// Memories compacted during the cycle.
        compacted: u64,
    },
}

/// One drawer row projected for the TUI activity panel.
///
/// Why: the activity panel renders a compact one-line summary per drawer —
/// truncated id, creation timestamp, creator tag, and memory count.
/// Keeping a typed projection out of the renderer means the parsing /
/// creator-tag extraction logic is unit-testable without a live daemon.
/// What: a stable id, a created_at timestamp (UTC), the resolved creator
/// label (`"—"` when no creator tag was found), the drawer's tag list
/// surfaced so future panels can present richer detail without
/// re-fetching, and an optional content snippet for inline display
/// (issue #202; `None` when the body was empty or the daemon predates
/// the snippet field).
/// Test: `parse_drawers_projects_fields`, `creator_label_picks_first_match`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawerInfo {
    /// Stable drawer identifier (UUID as string).
    pub id: String,
    /// Creation timestamp as parsed from the wire payload.
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Resolved creator label (e.g. `"msg:from=cto"`, `"creator:client=mpm"`)
    /// or `"—"` when no recognised creator tag was attached.
    pub creator: String,
    /// All tags as carried on the wire (for downstream filtering / display).
    pub tags: Vec<String>,
    /// Short whitespace-collapsed snippet of the drawer body (issue #202).
    ///
    /// Populated by the daemon's `/api/v1/palaces/{id}/drawers` endpoint
    /// (truncated to ~60 chars with `…`). The client falls back to
    /// truncating the full `content` field when the daemon predates the
    /// `snippet` wire field; `None` when neither is available.
    pub snippet: Option<String>,
}

/// One drawer projected with its full body for the detail modal (issue #215).
///
/// Why: the activity panel's row only carries a truncated snippet; the
/// modal that opens on `Enter` needs the verbatim drawer body so the
/// operator can read the entire memory. Keeping this as a separate type
/// from [`DrawerInfo`] keeps the row layout helpers free of an unused
/// `content` field and makes the modal-renderer signature explicit.
/// What: drawer id, full untruncated content, and the tag list (the modal
/// renders `creator:*` tags in a header along with the timestamp). The
/// fields are deliberately a subset of the daemon's serialised `Drawer` —
/// the modal does not render importance, drawer_type, or room.
/// Test: `parse_memory_details_projects_full_content`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryDetail {
    /// Stable drawer identifier (UUID as string).
    pub id: String,
    /// Verbatim drawer body, exactly as returned by the daemon.
    pub content: String,
    /// All tags carried on the wire (creator, session, custom).
    pub tags: Vec<String>,
    /// Creation timestamp parsed from the wire payload, when present.
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Fallback creator label rendered when no recognised creator tag is found.
///
/// Why: the panel must distinguish "writer didn't self-identify" from a real
/// label; using an em-dash mirrors the convention used by the statistics
/// panel for "never written" timestamps.
/// What: the literal em-dash glyph.
/// Test: `creator_label_picks_first_match`.
pub const NO_CREATOR_LABEL: &str = "—";

/// Maximum characters retained when the client falls back to truncating
/// `content` because the daemon didn't return a `snippet` (issue #202).
///
/// Why: the activity panel must show a usable snippet against older
/// daemons that predate the wire field. Matching the server's
/// `DRAWER_SNIPPET_MAX_CHARS` keeps the rendered width consistent across
/// daemon versions.
/// What: 60 characters; matches `trusty_memory::service::DRAWER_SNIPPET_MAX_CHARS`.
/// Test: `parse_drawers_projects_fields`.
pub(super) const DRAWER_SNIPPET_FALLBACK_MAX: usize = 60;
