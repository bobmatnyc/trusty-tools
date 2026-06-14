//! Data types and state for the unified monitor dashboard.
//!
//! Why: keeping the pure data model separate from format and render logic
//! makes the types independently testable and re-usable by tooling that
//! only needs to inspect state without drawing a frame.
//! What: constants, `Focus`, `IndexRow`, `SearchData`, `PalaceRow`,
//! `MemoryData`, `PanelStatus<T>`, `DaemonPanel<T>`, and `DashboardState`.
//! Test: `test_toggle_focus`, `test_new_state_starts_connecting`,
//! `test_panel_starts_connecting`, `test_panel_status_is_online`,
//! `test_reindex_target`, `test_index_row_project`, `test_palace_row_project`.

/// Terminal width (in columns) at or above which panels render side by side.
///
/// Why: a narrow terminal cannot fit two readable panels horizontally, so the
/// layout stacks them vertically below this threshold.
/// What: 120 columns, the spec's wide/narrow boundary.
/// Test: `test_layout_wide`, `test_layout_narrow`.
pub const WIDE_LAYOUT_MIN_COLS: u16 = 120;

/// One-line key hint shown in the header.
pub const KEY_HINT: &str = "[Tab] focus  [r] reindex  [q] quit  [?] help";

/// Crate version, surfaced in the dashboard title.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which daemon panel currently holds keyboard focus.
///
/// Why: `[Tab]` cycles focus; the focused panel gets a highlighted border and
/// `[r]` only acts on the search panel when it is focused.
/// What: `Search` (the default) or `Memory`.
/// Test: `test_toggle_focus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The trusty-search panel has focus.
    #[default]
    Search,
    /// The trusty-memory panel has focus.
    Memory,
}

/// One trusty-search index row rendered in the search panel's table.
///
/// Why: the search panel lists every registered index with its chunk count so
/// the operator can see corpus sizes at a glance; the TUI also uses
/// `disk_bytes` and `last_indexed` to drive its sort / stats panels and the
/// inferred project name (from `root_path`) to group rows. Graph stats and
/// community info give the operator visibility into the symbol graph and
/// detected community structure built by the daemon.
/// What: the index id, its chunk count, indexed root path, optional on-disk
/// size, optional last-indexed timestamp, plus knowledge-graph node/edge
/// counts, per-edge-kind breakdown (sorted desc by count), and community
/// count + modularity score from community detection.
/// Test: `test_search_panel_renders`, `test_index_row_project`,
/// `test_stats_lines_graph_section`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexRow {
    /// The index identifier.
    pub id: String,
    /// Number of indexed chunks.
    pub chunk_count: u64,
    /// Filesystem root the index covers.
    pub root_path: String,
    /// Approximate on-disk size of the index in bytes, when reported.
    pub disk_bytes: Option<u64>,
    /// The last time the index was (re)built, when reported.
    pub last_indexed: Option<chrono::DateTime<chrono::Utc>>,
    /// Knowledge-graph node count for the index (0 when no graph was built).
    ///
    /// Why: surfaces graph size in the STATISTICS panel.
    /// What: nodes from `/indexes/:id/graph/stats`.
    /// Test: `test_stats_lines_graph_section`.
    pub node_count: u64,
    /// Knowledge-graph edge count for the index.
    ///
    /// Why: paired with `node_count` to show graph density.
    /// What: edges from `/indexes/:id/graph/stats`.
    /// Test: `test_stats_lines_graph_section`.
    pub edge_count: u64,
    /// Per-edge-kind counts, sorted by count descending.
    ///
    /// Why: lets the operator see which relationship types dominate.
    /// What: vector of `(kind_name, count)` from `edge_kinds`.
    /// Test: `test_stats_lines_edge_kind_bars`.
    pub edge_kinds: Vec<(String, u64)>,
    /// Number of communities detected by community detection.
    ///
    /// Why: surfaces high-level cluster count in the STATISTICS panel.
    /// What: `community_count` from `/indexes/:id/communities` (0 when none).
    /// Test: `test_stats_lines_graph_section`.
    pub community_count: u64,
    /// Modularity score of the community partition (0.0 when unknown).
    ///
    /// Why: a quality signal for the detected community structure.
    /// What: `modularity` from `/indexes/:id/communities`.
    /// Test: `test_stats_lines_graph_section`.
    pub modularity: f64,
}

#[cfg(feature = "monitor-tui")]
impl crate::monitor::tui_common::ListItem for IndexRow {
    /// Why: navigation maps cursor ↔ id via this stable handle.
    /// What: returns `&self.id`.
    /// Test: covered through `tui_common::tests::test_visible_ids_and_navigation`.
    fn id(&self) -> &str {
        &self.id
    }
    /// Why: the search TUI filters and sorts by index id (its display name).
    /// What: returns `&self.id` (index rows have no separate name field).
    /// Test: covered through the search_tui filter / sort tests.
    fn name(&self) -> &str {
        &self.id
    }
    /// Why: grouping bucket keyed off the inferred project basename.
    /// What: delegates to [`IndexRow::project`].
    /// Test: covered through the search_tui grouping tests.
    fn project(&self) -> &str {
        self.project()
    }
    /// Why: the Activity sort key reads this timestamp; the trait surface
    /// keeps memory and search interchangeable.
    /// What: returns `self.last_indexed`.
    /// Test: covered through `test_apply_sort_activity` in search_tui.
    fn activity_ts(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.last_indexed
    }
    /// Why: the Count sort key reads this; for indexes that means chunks.
    /// What: returns `self.chunk_count`.
    /// Test: covered through `test_apply_sort_chunks` in search_tui.
    fn count(&self) -> u64 {
        self.chunk_count
    }
}

impl IndexRow {
    /// Infer the project this index belongs to.
    ///
    /// Why: indexing typically tracks one project per `root_path`; surfacing
    /// the basename lets the TUI group rows under their originating repo.
    /// What: extracts the basename of `root_path`, falling back to the index
    /// id when the path is empty or has no terminal segment.
    /// Test: `test_index_row_project`.
    pub fn project(&self) -> &str {
        std::path::Path::new(&self.root_path)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id)
    }
}

/// The polled trusty-search panel payload.
///
/// Why: the search panel renders aggregate health plus a per-index table; this
/// groups everything one poll produces.
/// What: the daemon version, uptime, and the index rows.
/// Test: `test_search_panel_renders`.
#[derive(Debug, Clone, Default)]
pub struct SearchData {
    /// The trusty-search daemon version string.
    pub version: String,
    /// Daemon uptime in whole seconds.
    pub uptime_secs: u64,
    /// One row per registered index.
    pub indexes: Vec<IndexRow>,
}

impl SearchData {
    /// Sum the chunk counts across every index.
    ///
    /// Why: the panel header shows a single "total chunks" figure.
    /// What: folds `chunk_count` over [`Self::indexes`].
    /// Test: `test_search_total_chunks`.
    pub fn total_chunks(&self) -> u64 {
        self.indexes.iter().map(|i| i.chunk_count).sum()
    }
}

/// One trusty-memory palace row rendered in the memory panel's table.
///
/// Why: the memory panel lists every palace with its vector count plus the
/// metadata the TUI needs to filter, sort, and group by project.
/// What: the palace id, friendly name, vector and drawer counts, the last
/// write timestamp (when reported), and the auto-registration description
/// string used to infer the originating project.
/// Test: `test_memory_panel_renders`, `test_palace_row_project`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PalaceRow {
    /// The palace identifier.
    pub id: String,
    /// The palace's human-readable name.
    pub name: String,
    /// Number of stored vectors in the palace.
    pub vector_count: u64,
    /// Number of drawers in the palace (from `PalaceInfo`).
    pub drawer_count: u64,
    /// The last write timestamp, when reported by the daemon.
    pub last_write_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The palace description; used to infer the originating project.
    pub description: Option<String>,
    /// Number of active KG triples in the palace (0 when no handle).
    ///
    /// Why: surfaces graph activity in the STATISTICS panel and lets the empty-
    /// palace filter keep palaces that have only KG data and no vectors.
    /// What: `kg_triple_count` from `/api/v1/palaces`.
    /// Test: `test_stats_graph_section`, `test_filter_empty_palaces`.
    pub kg_triple_count: u64,
    /// Distinct-entity (node) count in the KG (0 when no handle).
    ///
    /// Why: shown in the STATISTICS Knowledge Graph section.
    /// What: `node_count` from `/api/v1/palaces`.
    /// Test: `test_stats_graph_section`.
    pub node_count: u64,
    /// Directed-edge count in the KG (0 when no handle).
    ///
    /// Why: shown in the STATISTICS Knowledge Graph section.
    /// What: `edge_count` from `/api/v1/palaces`.
    /// Test: `test_stats_graph_section`.
    pub edge_count: u64,
    /// Number of Louvain communities detected in the KG (0 when no handle).
    ///
    /// Why: shown in the STATISTICS Knowledge Graph section.
    /// What: `community_count` from `/api/v1/palaces`.
    /// Test: `test_stats_graph_section`.
    pub community_count: u64,
    /// Whether a dream/compaction cycle is currently running against the palace.
    ///
    /// Why: drives the "Dreaming" state in the active-palace indicators.
    /// What: `is_compacting` from `/api/v1/palaces`.
    /// Test: `test_palace_activity_state`.
    pub is_compacting: bool,
}

#[cfg(feature = "monitor-tui")]
impl crate::monitor::tui_common::ListItem for PalaceRow {
    /// Why: navigation maps cursor ↔ id via this stable handle.
    /// What: returns `&self.id`.
    /// Test: covered through `tui_common::tests::test_visible_ids_and_navigation`.
    fn id(&self) -> &str {
        &self.id
    }
    /// Why: filter / sort by display name; the memory TUI prefers `name`.
    /// What: returns `&self.name`.
    /// Test: covered through the memory_tui filter / sort tests.
    fn name(&self) -> &str {
        &self.name
    }
    /// Why: grouping bucket keyed off the inferred project basename.
    /// What: delegates to [`PalaceRow::project`].
    /// Test: covered through the memory_tui grouping tests.
    fn project(&self) -> &str {
        self.project()
    }
    /// Why: the Activity sort key reads this timestamp.
    /// What: returns `self.last_write_at`.
    /// Test: covered through `test_apply_sort_activity` in memory_tui.
    fn activity_ts(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.last_write_at
    }
    /// Why: the Count sort key reads this; for palaces that means vectors.
    /// What: returns `self.vector_count`.
    /// Test: covered through `test_apply_sort_vectors` in memory_tui.
    fn count(&self) -> u64 {
        self.vector_count
    }
}

impl PalaceRow {
    /// Infer the project this palace belongs to.
    ///
    /// Why: project name is encoded in the auto-registered description path,
    /// so the TUI can group palaces by their originating repo.
    /// What: extracts the basename of the path in
    /// `"Auto-registered from <path>"`, falling back to the palace name when
    /// the description does not match the expected prefix.
    /// Test: `test_palace_row_project`.
    pub fn project(&self) -> &str {
        self.description
            .as_deref()
            .and_then(|d| d.strip_prefix("Auto-registered from "))
            .and_then(|p| p.rsplit('/').next())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.name)
    }
}

/// The polled trusty-memory panel payload.
///
/// Why: the memory panel renders aggregate counts plus a per-palace table; this
/// groups everything one poll produces.
/// What: the daemon version, the aggregate counts, and the palace rows.
/// Test: `test_memory_panel_renders`.
#[derive(Debug, Clone, Default)]
pub struct MemoryData {
    /// The trusty-memory daemon version string.
    pub version: String,
    /// Number of palaces.
    pub palace_count: u64,
    /// Total drawers across all palaces.
    pub total_drawers: u64,
    /// Total stored vectors across all palaces.
    pub total_vectors: u64,
    /// Total knowledge-graph triples across all palaces.
    pub total_kg_triples: u64,
    /// One row per palace.
    pub palaces: Vec<PalaceRow>,
}

/// The connection state of one daemon panel.
///
/// Why: each panel must render distinctly whether it is still connecting, has a
/// fresh payload, or is offline with a captured error; a typed enum keeps that
/// rendering exhaustive.
/// What: `Connecting` before the first poll, `Online(T)` with a payload, or
/// `Offline` with the last error string.
/// Test: `test_offline_panel_renders`, `test_search_panel_renders`.
#[derive(Debug, Clone)]
pub enum PanelStatus<T> {
    /// The first poll has not completed yet.
    Connecting,
    /// The daemon answered; `T` is the latest payload.
    Online(T),
    /// The daemon is unreachable; carries the last error message.
    Offline {
        /// The error captured from the most recent failed poll.
        last_error: String,
    },
}

impl<T> PanelStatus<T> {
    /// Whether this panel is currently online.
    ///
    /// Why: the header badge and the focus-dependent `[r]` action both branch
    /// on reachability.
    /// What: returns `true` only for [`PanelStatus::Online`].
    /// Test: `test_panel_status_is_online`.
    pub fn is_online(&self) -> bool {
        matches!(self, PanelStatus::Online(_))
    }
}

/// One daemon's panel: its connection status and the URL it targets.
///
/// Why: the search and memory panels are structurally identical — a status and
/// a base URL — so a generic struct removes the duplication.
/// What: a [`PanelStatus`] payload plus the daemon base URL the poller probes.
/// Test: `test_offline_panel_renders`.
#[derive(Debug, Clone)]
pub struct DaemonPanel<T> {
    /// The panel's connection status and latest payload.
    pub status: PanelStatus<T>,
    /// The daemon base URL this panel polls.
    pub base_url: String,
}

impl<T> DaemonPanel<T> {
    /// Build a panel that starts in the `Connecting` state.
    ///
    /// Why: before the first poll completes the panel has no payload yet.
    /// What: stores `base_url` and sets the status to [`PanelStatus::Connecting`].
    /// Test: `test_panel_starts_connecting`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            status: PanelStatus::Connecting,
            base_url: base_url.into(),
        }
    }
}

/// Snapshot of everything the dashboard renders this frame.
///
/// Why: the event loop polls both daemons, fills this struct, and hands it to
/// [`super::render::render`] — a clean data/render split that keeps the loop terse.
/// What: a [`DaemonPanel`] per daemon plus focus and help-overlay flags.
/// Test: `test_toggle_focus`, `test_layout_wide`, `test_offline_panel_renders`.
#[derive(Debug, Clone)]
pub struct DashboardState {
    /// The trusty-search panel.
    pub search: DaemonPanel<SearchData>,
    /// The trusty-memory panel.
    pub memory: DaemonPanel<MemoryData>,
    /// Which panel currently holds keyboard focus.
    pub focus: Focus,
    /// Whether the help overlay is visible (toggled with `?`).
    pub show_help: bool,
    /// Human-readable result of the last action, shown in the header.
    pub last_action: Option<String>,
}

impl DashboardState {
    /// Build a dashboard targeting the two given daemon URLs.
    ///
    /// Why: the event loop resolves both daemon addresses at startup and seeds
    /// the panels with them; both start in `Connecting` until the first poll.
    /// What: constructs both [`DaemonPanel`]s and defaults focus to the search
    /// panel with the help overlay hidden.
    /// Test: `test_new_state_starts_connecting`.
    pub fn new(search_url: impl Into<String>, memory_url: impl Into<String>) -> Self {
        Self {
            search: DaemonPanel::new(search_url),
            memory: DaemonPanel::new(memory_url),
            focus: Focus::Search,
            show_help: false,
            last_action: None,
        }
    }

    /// Cycle keyboard focus between the search and memory panels (`[Tab]`).
    ///
    /// Why: `[Tab]` moves the highlighted border and decides which panel `[r]`
    /// acts on.
    /// What: flips [`Self::focus`].
    /// Test: `test_toggle_focus`.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Search => Focus::Memory,
            Focus::Memory => Focus::Search,
        };
    }

    /// The id of the first index in the focused search panel, if any.
    ///
    /// Why: `[r]` reindexes a search index; without a richer selection model
    /// the dashboard targets the first index of an online, focused search panel.
    /// What: returns `Some(id)` only when the search panel is focused, online,
    /// and has at least one index.
    /// Test: `test_reindex_target`.
    pub fn reindex_target(&self) -> Option<String> {
        if self.focus != Focus::Search {
            return None;
        }
        match &self.search.status {
            PanelStatus::Online(data) => data.indexes.first().map(|i| i.id.clone()),
            _ => None,
        }
    }
}
