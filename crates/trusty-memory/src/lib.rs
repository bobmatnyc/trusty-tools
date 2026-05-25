//! MCP server (stdio + HTTP/SSE) for trusty-memory.
//!
//! Why: Claude Code and other MCP-aware clients integrate with trusty-memory
//! through the standardized Model Context Protocol; we expose memory + KG
//! tools so they can be called by name.
//! What: Provides `run_stdio` (JSON-RPC 2.0 over stdin/stdout) and `run_http`
//! (axum HTTP/SSE stub), plus an `AppState` that carries the shared
//! `PalaceRegistry`, on-disk data root, and a lazily-initialized embedder.
//! Test: `cargo test -p trusty-memory-mcp` validates handshake + dispatch.

use anyhow::Result;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::{broadcast, OnceCell};
use tracing::info;
use trusty_common::mcp::{error_codes, initialize_response, Request, Response};
use trusty_common::memory_core::embed::FastEmbedder;
use trusty_common::memory_core::store::ChatSessionStore;
use trusty_common::memory_core::PalaceRegistry;
use trusty_common::ChatProvider;

pub mod activity;
pub mod bootstrap;
pub mod commands;
pub mod discovery;
pub mod kg_extract;
pub mod openrpc;
pub mod prompt_facts;
pub mod service;
pub mod tools;
pub mod web;

pub use activity::{ActivityEntry, ActivityFilter, ActivityLog, ActivitySource};

pub use service::MemoryMcpService;
pub use tools::MemoryMcpServer;

/// Resolve the directory that actually holds the per-palace subdirectories.
///
/// Why: there are two on-disk layouts in the wild. The current monorepo code
/// treats the registry directory *itself* as the parent of per-palace dirs
/// (`<dir>/<id>/palace.json`). The legacy standalone `trusty-memory` repo
/// nested everything one level deeper under a `palaces/` subdirectory
/// (`<data_dir>/palaces/<id>/palace.json`) — and that is where existing
/// installs' data lives (e.g. 88 palaces under
/// `~/Library/Application Support/trusty-memory/palaces/`). A daemon that uses
/// the bare data dir as its registry root finds zero palaces because every
/// `palace.json` sits one level below where it looked — the "palaces lost on
/// restart" bug.
/// What: given the standard data dir, returns `<data_dir>/palaces` when that
/// subdirectory exists, otherwise `<data_dir>` itself. Resolving this once in
/// `main.rs` and using the result as `AppState::data_root` keeps every call
/// site (`status`, `palace_list`, `open_palace`, `palace_create`,
/// `load_palaces_from_disk`) consistent without forcing a data migration.
/// Test: `tests::resolve_palace_registry_dir_prefers_palaces_subdir` and
/// `resolve_palace_registry_dir_falls_back_to_data_dir`.
pub fn resolve_palace_registry_dir(data_dir: PathBuf) -> PathBuf {
    let nested = data_dir.join("palaces");
    if nested.is_dir() {
        nested
    } else {
        data_dir
    }
}

/// Live daemon events broadcast to connected SSE subscribers.
///
/// Why: The dashboard needs push-driven updates so palace creation, drawer
/// add/delete, dream cycles, and aggregate status changes are visible without
/// polling. A single broadcast channel fans out to every connected browser.
/// What: Tagged enum serialized as `{"type": "...", ...fields}` over SSE.
/// Test: `web::tests::sse_stream_emits_events` subscribes, triggers a
/// mutation, and asserts the frame arrives.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent {
    PalaceCreated {
        id: String,
        name: String,
        /// Originating subsystem (HTTP, MCP, Hook). Why (issue #96): the
        /// UI badges each row with its source so operators can tell at a
        /// glance whether a write came from the dashboard form, an MCP
        /// tool call, or a hook-driven path. The wire-format key is
        /// `source` (lower-case strings via serde rename_all on
        /// `ActivitySource`).
        source: ActivitySource,
    },
    DrawerAdded {
        palace_id: String,
        /// Friendly palace name (Palace.name) at write time. Why: lets SSE
        /// consumers (the dashboard activity feed) render the human-readable
        /// label without a separate id→name lookup. Empty string if the
        /// emitter could not resolve the name.
        #[serde(default)]
        palace_name: String,
        drawer_count: usize,
        /// Wall-clock timestamp when the drawer was added. Why: SSE
        /// receivers want to render "just now / 2m ago" relative to the
        /// daemon's clock, not the time the SSE frame happens to arrive.
        timestamp: chrono::DateTime<chrono::Utc>,
        /// Short preview of the drawer's content (whitespace-collapsed,
        /// truncated to ~80 chars with an ellipsis when cut). Why: the TUI
        /// activity feed and dashboard ticker want to show *what* was
        /// stored, not just the running drawer count. Empty when the
        /// emitter could not resolve the content (legacy clients tolerate
        /// the missing field via `#[serde(default)]`).
        #[serde(default)]
        content_preview: String,
        /// Originating subsystem (issue #96).
        source: ActivitySource,
    },
    DrawerDeleted {
        palace_id: String,
        drawer_count: usize,
        /// Originating subsystem (issue #96).
        source: ActivitySource,
    },
    DreamCompleted {
        palace_id: Option<String>,
        merged: usize,
        pruned: usize,
        compacted: usize,
        closets_updated: usize,
        duration_ms: u64,
        /// Originating subsystem (issue #96).
        source: ActivitySource,
    },
    StatusChanged {
        total_drawers: usize,
        total_vectors: usize,
        total_kg_triples: usize,
    },
}

/// Open the activity log under `data_root`, falling back to a per-process
/// tempdir when the primary location is unwritable.
///
/// Why (issue #96): the activity log is a best-effort feature — if the data
/// root is on a read-only mount, missing, or locked by another process, the
/// daemon should still come up and serve every other endpoint. Falling back
/// to a `std::env::temp_dir()`-anchored subdirectory keyed by the daemon's
/// process id keeps the log available for the lifetime of the daemon while
/// guaranteeing a unique writable directory.
/// What: tries `ActivityLog::open(data_root)`; on error logs a warning and
/// retries against `<temp>/trusty-memory-activity-<pid>/`. The final
/// fallback `expect` is only reachable when even `temp_dir()` is broken,
/// which is a programmer-error class invariant.
/// Test: covered indirectly by `app_state_default_constructs` and every
/// test that builds an `AppState` against a tempdir.
fn open_activity_log_with_fallback(data_root: &Path) -> Arc<ActivityLog> {
    match ActivityLog::open(data_root) {
        Ok(log) => Arc::new(log),
        Err(e) => {
            tracing::warn!(
                "could not open activity log at {}: {e:#}; falling back to per-process tempdir",
                data_root.display()
            );
            let fallback =
                std::env::temp_dir().join(format!("trusty-memory-activity-{}", std::process::id()));
            Arc::new(
                ActivityLog::open(&fallback)
                    .expect("activity fallback log must open in a fresh tempdir"),
            )
        }
    }
}

impl DaemonEvent {
    /// Short discriminant label matching the SSE `type` field.
    ///
    /// Why: the persisted activity log stores `event_type` as a string so
    /// the UI can render the row without re-parsing the payload. Sharing
    /// the same labels the SSE serializer uses keeps the wire and the
    /// stored history consistent.
    /// What: returns one of `palace_created`, `drawer_added`,
    /// `drawer_deleted`, `dream_completed`, `status_changed`.
    /// Test: `daemon_event_type_str_matches_sse_tag` in the lib tests.
    pub fn type_str(&self) -> &'static str {
        match self {
            Self::PalaceCreated { .. } => "palace_created",
            Self::DrawerAdded { .. } => "drawer_added",
            Self::DrawerDeleted { .. } => "drawer_deleted",
            Self::DreamCompleted { .. } => "dream_completed",
            Self::StatusChanged { .. } => "status_changed",
        }
    }

    /// `palace_id` if the event is scoped to a single palace.
    ///
    /// Why: the activity log indexes entries by palace id so the UI can
    /// filter by palace; daemon-wide events (`status_changed`,
    /// dream-across-all-palaces) return `None`.
    /// What: returns a borrowed string when the variant carries a palace
    /// id, otherwise `None`.
    /// Test: `daemon_event_palace_id_extraction`.
    pub fn palace_id(&self) -> Option<&str> {
        match self {
            Self::PalaceCreated { id, .. } => Some(id),
            Self::DrawerAdded { palace_id, .. } | Self::DrawerDeleted { palace_id, .. } => {
                Some(palace_id)
            }
            Self::DreamCompleted { palace_id, .. } => palace_id.as_deref(),
            Self::StatusChanged { .. } => None,
        }
    }

    /// Originating subsystem if the event carries one.
    ///
    /// Why: only mutation events carry a `source`; the aggregate
    /// `StatusChanged` is recomputed by the daemon and has no caller, so
    /// it returns `None`.
    /// What: returns the variant's `source` field where present.
    /// Test: `daemon_event_source_extraction`.
    pub fn source(&self) -> Option<ActivitySource> {
        match self {
            Self::PalaceCreated { source, .. }
            | Self::DrawerAdded { source, .. }
            | Self::DrawerDeleted { source, .. }
            | Self::DreamCompleted { source, .. } => Some(*source),
            Self::StatusChanged { .. } => None,
        }
    }
}

/// Shared application state passed to every request handler.
///
/// Why: The stdio loop and HTTP server need the same handles to the registry,
/// data root, and embedder so MCP tools can perform real reads/writes against
/// the live trusty-memory core. The embedder is heavy (loads ONNX weights) so
/// we hold it behind a `OnceCell` and initialize lazily on first use.
/// What: `Clone`-able via `Arc` fields. The registry / data root are eager;
/// `embedder` is `Arc<OnceCell<Arc<FastEmbedder>>>` so concurrent first-use
/// races resolve to a single shared instance.
/// Test: `app_state_default_constructs` confirms construction without panic.
#[derive(Clone)]
pub struct AppState {
    pub version: String,
    pub registry: Arc<PalaceRegistry>,
    pub data_root: PathBuf,
    pub embedder: Arc<OnceCell<Arc<FastEmbedder>>>,
    /// Optional default palace applied to MCP tool calls when the caller
    /// omits the `palace` argument. Set via `trusty-memory serve --palace`.
    pub default_palace: Option<String>,
    /// Active chat provider selected at startup. `None` means no upstream is
    /// configured (no Ollama detected and no OpenRouter key) — callers must
    /// degrade gracefully (chat endpoint returns 412).
    pub chat_provider: Arc<OnceCell<Option<Arc<dyn ChatProvider>>>>,
    /// Per-palace chat-session stores, opened lazily so cold-start cost is
    /// paid only when chat-history endpoints are hit.
    pub session_stores: Arc<dashmap::DashMap<String, Arc<ChatSessionStore>>>,
    /// Broadcast sender for live `DaemonEvent` pushes to SSE subscribers.
    ///
    /// Why: Lets mutating handlers emit events that any connected dashboard
    /// receives instantly. Cap of 128 buffers transient slow readers; if a
    /// receiver lags it gets `RecvError::Lagged` and we emit a `lag` frame.
    pub events: Arc<broadcast::Sender<DaemonEvent>>,
    /// Instant the daemon started, used to compute `uptime_secs` on `/health`.
    ///
    /// Why (issue #35): `GET /health` reports how long the daemon has been
    /// up. Capturing a monotonic `Instant` at `AppState` construction lets the
    /// handler compute the elapsed seconds cheaply and without a clock-skew
    /// hazard.
    /// What: a wall-monotonic `Instant`; `AppState::new` stamps it at startup.
    /// Test: `health_endpoint_includes_resource_fields`.
    pub started_at: std::time::Instant,
    /// In-memory ring buffer of recent tracing log lines (issue #35).
    ///
    /// Why: the `GET /api/v1/logs/tail` endpoint serves the last N log lines
    /// so operators can inspect a running daemon without tailing a file. The
    /// buffer is shared between the tracing `LogBufferLayer` (writer) and the
    /// HTTP handler (reader).
    /// What: a cheap `Arc`-backed clone of the buffer the subscriber writes
    /// to. Defaults to an empty buffer for states that never install the
    /// layer (tests, the stdio path).
    /// Test: `logs_tail_returns_recent_lines`.
    pub log_buffer: trusty_common::log_buffer::LogBuffer,
    /// Most recent on-disk footprint of `data_root`, in bytes (issue #35).
    ///
    /// Why: `GET /health` reports `disk_bytes`. Walking the data directory on
    /// every health request would make a frequent health poll do unbounded
    /// I/O; a background task recomputes it every 10 s and stores it here so
    /// the handler reads it lock-free.
    /// What: an `AtomicU64` updated by the ticker spawned in `run_http_on`.
    /// `0` until the first walk completes.
    /// Test: `health_endpoint_includes_resource_fields`.
    pub disk_bytes: Arc<std::sync::atomic::AtomicU64>,
    /// Per-process RSS + CPU sampler, refreshed on each `/health` request
    /// (issue #35).
    ///
    /// Why: CPU usage is a delta between two `sysinfo` refreshes, so the
    /// sampler must persist between requests — hence the shared `Mutex`.
    /// What: a `tokio::sync::Mutex<SysMetrics>` so the async health handler
    /// can sample without blocking the runtime.
    /// Test: `health_endpoint_includes_resource_fields`.
    pub sys_metrics: Arc<tokio::sync::Mutex<trusty_common::sys_metrics::SysMetrics>>,
    /// HTTP listener address the daemon bound to, once `run_http_on` is running.
    ///
    /// Why: clients (and `/health` responses) need to advertise the live
    /// `host:port` even though port selection happens dynamically (7070–7079
    /// walk + OS fallback). Stashing it on `AppState` lets request handlers
    /// surface the discovery value without re-querying the listener.
    /// What: a `OnceLock<SocketAddr>` so `run_http_on` writes it exactly once
    /// at bind time and every handler reads it lock-free thereafter. Empty
    /// (`None` from `get()`) on the stdio path where no listener exists.
    /// Test: `health_endpoint_reports_bound_addr` (added below).
    pub bound_addr: Arc<OnceLock<SocketAddr>>,
    /// Cached prompt-facts surface served by the MCP `get_prompt_context`
    /// tool (issue #42).
    ///
    /// Why: The original session-init `prompts/get` design loaded context
    /// once per connection; switching to a per-message tool lets the model
    /// pull fresh, query-filtered context on demand. The cache holds both
    /// the raw triples (for filtered lookups) and a pre-formatted Markdown
    /// block (for the unfiltered hot path) so neither code path re-walks
    /// the KG. The cache is rebuilt by
    /// `prompt_facts::rebuild_prompt_cache` after any write that touches a
    /// hot predicate (`kg_assert`, `add_alias`, `remove_prompt_fact`).
    /// What: An `Arc<RwLock<PromptFactsCache>>` so the hot read path takes
    /// a brief read lock and clones the cache; rebuilds take a write lock
    /// for the assignment only. An empty `triples` vec ↔ "no context
    /// stored yet" (the tool handler renders a hint).
    /// Test: `get_prompt_context_returns_cached_or_hint`,
    /// `get_prompt_context_filters_by_query`.
    pub prompt_context_cache: Arc<RwLock<prompt_facts::PromptFactsCache>>,
    /// Persistent activity log (issue #96).
    ///
    /// Why: the dashboard activity feed used to be a pure live-stream over
    /// `/sse` — opening the UI showed an empty feed and any mutation from
    /// the MCP path was invisible. Holding an `ActivityLog` on `AppState`
    /// lets `emit` record an entry on every push so the
    /// `GET /api/v1/activity` handler can return historical rows on mount
    /// and the live SSE stream can continue prepending events on top of
    /// the loaded history. `None` on builds that opt out (tests that use
    /// `AppState::new` get a real log under their tempdir so behaviour
    /// matches production).
    /// What: an `Arc<ActivityLog>` shared with every emitter.
    /// Test: `web::tests::activity_endpoint_lists_recent_emits`.
    pub activity_log: Arc<ActivityLog>,
}

impl AppState {
    /// Construct an `AppState` rooted at the given on-disk data directory.
    ///
    /// Why: The CLI (`serve`) and integration tests need to point the MCP
    /// server at different roots — production at `dirs::data_dir`, tests at a
    /// `tempfile::tempdir()`.
    /// What: Builds an empty `PalaceRegistry`, captures the version, and
    /// allocates an empty `OnceCell` for the embedder. `default_palace` is
    /// `None`; use `with_default_palace` to set it.
    /// Test: `tools::tests::dispatch_palace_create_persists` constructs an
    /// AppState pointed at a tempdir and round-trips a palace through it.
    pub fn new(data_root: PathBuf) -> Self {
        let (events_tx, _) = broadcast::channel::<DaemonEvent>(128);
        // Issue #96: open (or create) the persistent activity log under the
        // daemon data root. Open failure is logged but never crashes the
        // daemon — we fall back to a per-process tempdir so emits remain
        // best-effort and the rest of the daemon keeps working.
        let activity_log = open_activity_log_with_fallback(&data_root);
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            registry: Arc::new(PalaceRegistry::new()),
            data_root,
            embedder: Arc::new(OnceCell::new()),
            default_palace: None,
            chat_provider: Arc::new(OnceCell::new()),
            session_stores: Arc::new(dashmap::DashMap::new()),
            events: Arc::new(events_tx),
            started_at: std::time::Instant::now(),
            // Default to an empty buffer — `with_log_buffer` overrides this
            // when the daemon installs the `LogBufferLayer` (HTTP mode).
            log_buffer: trusty_common::log_buffer::LogBuffer::new(
                trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
            ),
            disk_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sys_metrics: Arc::new(tokio::sync::Mutex::new(
                trusty_common::sys_metrics::SysMetrics::new(),
            )),
            bound_addr: Arc::new(OnceLock::new()),
            prompt_context_cache: Arc::new(RwLock::new(prompt_facts::PromptFactsCache::default())),
            activity_log,
        }
    }

    /// Scan the palace registry directory and re-register every persisted
    /// palace into the in-memory [`PalaceRegistry`].
    ///
    /// Why: `AppState::new` builds an *empty* registry, so after a daemon
    /// restart `palace_list` / the dashboard reported zero palaces even though
    /// dozens existed on disk — palace metadata was persisted by
    /// `palace_create` but never re-hydrated on startup. This method closes
    /// that gap by walking the on-disk layout (each subdirectory holding a
    /// `palace.json` is one palace) and rebuilding a live `PalaceHandle` for
    /// each, so recall paths see the full set immediately after a restart.
    /// What: runs the blocking filesystem walk + per-palace `PalaceHandle::open`
    /// on a `spawn_blocking` thread (so it never stalls the async runtime),
    /// registers each successfully opened palace via `register_arc`, logs every
    /// load at `debug!`, and returns the count loaded. A palace that fails to
    /// open (corrupt index, unreadable `kg.db`, etc.) is logged at `warn!` and
    /// skipped — one bad palace must not abort startup or crash the daemon.
    /// `data_root` is expected to already be the palace registry directory —
    /// `main.rs` resolves it via [`resolve_palace_registry_dir`] before
    /// constructing the `AppState`, so the flat / legacy-`palaces/` layout
    /// difference is handled exactly once.
    /// Test: `tests::load_palaces_from_disk_rehydrates_registry` writes two
    /// palaces into a tempdir, constructs an `AppState`, calls this method, and
    /// asserts the returned count and registry contents.
    pub async fn load_palaces_from_disk(&self) -> Result<usize> {
        let registry_dir = self.data_root.clone();
        let registry = self.registry.clone();
        // The directory walk and each `PalaceHandle::open` perform blocking
        // filesystem + redb/usearch I/O — run the whole hydration on the
        // blocking pool so it never parks an async worker thread.
        let count = tokio::task::spawn_blocking(move || -> Result<usize> {
            let palaces = PalaceRegistry::list_palaces(&registry_dir)?;
            let total = palaces.len();
            let mut loaded = 0usize;
            let mut skipped = 0usize;
            for palace in palaces {
                match trusty_common::memory_core::PalaceHandle::open(&palace) {
                    Ok(handle) => {
                        tracing::debug!(
                            palace = %palace.id,
                            data_dir = %palace.data_dir.display(),
                            "loaded palace from disk"
                        );
                        registry.register_arc(handle);
                        loaded += 1;
                    }
                    Err(e) => {
                        // Why: a single bad palace (corrupt kg.db, stale WAL,
                        // permissions) must never abort startup or block the
                        // HTTP server from binding. Log per-palace and keep
                        // going; the summary below tells operators how many
                        // were skipped without trawling the log.
                        tracing::warn!(
                            palace = %palace.id,
                            data_dir = %palace.data_dir.display(),
                            "skipping palace during startup hydration: {e:#}"
                        );
                        skipped += 1;
                    }
                }
            }
            tracing::info!(
                "palace hydration summary: loaded {loaded}/{total} ({skipped} skipped due to errors)"
            );
            Ok(loaded)
        })
        .await
        .map_err(|e| anyhow::anyhow!("join load_palaces_from_disk: {e}"))??;
        Ok(count)
    }

    /// Builder-style: attach the daemon's shared [`LogBuffer`] so the
    /// `GET /api/v1/logs/tail` endpoint serves the same lines the tracing
    /// subscriber captures (issue #35).
    ///
    /// Why: `main` builds the buffer (via `init_tracing_with_buffer`) before
    /// constructing the `AppState`, then hands a clone here so the HTTP
    /// handler and the tracing layer observe the same ring.
    /// What: replaces the empty default buffer with the supplied one.
    /// Test: `logs_tail_returns_recent_lines`.
    #[must_use]
    pub fn with_log_buffer(mut self, buffer: trusty_common::log_buffer::LogBuffer) -> Self {
        self.log_buffer = buffer;
        self
    }

    /// Send a `DaemonEvent` to all connected SSE subscribers and persist
    /// it to the activity log when the variant carries a source.
    ///
    /// Why: Mutating handlers call this after a successful write so the
    /// dashboard can update without polling. The send is best-effort —
    /// `broadcast::Sender::send` returns `Err` only when there are no live
    /// receivers, which is fine (no listeners == no work to do). Issue
    /// #96 additionally writes the entry to the persistent activity log
    /// so the feed can serve historical rows on page load and so MCP /
    /// HTTP / Hook origins are visible to the operator. Persistence is
    /// also best-effort — a write failure is logged but never blocks the
    /// SSE broadcast.
    /// What: serialises the event for the log (skipping `StatusChanged`
    /// which is a recomputed aggregate, not a mutation), then sends it
    /// over the broadcast channel.
    /// Test: `web::tests::sse_stream_receives_palace_created` confirms a
    /// subscriber observes the emitted event;
    /// `activity_endpoint_lists_recent_emits` confirms persistence.
    pub fn emit(&self, event: DaemonEvent) {
        if let Some(source) = event.source() {
            let event_type = event.type_str();
            let palace_id = event.palace_id().map(|s| s.to_string());
            if let Err(e) = self
                .activity_log
                .append(source, palace_id, event_type, &event)
            {
                tracing::warn!("activity_log.append failed for {event_type}: {e:#}");
            }
        }
        let _ = self.events.send(event);
    }

    /// Open (or return cached) the chat-session store for a palace.
    ///
    /// Why: Chat session persistence lives in a dedicated SQLite file under
    /// the palace's data dir (`chat_sessions.db`) so it doesn't intermingle
    /// with the KG's transactional load. The store is cheap to clone via
    /// `Arc` but the underlying r2d2 pool should be reused, so cache by id.
    /// What: Creates the palace data dir if missing, opens (or reuses) a
    /// `ChatSessionStore` and stashes an `Arc` in the DashMap.
    /// Test: Indirectly via the session HTTP handlers in `web::tests`.
    pub fn session_store(&self, palace_id: &str) -> Result<Arc<ChatSessionStore>> {
        if let Some(entry) = self.session_stores.get(palace_id) {
            return Ok(entry.clone());
        }
        let dir = self.data_root.join(palace_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("create palace dir {}: {e}", dir.display()))?;
        let store = Arc::new(ChatSessionStore::open(&dir.join("chat_sessions.db"))?);
        self.session_stores
            .insert(palace_id.to_string(), store.clone());
        Ok(store)
    }

    /// Builder-style setter for the default palace name.
    ///
    /// Why: `serve --palace <name>` wants to bind every tool call to a
    /// project-scoped namespace without forcing every MCP request to repeat
    /// the palace argument.
    /// What: Returns `self` with `default_palace = Some(name)`.
    /// Test: `default_palace_used_when_arg_omitted` covers the resolution
    /// path; this setter is exercised there.
    pub fn with_default_palace(mut self, name: Option<String>) -> Self {
        self.default_palace = name;
        self
    }

    /// Resolve (or initialize) the shared embedder.
    ///
    /// Why: FastEmbedder load is expensive — we share one instance across all
    /// tool calls; the `OnceCell` ensures concurrent first-use races collapse
    /// to a single load.
    /// What: Returns `Arc<FastEmbedder>` on success. Errors propagate from the
    /// underlying ONNX load.
    /// Test: Indirectly via `dispatch_remember_then_recall`.
    /// Resolve the active chat provider, auto-detecting on first call.
    ///
    /// Why: Provider selection depends on filesystem-loaded config plus a
    /// network probe (Ollama liveness), so it must be lazily initialised at
    /// runtime. Caching the choice in a `OnceCell` keeps it stable across
    /// concurrent requests without re-probing on every chat call.
    /// What: On first use loads `~/.trusty-memory/config.toml`, prefers an
    /// auto-detected Ollama instance (when `local_model.enabled`), and falls
    /// back to OpenRouter when an API key is set. Returns `Ok(None)` when
    /// neither is available so the caller can emit a 412.
    /// Test: `web::tests::providers_endpoint_returns_payload` covers the
    /// detection path indirectly through `/api/v1/chat/providers`.
    pub async fn chat_provider(&self) -> Option<Arc<dyn ChatProvider>> {
        self.chat_provider
            .get_or_init(|| async {
                let cfg = crate::web::load_user_config().unwrap_or_default();
                if cfg.local_model.enabled {
                    if let Some(mut p) =
                        trusty_common::auto_detect_local_provider(&cfg.local_model.base_url).await
                    {
                        // auto_detect returns an empty model id; callers must
                        // set the configured model name themselves.
                        p.model = cfg.local_model.model.clone();
                        return Some(Arc::new(p) as Arc<dyn ChatProvider>);
                    }
                }
                if !cfg.openrouter_api_key.is_empty() {
                    return Some(Arc::new(trusty_common::OpenRouterProvider::new(
                        cfg.openrouter_api_key,
                        cfg.openrouter_model,
                    )) as Arc<dyn ChatProvider>);
                }
                None
            })
            .await
            .clone()
    }

    /// Spawn a fire-and-forget background task that auto-discovers project
    /// aliases under `project_root` and asserts new ones into `palace`.
    ///
    /// Why (issue #42): Projects carry implicit shorthand — cargo package
    /// names that differ from their directory, binary names that differ
    /// from packages, first-letter abbreviations — that should be surfaced
    /// without a user ever calling `add_alias`. Running discovery as a
    /// detached task on palace-open keeps startup latency unchanged: the
    /// daemon binds and starts serving immediately while the discovery scan
    /// completes in the background, and any newly-asserted aliases land in
    /// the prompt cache before the model's next `get_prompt_context` call.
    /// What: clones `self` (cheap; `Arc`-backed), spawns a tokio task that
    /// invokes the `discover_aliases` tool handler directly so the
    /// dedup + cache-rebuild logic runs exactly the same path as the MCP
    /// tool call. Errors are logged at `warn!`; one failed discovery never
    /// destabilises the daemon.
    /// Test: not unit-tested (timing-dependent fire-and-forget); the
    /// underlying `discover_aliases` dispatch is covered by
    /// `dispatch_discover_aliases_inserts_new_and_dedupes` in `tools::tests`.
    pub fn spawn_alias_discovery(&self, palace: String, project_root: PathBuf) {
        let state = self.clone();
        tokio::spawn(async move {
            let args = serde_json::json!({
                "palace": palace,
                "project_root": project_root.to_string_lossy(),
            });
            match tools::dispatch_tool(&state, "discover_aliases", args).await {
                Ok(result) => tracing::info!(
                    new = ?result.get("new"),
                    already_known = ?result.get("already_known"),
                    "alias discovery complete"
                ),
                Err(e) => tracing::warn!("alias discovery failed: {e:#}"),
            }
        });
    }

    pub async fn embedder(&self) -> Result<Arc<FastEmbedder>> {
        let cell = self.embedder.clone();
        let embedder = cell
            .get_or_try_init(|| async {
                let e = FastEmbedder::new().await?;
                Ok::<Arc<FastEmbedder>, anyhow::Error>(Arc::new(e))
            })
            .await?
            .clone();
        Ok(embedder)
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("version", &self.version)
            .field("data_root", &self.data_root)
            .field("registry_len", &self.registry.len())
            .finish()
    }
}

/// Handle a single MCP JSON-RPC message and produce its response.
///
/// Why: Pulled out of the stdio loop so unit tests can drive every method
/// without touching real stdin/stdout.
/// What: Routes `initialize`, `tools/list`, `tools/call`, `ping`, and the
/// `notifications/initialized` notification (which returns `Value::Null`).
/// Test: See unit tests below — initialize/list/call all return expected
/// JSON-RPC envelopes; notifications return `Null` (no response written).
pub async fn handle_message(state: &AppState, msg: Value) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    match method {
        "initialize" => {
            let extra = state
                .default_palace
                .as_ref()
                .map(|dp| json!({ "default_palace": dp }));
            let result = initialize_response("trusty-memory", &state.version, extra);
            // Why (issue #42): prompt-facts now flow through the
            // per-message `get_prompt_context` tool rather than MCP
            // prompts, so we no longer advertise the `prompts` capability.
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            })
        }
        // Notifications must NOT receive a response.
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": tools::tool_definitions_with(state.default_palace.is_some())
        }),
        // OpenRPC 1.3.2 discovery — see `openrpc.rs`. Returns the full
        // service description so orchestrators (open-mpm, etc.) can
        // introspect every tool and its required `memory.read`/`memory.write`
        // scope without bespoke per-server adapters.
        "rpc.discover" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": openrpc::build_discover_response(
                &state.version,
                state.default_palace.is_some(),
            ),
        }),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_default();
            let tool_name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or_default();
            match tools::dispatch_tool(state, &tool_name, args).await {
                Ok(content) => {
                    // Why: tools that return a bare JSON string (e.g.
                    // `get_prompt_context` returning the formatted
                    // Markdown block) should surface as plain text in the
                    // MCP `content[0].text` field — wrapping in
                    // `Value::to_string()` would re-quote the payload and
                    // force every caller to strip outer quotes.
                    let text = match &content {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": text}]
                        }
                    })
                }
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    // Why: anyhow's `{:#}` alternate format walks the full
                    // `Caused by:` chain so MCP clients see actionable
                    // detail (e.g. "PalaceHandle::remember_with_options:
                    // filter rejected: too short") instead of just the
                    // outermost context label.
                    "error": {"code": -32603, "message": format!("{e:#}")}
                }),
            }
        }
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        }),
    }
}

/// Run the MCP stdio JSON-RPC 2.0 server loop.
///
/// Why: Claude Code launches MCP servers as child processes and speaks
/// JSON-RPC over stdin/stdout — this is the primary integration path.
/// What: Delegates to `trusty_mcp_core::run_stdio_loop`, adapting each
/// shared `Request` back into the JSON `Value` shape `handle_message`
/// expects, and translating the returned `Value` into a `Response`.
/// Notifications (where `handle_message` returns `Value::Null`) become
/// suppressed responses so the loop emits nothing on the wire.
/// Test: `handle_message` covers protocol behaviour in unit tests.
pub async fn run_stdio(state: AppState) -> Result<()> {
    info!("trusty-memory MCP stdio server starting");
    let state = Arc::new(state);
    trusty_common::mcp::run_stdio_loop(move |req: Request| {
        let state = state.clone();
        async move {
            // Re-serialise the Request into the JSON shape handle_message expects.
            // (handle_message predates the shared types and reads loose Values.)
            let msg = json!({
                "jsonrpc": req.jsonrpc.unwrap_or_else(|| "2.0".to_string()),
                "id": req.id.clone().unwrap_or(Value::Null),
                "method": req.method,
                "params": req.params.unwrap_or(Value::Null),
            });
            let resp_value = handle_message(&state, msg).await;
            // handle_message returns Value::Null for notifications.
            if resp_value.is_null() {
                return Response::suppressed();
            }
            // Otherwise it returns the full JSON-RPC envelope as a Value;
            // re-encode into the shared Response struct so the loop can serialise.
            let id = resp_value.get("id").cloned();
            if let Some(result) = resp_value.get("result").cloned() {
                Response::ok(id, result)
            } else if let Some(err) = resp_value.get("error") {
                let code =
                    err.get("code")
                        .and_then(|c| c.as_i64())
                        .unwrap_or(error_codes::INTERNAL_ERROR as i64) as i32;
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("internal error")
                    .to_string();
                Response::err(id, code, message)
            } else {
                Response::err(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "malformed handler response",
                )
            }
        }
    })
    .await
}

/// Preferred starting port for the trusty-memory HTTP daemon.
///
/// Why: keeps the well-known default stable for clients that have hard-coded
/// `127.0.0.1:7070` in their configuration, while still allowing dynamic
/// walking when the port is in use (`DYNAMIC_PORT_RANGE` ports starting here).
/// What: `7070` — historic default, matches the launchd plist's prior value.
/// Test: covered indirectly by `bind_dynamic_port_returns_listener`.
pub const DEFAULT_HTTP_PORT: u16 = 7070;

/// Number of consecutive ports `bind_dynamic_port` walks before falling back
/// to the OS-assigned port. Matches the trusty-search convention.
const DYNAMIC_PORT_RANGE: u16 = 10;

/// Path to the canonical address-discovery file for the trusty-memory daemon.
///
/// Why: clients (CLI, MCP tools, dashboards) need to find the running daemon
/// without configuration when the port was selected dynamically. Using
/// `trusty_common::resolve_data_dir` aligns this path with the location
/// that `trusty_common::read_daemon_addr("trusty-memory")` reads from, so
/// `prompt-context`, `doctor`, and `start`'s probe all find the running daemon.
/// The old `~/.trusty-memory/http_addr` path and the new
/// `~/Library/Application Support/trusty-memory/http_addr` (macOS) path were
/// divergent — the daemon wrote one; readers expected the other.
/// What: returns `{resolve_data_dir("trusty-memory")}/http_addr`, or `None` if
/// the data dir cannot be resolved (locked-down container, no passwd entry).
/// Test: `http_addr_path_uses_resolve_data_dir`.
pub fn http_addr_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        .map(|d| d.join("http_addr"))
}

/// Bind a `TcpListener` to `127.0.0.1`, dynamically selecting a port.
///
/// Why: the historic default `7070` is convenient for clients but a stale
/// process or a second daemon must not produce a noisy failure. Walking
/// `DEFAULT_HTTP_PORT..DEFAULT_HTTP_PORT+DYNAMIC_PORT_RANGE` first preserves
/// backwards compatibility for the common case; OS-assigned fallback (`:0`)
/// guarantees the daemon always comes up even when every preferred port is
/// busy.
/// What: returns the first successful `TcpListener`. Tries 7070..=7079
/// in order, then falls back to OS-assigned. Caller inspects
/// `local_addr()` to learn the chosen port.
/// Test: `bind_dynamic_port_returns_listener` confirms it always binds *some*
/// port even after another listener occupies the preferred one.
pub async fn bind_dynamic_port() -> Result<tokio::net::TcpListener> {
    let preferred: SocketAddr = SocketAddr::from(([127, 0, 0, 1], DEFAULT_HTTP_PORT));
    // First: walk the preferred range (7070..=7079).
    if let Ok(listener) =
        trusty_common::bind_with_auto_port(preferred, DYNAMIC_PORT_RANGE - 1).await
    {
        return Ok(listener);
    }
    // Last resort: ask the kernel for any free port. `bind_with_auto_port`
    // with `:0` resolves immediately to the OS-assigned port.
    tracing::warn!(
        "all ports {DEFAULT_HTTP_PORT}..{} in use; requesting OS-assigned port",
        DEFAULT_HTTP_PORT + DYNAMIC_PORT_RANGE - 1
    );
    let any: SocketAddr = SocketAddr::from(([127, 0, 0, 1], 0));
    trusty_common::bind_with_auto_port(any, 0).await
}

/// Write the bound `host:port` to `~/.trusty-memory/http_addr` atomically.
///
/// Why: clients must read the file mid-write without observing a partial
/// value. Writing to a `.tmp` sibling and renaming over the target gives
/// POSIX atomicity, matching the trusty-search implementation.
/// What: creates `~/.trusty-memory/` if missing; writes `addr` followed by a
/// trailing newline (avoids the "no newline at end of file" warnings from
/// `cat`); renames `.tmp` → `http_addr`. Best-effort: I/O errors are
/// returned to the caller so `run_http_on` can log without panicking.
/// Test: `http_addr_file_round_trip_via_helpers`.
fn write_http_addr_file(path: &Path, addr: &SocketAddr) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{addr}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Run the optional HTTP/SSE + web admin server.
///
/// Why: A long-running daemon mode lets non-stdio clients (browsers, curl,
/// future remote agents) hit `/health`, the `/api/v1/*` REST surface, and the
/// embedded admin SPA.
/// What: axum router built from `web::router()` plus a `/sse` stub for the
/// existing MCP-over-SSE clients. Caller provides a pre-bound listener so
/// port auto-detection lives at the call site. Before accepting connections
/// the daemon stamps the bound `host:port` onto `AppState.bound_addr` and
/// writes `~/.trusty-memory/http_addr` so clients can discover the live port.
/// On shutdown the file is removed best-effort (a stale file with the wrong
/// port is worse than a missing one).
/// Test: `cargo test -p trusty-memory web::tests` exercises the router shape;
/// manual: `curl http://127.0.0.1:<port>/health` returns `ok` with `addr`.
pub async fn run_http_on(state: AppState, listener: tokio::net::TcpListener) -> Result<()> {
    use axum::routing::get;

    // Issue #35: recompute the `data_root` disk footprint every 10 s on a
    // background task so `GET /health` reports `disk_bytes` without doing a
    // recursive directory walk on the request path.
    spawn_disk_size_ticker(state.clone());

    // Capture and advertise the bound address BEFORE serving so the first
    // request handler — and the http_addr discovery file — see the real port
    // even if `local_addr()` would otherwise be racy.
    let local = listener.local_addr().ok();
    let written_path = if let Some(a) = local {
        // Stash on state for handlers (e.g. /health) to surface.
        let _ = state.bound_addr.set(a);
        info!("HTTP server listening on http://{a}");
        eprintln!("HTTP server listening on http://{a}");
        // Best-effort: a missing $HOME or read-only fs is non-fatal — the
        // /health endpoint still advertises `addr`. Logging the failure
        // helps operators diagnose discovery problems.
        match http_addr_path() {
            Some(p) => match write_http_addr_file(&p, &a) {
                Ok(()) => {
                    info!("wrote daemon address to {}", p.display());
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!("could not write {}: {e}", p.display());
                    None
                }
            },
            None => {
                tracing::warn!("no $HOME — skipping http_addr discovery file");
                None
            }
        }
    } else {
        None
    };

    let app = web::router()
        .route("/sse", get(sse_handler))
        .with_state(state);

    let serve_result = axum::serve(listener, app).await;

    // Best-effort cleanup: remove `http_addr` so stale clients fail fast
    // instead of timing out against a dead port.
    if let Some(p) = written_path.as_ref() {
        let _ = std::fs::remove_file(p);
    }

    serve_result?;
    Ok(())
}

/// Convenience: bind `addr` and serve via [`run_http_on`].
pub async fn run_http(state: AppState, addr: std::net::SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    run_http_on(state, listener).await
}

/// Convenience: bind dynamically (7070..=7079, OS fallback) and serve.
///
/// Why: `trusty-memory serve` with no `--http` flag is the canonical
/// launchd-managed daemon entry point. Dynamic binding lets a stale daemon
/// or a hand-spawned `serve --http 127.0.0.1:7070` coexist without breaking
/// the launchd-managed instance.
/// What: calls [`bind_dynamic_port`] then [`run_http_on`].
/// Test: integration via `trusty-memory serve` + `cat ~/.trusty-memory/http_addr`.
pub async fn run_http_dynamic(state: AppState) -> Result<()> {
    let listener = bind_dynamic_port().await?;
    run_http_on(state, listener).await
}

/// Spawn a background ticker that recomputes the `data_root` disk footprint
/// every 10 seconds and stores it in `state.disk_bytes` (issue #35).
///
/// Why: `GET /health` reports `disk_bytes`. Walking the data directory on
/// every health request would turn a frequent health poll into unbounded
/// recursive I/O. Computing it off the request path on a fixed cadence keeps
/// `/health` cheap and bounds the staleness to ~10 s — fine for an
/// at-a-glance footprint figure.
/// What: spawns a detached tokio task. `AppState` is cheap to `Clone` (all
/// `Arc` fields), so the task holds a full clone; the daemon process lives
/// for the lifetime of the server anyway, so no `Weak` downgrade is needed.
/// Each tick runs the blocking directory walk on `spawn_blocking` so it never
/// stalls the async runtime, then stores the byte total atomically.
/// Test: `health_endpoint_includes_resource_fields` asserts the field shape;
/// the ticker cadence is not unit-tested (timing-dependent).
fn spawn_disk_size_ticker(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let dir = state.data_root.clone();
            // The directory walk is blocking filesystem I/O — run it on the
            // blocking pool so it never parks an async worker thread.
            let bytes = tokio::task::spawn_blocking(move || {
                trusty_common::sys_metrics::dir_size_bytes(&dir)
            })
            .await
            .unwrap_or(0);
            state
                .disk_bytes
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Live SSE event stream — pushes `DaemonEvent` frames to dashboard clients.
///
/// Why: The dashboard subscribes once and reacts to live pushes (palace
/// created, drawer added/deleted, dream completed, status changed) instead of
/// polling `/api/v1/*` endpoints.
/// What: Subscribes to `state.events`, emits an initial `connected` frame,
/// then forwards every `DaemonEvent` as `data: <json>\n\n`. Lagged
/// subscribers receive a `lag` frame indicating skipped events; channel
/// closure ends the stream.
/// Test: `web::tests::sse_stream_emits_palace_created` (covers subscribe +
/// emit + receive); manual: `curl -N http://.../sse`.
pub(crate) async fn sse_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl axum::response::IntoResponse {
    use futures::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let rx = state.events.subscribe();
    let initial = futures::stream::once(async {
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(
            "data: {\"type\":\"connected\"}\n\n",
        ))
    });
    let events = BroadcastStream::new(rx).map(|res| {
        let frame = match res {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => format!("data: {json}\n\n"),
                Err(e) => format!("data: {{\"type\":\"error\",\"message\":\"{e}\"}}\n\n"),
            },
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                format!("data: {{\"type\":\"lag\",\"skipped\":{n}}}\n\n")
            }
        };
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(frame))
    });
    let stream = initial.chain(events);

    axum::response::Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .expect("valid SSE response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Leak the tempdir so it lives for the test process; tests are short.
        std::mem::forget(tmp);
        AppState::new(root)
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version_and_capabilities() {
        let state = test_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        });
        let resp = handle_message(&state, req).await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "trusty-memory");
    }

    #[tokio::test]
    async fn initialized_notification_returns_null() {
        let state = test_state();
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        let resp = handle_message(&state, req).await;
        assert!(resp.is_null());
    }

    #[tokio::test]
    async fn tools_list_returns_all_tools() {
        let state = test_state();
        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let resp = handle_message(&state, req).await;
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 20);
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let state = test_state();
        let req = json!({"jsonrpc": "2.0", "id": 4, "method": "wat"});
        let resp = handle_message(&state, req).await;
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn ping_returns_empty_result() {
        let state = test_state();
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "ping"});
        let resp = handle_message(&state, req).await;
        assert!(resp["result"].is_object());
    }

    #[tokio::test]
    async fn app_state_default_constructs() {
        let s = test_state();
        assert!(!s.version.is_empty());
        assert!(s.registry.is_empty());
        assert!(s.default_palace.is_none());
    }

    /// Why: Issue #26 — when `serve --palace <name>` is set, the MCP server
    /// must (a) report the default in the `initialize` `serverInfo`, (b)
    /// drop `palace` from the required schema in `tools/list`, and (c) let
    /// `tools/call` use the default when the caller omits `palace`.
    /// Test: Construct an AppState with a default palace, create that palace
    /// on disk via the registry, then call `memory_remember` without a
    /// `palace` argument and confirm it resolves to the default.
    #[tokio::test]
    async fn default_palace_used_when_arg_omitted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();

        // Pre-create the default palace so remember has somewhere to land.
        let registry = trusty_common::memory_core::PalaceRegistry::new();
        let palace = trusty_common::memory_core::Palace {
            id: trusty_common::memory_core::PalaceId::new("default-pal"),
            name: "default-pal".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: root.join("default-pal"),
        };
        registry
            .create_palace(&root, palace)
            .expect("create_palace");

        let state = AppState::new(root).with_default_palace(Some("default-pal".to_string()));

        // (a) initialize advertises the default.
        let init = handle_message(
            &state,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
        )
        .await;
        assert_eq!(
            init["result"]["serverInfo"]["default_palace"], "default-pal",
            "initialize must echo default_palace in serverInfo"
        );

        // (b) tools/list drops `palace` from required when default is set.
        let list = handle_message(
            &state,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
        let tools = list["result"]["tools"].as_array().expect("tools array");
        let remember = tools
            .iter()
            .find(|t| t["name"] == "memory_remember")
            .expect("memory_remember tool");
        let required: Vec<&str> = remember["inputSchema"]["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !required.contains(&"palace"),
            "palace must not be required when default is configured; got {required:?}"
        );
        assert!(required.contains(&"text"));

        // (c) tools/call resolves the default when arg is omitted.
        let call = handle_message(
            &state,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory_remember",
                    "arguments": {"text": "default palace test memory content with several tokens"},
                },
            }),
        )
        .await;
        // Successful dispatch returns `result.content[0].text` JSON.
        let text = call["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected success result, got {call}"));
        let parsed: Value = serde_json::from_str(text).expect("parse content json");
        assert_eq!(parsed["palace"], "default-pal");
        assert_eq!(parsed["status"], "stored");
        assert!(parsed["drawer_id"].as_str().is_some());
    }

    /// Why: When no default is set, `tools/call` for a palace-bound tool
    /// without a `palace` argument should error helpfully rather than panic.
    #[tokio::test]
    async fn missing_palace_without_default_errors() {
        let state = test_state();
        let resp = handle_message(
            &state,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "memory_recall",
                    "arguments": {"query": "anything"},
                },
            }),
        )
        .await;
        assert_eq!(resp["error"]["code"], -32603);
        let msg = resp["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("missing 'palace'"),
            "expected helpful error, got: {msg}"
        );
    }

    /// Why: regression for the "palaces lost on restart" bug — `AppState::new`
    /// builds an empty registry, so the daemon must call
    /// `load_palaces_from_disk` on startup to re-register palaces persisted by
    /// a previous run. Without that call the registry stays empty even though
    /// `palace.json` files exist on disk.
    /// What: persists two palaces under a tempdir (via the same
    /// `create_palace` path the `palace_create` tool uses), constructs a fresh
    /// `AppState` rooted there, calls `load_palaces_from_disk`, and asserts the
    /// returned count and registry contents.
    /// Test: this test itself.
    #[tokio::test]
    async fn load_palaces_from_disk_rehydrates_registry() {
        use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();

        // Phase 1: persist two palaces to disk, then drop the writer registry
        // so nothing is held in memory — simulating a prior daemon run.
        {
            let writer = PalaceRegistry::new();
            for id in ["alpha", "beta"] {
                let palace = Palace {
                    id: PalaceId::new(id),
                    name: id.to_string(),
                    description: None,
                    created_at: chrono::Utc::now(),
                    data_dir: root.join(id),
                };
                writer
                    .create_palace(&root, palace)
                    .expect("persist palace to disk");
            }
        }

        // Add a stray non-palace subdirectory; the walker must ignore it.
        std::fs::create_dir_all(root.join("not-a-palace")).expect("mkdir");

        // Phase 2: fresh AppState starts with an empty registry (the bug).
        let state = AppState::new(root);
        assert!(
            state.registry.is_empty(),
            "AppState::new must start with an empty registry"
        );

        // The fix: hydrate from disk.
        let count = state
            .load_palaces_from_disk()
            .await
            .expect("load_palaces_from_disk");

        assert_eq!(count, 2, "both persisted palaces should be loaded");
        assert_eq!(state.registry.len(), 2, "registry should hold both palaces");
        let ids: Vec<String> = state.registry.list().into_iter().map(|p| p.0).collect();
        assert!(ids.contains(&"alpha".to_string()));
        assert!(ids.contains(&"beta".to_string()));
    }

    /// Why: existing installs (and the legacy standalone `trusty-memory` repo)
    /// nest palaces one level deeper under a `palaces/` subdirectory. When that
    /// subdirectory exists, `resolve_palace_registry_dir` must descend into it
    /// so the daemon scans the level that actually holds the `palace.json`
    /// files — otherwise it finds zero palaces, which is the restart bug.
    /// What: creates `<dir>/palaces/`, resolves, and asserts the nested path is
    /// returned.
    /// Test: this test itself.
    #[test]
    fn resolve_palace_registry_dir_prefers_palaces_subdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(data_dir.join("palaces")).expect("mkdir palaces");

        let resolved = resolve_palace_registry_dir(data_dir.clone());
        assert_eq!(resolved, data_dir.join("palaces"));
    }

    /// Why: a fresh install with no `palaces/` subdirectory must fall back to
    /// the data dir itself (the current flat monorepo layout).
    #[test]
    fn resolve_palace_registry_dir_falls_back_to_data_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().to_path_buf();

        let resolved = resolve_palace_registry_dir(data_dir.clone());
        assert_eq!(resolved, data_dir);
    }

    /// Why: end-to-end check that the nested-`palaces/` layout hydrates — the
    /// daemon resolves the registry dir via `resolve_palace_registry_dir`, so
    /// an `AppState` rooted there must load palaces persisted one level below
    /// the bare data dir.
    /// What: persists two palaces under `<root>/palaces/<id>/`, constructs an
    /// `AppState` rooted at the resolved registry dir, and asserts hydration
    /// finds both.
    /// Test: this test itself.
    #[tokio::test]
    async fn load_palaces_from_disk_handles_palaces_subdir() {
        use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let nested = root.join("palaces");

        {
            let writer = PalaceRegistry::new();
            for id in ["cto", "engineering"] {
                let palace = Palace {
                    id: PalaceId::new(id),
                    name: id.to_string(),
                    description: None,
                    created_at: chrono::Utc::now(),
                    data_dir: nested.join(id),
                };
                // create_palace anchors data_dir under the passed root, so
                // pass `nested` here to land palaces under `<root>/palaces/`.
                writer
                    .create_palace(&nested, palace)
                    .expect("persist palace under palaces/ subdir");
            }
        }

        // Mirror main.rs: resolve the registry dir, then root AppState there.
        let registry_dir = resolve_palace_registry_dir(root);
        assert_eq!(registry_dir, nested, "must resolve into palaces/ subdir");
        let state = AppState::new(registry_dir);
        let count = state
            .load_palaces_from_disk()
            .await
            .expect("load_palaces_from_disk");

        assert_eq!(count, 2, "both nested palaces should be loaded");
        assert_eq!(state.registry.len(), 2);
        let ids: Vec<String> = state.registry.list().into_iter().map(|p| p.0).collect();
        assert!(ids.contains(&"cto".to_string()));
        assert!(ids.contains(&"engineering".to_string()));
    }

    /// Why: an empty (or missing) palace registry directory must not error — a
    /// brand-new install has nothing to hydrate and should report zero.
    #[tokio::test]
    async fn load_palaces_from_disk_empty_root_returns_zero() {
        let state = test_state();
        let count = state
            .load_palaces_from_disk()
            .await
            .expect("load_palaces_from_disk on empty root");
        assert_eq!(count, 0);
        assert!(state.registry.is_empty());
    }

    /// Why: initialize without a default palace must omit `default_palace`
    /// from `serverInfo` so clients can detect the unbound mode.
    #[tokio::test]
    async fn initialize_without_default_palace_omits_field() {
        let state = test_state();
        let init = handle_message(
            &state,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
        )
        .await;
        assert!(init["result"]["serverInfo"]["default_palace"].is_null());
    }

    /// Why: every `~/.trusty-memory/http_addr` consumer (CLI, dashboard,
    /// future trusty-mpm wiring) must agree on the path. A regression that
    /// moves this file breaks every client relying on `read_daemon_addr`.
    /// What: under a stubbed data dir, the path ends in
    /// `trusty-memory/http_addr` — matching `trusty_common::read_daemon_addr`'s
    /// expected location.
    #[test]
    fn http_addr_path_uses_resolve_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Pin the data directory so we don't depend on the real HOME / XDG.
        // SAFETY: single-threaded test; TRUSTY_DATA_DIR_OVERRIDE is a
        // test-only convention documented in trusty-common.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let result = http_addr_path();
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        let p = result.expect("http_addr_path must return Some when data dir is resolvable");
        assert!(
            p.ends_with("trusty-memory/http_addr"),
            "unexpected http_addr path: {}",
            p.display()
        );
    }

    /// Why: write+read round-trip pins the disk format: a single line of
    /// `host:port\n`. Clients (cat, sh `$(cat ...)`) trim whitespace, so the
    /// trailing newline is invisible — but anything else (extra whitespace,
    /// multi-line) would break callers.
    #[test]
    fn http_addr_file_round_trip_via_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("http_addr");
        let addr: SocketAddr = "127.0.0.1:7073".parse().unwrap();
        write_http_addr_file(&path, &addr).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.trim(), "127.0.0.1:7073");
        // The trailing newline keeps `cat` and editors happy.
        assert!(raw.ends_with('\n'));
    }

    /// Why: dynamic binding must succeed even when the preferred port is
    /// already in use. Walking 7070..=7079 + OS fallback guarantees the
    /// daemon never fails to come up just because another process holds 7070.
    /// What: pre-bind 7070 (best-effort — skip the test if it's already
    /// busy on the host), then call `bind_dynamic_port` and assert we got
    /// *some* listener back.
    #[tokio::test]
    async fn bind_dynamic_port_returns_listener() {
        let listener = bind_dynamic_port().await.expect("bind_dynamic_port");
        let addr = listener.local_addr().expect("local_addr");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0, "port must be non-zero after bind");
    }

    /// Why: Issue #42 — prompt-facts are now served by the per-message
    /// `get_prompt_context` tool rather than the MCP prompts surface, so the
    /// `initialize` handshake must NOT advertise a `prompts` capability and
    /// `prompts/list` / `prompts/get` must fall through to the "method not
    /// found" path.
    #[tokio::test]
    async fn initialize_does_not_advertise_prompts_capability() {
        let state = test_state();
        let init = handle_message(
            &state,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
        )
        .await;
        assert!(
            init["result"]["capabilities"]["prompts"].is_null(),
            "initialize must NOT advertise the prompts capability; got {init}"
        );

        // Both prompts/* dispatchers should now report method-not-found.
        for method in ["prompts/list", "prompts/get"] {
            let resp =
                handle_message(&state, json!({"jsonrpc": "2.0", "id": 2, "method": method})).await;
            assert_eq!(
                resp["error"]["code"], -32601,
                "{method} should return method-not-found; got {resp}"
            );
        }
    }

    /// Why: `AppState::new` must initialise `bound_addr` to an empty
    /// `OnceLock` so `/health` reports `addr: None` on the stdio path. A
    /// regression that pre-populates this field would advertise a bogus
    /// address from a stale clone.
    #[test]
    fn app_state_starts_with_empty_bound_addr() {
        let state = test_state();
        assert!(state.bound_addr.get().is_none());
    }

    /// Why (issue #96): `DaemonEvent::type_str` underpins the persisted
    /// activity log's `event_type` column — every variant must map to the
    /// exact SSE `type` tag the UI already handles. A drift between the
    /// SSE wire format and the stored type would break the feed's icon /
    /// label rendering for historical rows.
    /// What: constructs one of each variant, serialises via serde, and
    /// confirms `type_str()` matches the JSON `type` field.
    /// Test: this test.
    #[test]
    fn daemon_event_type_str_matches_sse_tag() {
        let cases = [
            DaemonEvent::PalaceCreated {
                id: "p".into(),
                name: "p".into(),
                source: ActivitySource::Http,
            },
            DaemonEvent::DrawerAdded {
                palace_id: "p".into(),
                palace_name: "p".into(),
                drawer_count: 1,
                timestamp: chrono::Utc::now(),
                content_preview: String::new(),
                source: ActivitySource::Mcp,
            },
            DaemonEvent::DrawerDeleted {
                palace_id: "p".into(),
                drawer_count: 0,
                source: ActivitySource::Http,
            },
            DaemonEvent::DreamCompleted {
                palace_id: None,
                merged: 0,
                pruned: 0,
                compacted: 0,
                closets_updated: 0,
                duration_ms: 0,
                source: ActivitySource::Http,
            },
            DaemonEvent::StatusChanged {
                total_drawers: 0,
                total_vectors: 0,
                total_kg_triples: 0,
            },
        ];
        for ev in &cases {
            let json = serde_json::to_value(ev).unwrap();
            assert_eq!(json["type"].as_str(), Some(ev.type_str()));
        }
    }

    /// Why (issue #96): `palace_id()` and `source()` feed the persisted
    /// activity log's columns; they must extract the right field per
    /// variant. Sloppy refactors could swap two fields and the log would
    /// silently mis-attribute writes.
    /// What: builds each variant with known field values and asserts the
    /// extractor returns them.
    /// Test: this test.
    #[test]
    fn daemon_event_palace_id_and_source_extraction() {
        let ev = DaemonEvent::DrawerAdded {
            palace_id: "alpha".into(),
            palace_name: "alpha".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: String::new(),
            source: ActivitySource::Mcp,
        };
        assert_eq!(ev.palace_id(), Some("alpha"));
        assert_eq!(ev.source(), Some(ActivitySource::Mcp));

        let status = DaemonEvent::StatusChanged {
            total_drawers: 1,
            total_vectors: 2,
            total_kg_triples: 3,
        };
        assert_eq!(status.palace_id(), None);
        assert_eq!(status.source(), None);

        let dream = DaemonEvent::DreamCompleted {
            palace_id: Some("p1".into()),
            merged: 0,
            pruned: 0,
            compacted: 0,
            closets_updated: 0,
            duration_ms: 10,
            source: ActivitySource::Http,
        };
        assert_eq!(dream.palace_id(), Some("p1"));
        assert_eq!(dream.source(), Some(ActivitySource::Http));
    }

    /// Why (issue #96): `AppState::emit` must persist mutation events to
    /// the activity log while keeping `StatusChanged` (a recomputed
    /// aggregate, not a mutation) out of the persisted history.
    /// What: emits one of each variant under a fresh state and asserts
    /// the persisted count matches the number of mutation events.
    /// Test: this test.
    #[tokio::test]
    async fn emit_persists_mutations_but_skips_status_changed() {
        let state = test_state();
        state.emit(DaemonEvent::PalaceCreated {
            id: "p".into(),
            name: "p".into(),
            source: ActivitySource::Http,
        });
        state.emit(DaemonEvent::StatusChanged {
            total_drawers: 1,
            total_vectors: 0,
            total_kg_triples: 0,
        });
        state.emit(DaemonEvent::DrawerAdded {
            palace_id: "p".into(),
            palace_name: "p".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: "x".into(),
            source: ActivitySource::Mcp,
        });
        let count = state.activity_log.count().unwrap();
        assert_eq!(count, 2, "only PalaceCreated + DrawerAdded must persist");
    }
}
