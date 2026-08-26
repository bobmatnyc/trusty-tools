//! Shared types and helpers for the trusty-memory monitor client.
//!
//! Why: the wire shapes, public domain types, and socket resolution are pure
//! data — splitting them here keeps `client.rs` and `parsers.rs` free of
//! constant / struct declarations.
//! What: constants, the socket-resolution re-export, wire structs
//! (`StatusWire`, `PalaceWire`), and the public projection types (`RecallHit`,
//! `DreamStats`, `MemoryEvent`, `DrawerInfo`, `MemoryDetail`).
//! Test: parsers are tested in `tests.rs`; type defaults/derives are implicitly
//! exercised by the parser tests.

use std::time::Duration;

use serde::Deserialize;

/// Per-call timeout for trusty-memory probes.
///
/// Why: a hung daemon must not stall the dashboard refresh tick.
/// What: three seconds, matching `search_client`.
/// Test: exercised implicitly by every `MemoryClient::new` call in tests.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve the socket the trusty-memory daemon serves on.
///
/// Why (#6286): there is one such resolution in the workspace and it lives in
/// [`crate::memory_rpc`]. This re-export keeps the monitor's call sites reading
/// `memory_client::resolve_memory_socket` the way they read
/// `search_client::resolve_search_url`, without a second copy of the rule.
/// Test: `resolve_memory_socket_names_the_daemon_socket`.
pub use crate::memory_rpc::resolve_memory_socket;

/// Wire shape of the `memory.status` result.
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

/// Wire shape of one palace, as `memory.palace_get` answers it.
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
    /// Whether the daemon had this palace's handle resident when it built the
    /// row — i.e. whether the counts above are measurements (#4682).
    ///
    /// Why: `Option`, not a plain `bool`, so the three states stay distinct.
    /// `Some(true)` = counts are live; `Some(false)` = counts are placeholder
    /// zeros (the normal case on the peek-based list route since #4640);
    /// `None` = the daemon predates the `cached` flag and always opened every
    /// palace, so its counts are authoritative. Defaulting the absent case to
    /// `false` would make a current client show `—` for every palace against
    /// an older daemon.
    /// Test: `parse_palace_detail_marks_uncached_rows_unknown`,
    /// `parse_palace_detail_trusts_counts_when_cached_flag_absent`.
    #[serde(default)]
    pub(super) cached: Option<bool>,
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

/// Aggregate counts returned by a `memory.dream_run` cycle.
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

/// One activity event from the trusty-memory daemon.
///
/// Why: the memory TUI shows dream cycles, drawer changes and palace creation
/// in its activity log; a typed enum lets the renderer format each distinctly
/// without parsing raw JSON in the event loop. The daemon pushed these over
/// `/sse` until #6286 retired that listener; `MemoryClient::recent_events`
/// polls the same bodies out of the activity log instead.
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
    /// Populated by the daemon's `memory.drawers_list` method
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
