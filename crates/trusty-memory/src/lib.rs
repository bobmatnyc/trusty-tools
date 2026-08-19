//! MCP server (HTTP/SSE + stdio) for trusty-memory.
//!
//! Why: Claude Code and other MCP-aware clients integrate with trusty-memory
//! through the standardized Model Context Protocol; we expose memory + KG
//! tools so they can be called by name. The canonical stdio integration is
//! `trusty-memory serve --stdio` (PR1 #919 of the #914 cutover epic), a
//! self-contained direct MCP server that binds no HTTP port or UDS socket.
//! The former `trusty-memory-mcp-bridge` binary and Unix-domain-socket
//! transport were removed in PR3 (#914) once `serve --stdio` made them dead
//! code.
//! What: Provides `run_http` / `run_http_dynamic` / `run_http_on` (axum
//! HTTP/SSE + REST + UI) plus an `AppState` that carries the shared
//! `PalaceRegistry`, on-disk data root, and a lazily-initialized embedder.
//! Test: `cargo test -p trusty-memory` validates handshake + dispatch via
//! the in-process `handle_message` unit tests and the
//! `tests/serve_stdio_e2e.rs` end-to-end harness.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use crate::session_store_cache::SessionStoreCache;
use anyhow::Result;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{broadcast, OnceCell, RwLock};
use trusty_common::mcp::initialize_response;
use trusty_common::memory_core::embed::Embedder;
use trusty_common::memory_core::{store::ChatSessionStore, PalaceRegistry};
use trusty_common::ChatProvider;

/// Two-phase daemon readiness state (issues #910/#911, revised by #1970).
///
/// Why: The embedder cold-init (CoreML compile, 30-120 s) must never block
/// the fast text/KG/BM25 paths that don't need it. Originally (#910/#911)
/// this state gated a hard-error preflight that rejected every
/// `memory_remember`/`memory_recall` call outright while `Warming` — mirrored
/// from trusty-search's staged pipeline, #1970 replaced that with graceful
/// degradation: writes persist immediately and defer embedding to a
/// background task, reads return BM25 + L0/L1 results and simply omit the
/// vector lane, all keyed off this same state.
/// What: Two stable values stored atomically.  `Warming` (0) is the initial
/// state; `Ready` (1) is set once the embedder has been successfully
/// initialised by `spawn_startup_tasks`.  The transition is one-way and
/// lock-free: a single `AtomicU8` compare-and-swap.
/// Test: `daemon_readiness_transitions_warming_to_ready` in this module;
///       degraded-path coverage in `tools::tests`
///       (`remember_succeeds_and_defers_embedding_while_state_is_warming`,
///       `recall_falls_back_to_bm25_and_l0_l1_while_warming`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonReadiness {
    /// Embedder cold-init (and/or pin scan) still in progress.
    Warming = 0,
    /// Embedder initialised; all handlers may proceed normally.
    Ready = 1,
}

impl DaemonReadiness {
    /// Decode the raw atomic value.
    ///
    /// Why: centralises the `0 → Warming, else Ready` mapping so every
    /// caller loads a meaningful enum rather than comparing raw integers.
    /// What: returns `Warming` for `0`, `Ready` for any other value (only
    /// `1` is ever written).
    /// Test: `daemon_readiness_from_u8` in this module.
    pub fn from_u8(v: u8) -> Self {
        if v == 0 {
            Self::Warming
        } else {
            Self::Ready
        }
    }
}

pub mod activity;
pub mod attribution;
pub mod authz;
pub mod bm25_backfill;
pub mod bm25_index;
pub mod bm25_lane;
pub mod bm25_repair;
pub mod bootstrap;
pub mod dream_scheduler;
pub mod fd_metrics;
pub mod idle_evict;
pub mod worker_liveness;
// Why (issue #226): `chat` and `web` are pure axum HTTP/SSE handler
//      surfaces. Gating them behind the `axum-server` feature is what lets
//      library consumers (e.g. `trusty-agents` linking only `MemoryMcpService`)
//      drop axum + tower-http entirely from their build graph.
#[cfg(feature = "axum-server")]
pub mod chat;
pub mod commands;
pub mod console_metrics;
pub mod discovery;
mod events;
pub mod foreground;
pub mod hook_emit;
mod http_server;
pub mod kg_extract;
// #5524: the single entry point every caller-supplied KG assert routes through.
pub mod kg_write;
pub mod mcp_service;
pub mod messaging;
pub mod openrpc;
pub mod palace_id_derive;
pub mod project_root;
pub mod prompt_facts;
pub mod prompt_log;
pub mod service;
pub mod session_store_cache;
pub mod startup_scan;
pub mod tools;
pub mod transport;
#[cfg(feature = "axum-server")]
pub mod web;
pub mod wordnet_pos;

pub use activity::{ActivityEntry, ActivityFilter, ActivityLog, ActivitySource};
pub use attribution::{CreatorInfo, CreatorSource};

// Re-export the event types so the crate's public API is unchanged after the
// #1195 split (`trusty_memory::DaemonEvent`, etc.). `open_activity_log_with_fallback`
// stays crate-internal but is re-exported so `AppState::new` and `lib_tests`
// reach it by its bare name via `super::*`.
pub(crate) use events::open_activity_log_with_fallback;
// #3434: test-only seam so `lib_tests` can force the tempdir-fallback path
// via an explicit parameter instead of mutating the process-global `TMPDIR`.
#[cfg(test)]
pub(crate) use events::open_activity_log_with_fallback_in;
pub use events::{DaemonEvent, HookType, InjectionKind};

// Re-export the HTTP-serving surface so the crate's public API is unchanged
// after the #1195 split (`trusty_memory::run_http_on`, etc.). The address-file
// helpers stay crate-internal but are re-exported for `lib_tests`' `super::*`.
pub use http_server::{
    bind_dynamic_port, http_addr_path, is_data_dir_override_active, DEFAULT_HTTP_PORT,
};
#[cfg(feature = "axum-server")]
pub use http_server::{run_http, run_http_dynamic, run_http_on};
// These crate-internal HTTP helpers are consumed only by test modules
// (`lib_tests`, `web::tests`), so gate the re-export on `cfg(test)` to avoid an
// unused-import warning in the normal (non-test) build.
#[cfg(all(test, feature = "axum-server"))]
pub(crate) use http_server::{dotfile_http_addr_path, write_http_addr_file};

/// Maximum bytes retained in the trigger-prompt excerpt embedded on a
/// `HookFired` event.
///
/// Why: the full triggering prompt is sensitive and already lives in the
/// JSONL prompt log; the activity feed only needs enough text to give an
/// operator a glance — a single-line ~80 char preview matches the existing
/// `drawer_content_preview` convention so dashboard rows render uniformly.
/// What: 80 characters; longer prompts are truncated with a trailing `…`.
/// Test: `hook_excerpt_truncates_long_prompts`.
pub const HOOK_PROMPT_EXCERPT_CHARS: usize = 80;

/// Reduce a triggering prompt to the short excerpt embedded on a
/// `HookFired` activity event.
///
/// Why: see [`HOOK_PROMPT_EXCERPT_CHARS`]. Centralising the truncation rule
/// keeps every emitter (HTTP, hook CLI handlers, future tests) producing
/// the same preview shape so UI rendering is uniform.
/// What: whitespace-collapses `prompt` and trims to
/// [`HOOK_PROMPT_EXCERPT_CHARS`] chars with `…` when cut. Empty input
/// returns an empty string.
/// Test: `hook_excerpt_truncates_long_prompts`,
/// `hook_excerpt_collapses_whitespace`.
pub fn hook_prompt_excerpt(prompt: &str) -> String {
    let normalised: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= HOOK_PROMPT_EXCERPT_CHARS {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(HOOK_PROMPT_EXCERPT_CHARS.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

pub use mcp_service::MemoryMcpService;
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
    // Issue #1939: the subdir-choice logic is hoisted into trusty-common
    // (`palace_alias::palace_registry_dir_from`) so trusty-mpm's alias-registration
    // path and this daemon path can never disagree on WHERE the registry (and the
    // alias file beside it) lives. This delegates to keep a single implementation.
    trusty_common::palace_alias::palace_registry_dir_from(data_dir)
}

/// Shared application state passed to every request handler.
///
/// Why: The stdio loop and HTTP server need the same handles to the registry,
/// data root, and embedder so MCP tools can perform real reads/writes against
/// the live trusty-memory core. The embedder is heavy (loads ONNX weights) so
/// it is resolved lazily through the process-wide singleton on first use.
///
/// #4836: this struct used to carry its own `Arc<OnceCell<Arc<FastEmbedder>>>`,
/// a SECOND cell independent of `retrieval::shared_embedder()`. Startup warmed
/// the shared cell and latched `daemon_readiness` off it, while every recall
/// consumed the private cell — so the flag reported one embedder's state while
/// the request path used another's. One cell removes the disagreement (and the
/// duplicate ~90 MB ONNX session) by construction.
/// What: `Clone`-able via `Arc` fields. The registry / data root are eager; the
/// embedder is reached via [`AppState::embedder`].
/// Test: `app_state_default_constructs` confirms construction without panic.
#[derive(Clone)]
pub struct AppState {
    pub version: String,
    pub registry: Arc<PalaceRegistry>,
    pub data_root: PathBuf,
    /// Optional default palace applied to MCP tool calls when the caller
    /// omits the `palace` argument. Set via `trusty-memory serve --palace`.
    pub default_palace: Option<String>,
    /// Active chat provider selected at startup. `None` means no upstream is
    /// configured (no Ollama detected and no OpenRouter key) — callers must
    /// degrade gracefully (chat endpoint returns 412).
    pub chat_provider: Arc<OnceCell<Option<Arc<dyn ChatProvider>>>>,
    /// Per-palace chat-session stores, opened lazily so cold-start cost is
    /// paid only when chat-history endpoints are hit.
    ///
    /// #4639: was an unbounded `DashMap` with no `remove`/TTL/cap, leaking one
    /// `chat_sessions.redb` fd per palace for the daemon's lifetime (844
    /// measured live, all pointing at already-unlinked files). Now an
    /// LRU-bounded cache that evicts cold, unused stores.
    pub session_stores: Arc<SessionStoreCache>,
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
    /// Bug-capture ERROR store (bug-reporting #478, Phase 1).
    ///
    /// Why: Phase 2 MCP / HTTP endpoints need to query captured errors; stashing
    ///      the `ErrorStore` handle here lets any handler reach it cheaply without
    ///      a second global or per-request construction.
    /// What: populated by `run_serve` from the `init_tracing_with_buffer_and_capture`
    ///      result; the layer writes to this store automatically so every
    ///      `tracing::error!` call site contributes without any changes to call
    ///      sites. `None` in states that do not install the layer (tests, the
    ///      stdio path).
    /// Test: compile-presence is verified by the `trusty-memory` build; Phase 2
    ///      will add query tests in `web.rs`.
    pub error_store: Option<trusty_common::error_capture::ErrorStore>,
    /// Minimal multi-tenant authorization seam (issue #1714). `false`
    /// (single-tenant, the default) preserves today's behaviour — every
    /// existing caller of `palace_create force=true` keeps working. `true`
    /// opts into `authz::authorize_force_palace_create` failing closed on
    /// every `force=true` request until a real capability check lands. Set
    /// via `with_multi_tenant_mode_from_env` (`TRUSTY_MEMORY_MULTI_TENANT=1`);
    /// see the `authz` module docs for the full design rationale.
    pub multi_tenant_mode: bool,
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
    /// hot predicate. #5524: every caller-supplied assert reaches that rebuild
    /// through `kg_write::assert_triple` rather than each surface remembering
    /// to call it; the retract side (`remove_prompt_fact`) still calls the
    /// rebuild directly.
    /// What: An `Arc<tokio::sync::RwLock<PromptFactsCache>>` so the hot
    /// read path takes a brief read lock and clones the cache; rebuilds
    /// take a write lock for the assignment only. The async-aware lock
    /// (issue #229) yields to the tokio runtime instead of blocking a
    /// runtime thread for the rebuild duration. An empty `triples` vec ↔
    /// "no context stored yet" (the tool handler renders a hint).
    /// Test: `get_prompt_context_returns_cached_or_hint`,
    /// `get_prompt_context_filters_by_query`.
    pub prompt_context_cache: Arc<RwLock<prompt_facts::PromptFactsCache>>,
    /// Serializes the Tier S check-then-write sequence (#4888).
    ///
    /// Why: the 20-fact cap is only a cap if it cannot be raced past.
    /// `check_tier_s_admission` counts active facts across every palace and
    /// then the caller writes — two callers that both observe 19 would both
    /// pass and the surface would land at 21. Nothing else serializes them:
    /// the KG's single-writer actor orders writes only *within* one palace,
    /// and the count spans all of them. "Usually 20, occasionally 21" is not
    /// the invariant ADR-0028 D8 asks for.
    /// What: an async mutex whose guard is acquired by
    /// `check_tier_s_admission` and returned to the caller, which must hold it
    /// until its `kg.assert` is enqueued. Cold predicates never acquire it, so
    /// ordinary knowledge-graph writes stay fully concurrent; hot writes are
    /// deliberate and rare, so serializing them costs nothing measurable.
    /// Test: `tier_s_cap_holds_under_concurrent_writes`.
    pub tier_s_admission_lock: Arc<tokio::sync::Mutex<()>>,
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
    /// Optional in-process BM25 lexical search lane (issue #156, #5329).
    ///
    /// Why: this single field replaces the former `bm25_client` +
    /// `bm25_supervisor` pair. Those existed because BM25 ran as a per-palace
    /// subprocess — one to speak its wire protocol, one to spawn and reap it.
    /// #5329 collapsed the subprocess into this process, so the lane IS the
    /// index and there is nothing left to supervise.
    /// What: `Some(lane)` only when `TRUSTY_BM25_DAEMON=1` at startup. Every
    /// code path that uses it is gated on `is_some()` and falls back to
    /// vector-only recall otherwise, so a deployment that never set the gate —
    /// which is every shipped deployment (#5186) — sees no behavioural change.
    ///
    /// 🔴 `pub(crate)`, not `pub`, and that is load-bearing. Installing a lane
    /// also has to rebuild the indexer worker, because `AppState::new` spawns it
    /// before any lane exists. Assigning this field on its own produces a state
    /// whose READS use the lane and whose WRITES the placeholder worker
    /// discards — silently, with no error anywhere. [`Self::with_bm25_lane`] is
    /// the only way to set it and does both halves; keeping the field private
    /// makes the broken half-installed state unrepresentable outside this crate
    /// rather than merely documented. Read it with [`Self::bm25_lane`].
    /// Test: `bm25_lane_disabled_by_default`, `bm25_lane_enabled_when_env_set`,
    /// `writes_through_the_tool_surface_survive_eviction`.
    pub(crate) bm25: Option<Arc<bm25_lane::Bm25Lane>>,
    /// Per-palace write serialisation locks (issue #230).
    ///
    /// Why: the dedup gate in `tools.rs` previously read a snapshot of
    /// existing drawers, checked for near-duplicates via Jaro-Winkler, and
    /// then issued the write — a classic time-of-check/time-of-use race.
    /// Two concurrent `memory_remember` calls with the same content could
    /// both see the pre-write snapshot, both pass the gate, and both land
    /// duplicate drawers. Serialising the gate-then-write sequence per
    /// palace closes the window: while one task holds the mutex, any
    /// concurrent writer for the same palace blocks until the first write
    /// finishes and is visible to `list_drawers`. The lock is **per
    /// palace** (not global) so writes to different palaces continue to
    /// run in parallel.
    /// What: a `DashMap` keyed by palace id, where each entry is an
    /// `Arc<tokio::sync::Mutex<()>>`. The mutex is constructed lazily by
    /// `palace_write_lock` on first access. `Arc` lets callers hold a
    /// clone of the lock past the lifetime of the `DashMap` entry so the
    /// map never needs to be held across an `.await`.
    /// Test: `tools::tests::dedup_gate_blocks_concurrent_duplicate_writes`.
    pub palace_write_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Counter of in-flight activity-log writes spawned by `emit`
    /// (issue #232).
    ///
    /// Why: `emit` offloads the synchronous redb append to the tokio blocking
    /// pool via `spawn_blocking` so the async runtime is never parked waiting
    /// on fsync. The write is fire-and-forget — `emit` returns immediately
    /// after spawning. Tests that observe the activity log right after a
    /// burst of `emit` calls need a deterministic synchronization point;
    /// holding an in-flight counter lets `flush_activity_writes` poll until
    /// every spawned append has settled, which keeps the assertions
    /// race-free without forcing every caller to `.await`.
    /// What: an `Arc<AtomicUsize>` incremented before each `spawn_blocking`
    /// and decremented inside the closure (after the append completes, even
    /// if it errored). The counter is cheap (one atomic add per emit) and
    /// stays at zero in steady-state production traffic.
    /// Test: `web::tests::activity_endpoint_lists_recent_emits` and
    /// `tests::emit_persists_mutations_but_skips_status_changed` call
    /// `flush_activity_writes` to drain the counter before reading the log.
    pub pending_activity_writes: Arc<AtomicUsize>,
    /// Live occupancy gauge for the palace open path (issue #4001).
    ///
    /// Why: during the #3992 incident six daemon threads sat parked in
    /// `concurrent_open::backoff_sleep_ms` with a `memory_remember` hung
    /// ~1800 s, while both doctors reported HEALTHY. Every existing signal —
    /// HTTP liveness, fastembed cache state, lock-file staleness — describes
    /// the *process*, not the *work*. This is the one field that can answer
    /// "is anything actually moving?", and `/health` surfaces it so an
    /// out-of-process doctor can report what the daemon actually observed
    /// instead of inferring health from a cheap proxy.
    /// What: an [`worker_liveness::WorkerLiveness`] slot table; one CAS on
    /// entry and one store on exit per tracked operation, so the gauge cannot
    /// itself become the load problem it exists to detect.
    /// Test: `web::tests::health_tests::health_reports_wedged_worker_pool`.
    pub worker_liveness: Arc<worker_liveness::WorkerLiveness>,
    /// How long an operation may run before the pool is called wedged.
    ///
    /// Why this is state rather than an env read on the request path: `/health`
    /// is polled once a second, so re-reading (and re-parsing) an environment
    /// variable per request is needless work; resolving it once at construction
    /// also makes the value a property of the daemon instead of a process-wide
    /// global, which is what lets tests drive the wedge condition
    /// deterministically instead of mutating shared env state and racing each
    /// other.
    /// What: defaults to [`worker_liveness::wedge_threshold`].
    /// Test: `web::tests::health_tests::health_reports_wedged_worker_pool`.
    pub wedge_threshold: std::time::Duration,
    /// In-memory cache mapping palace id → `Palace.name` (issue #228).
    ///
    /// Why: every `memory_remember` / `memory_note` write used to call
    /// `PalaceRegistry::list_palaces` (a synchronous filesystem walk of the
    /// data root) just to resolve a friendly palace name for the SSE
    /// `DrawerAdded` event. With N palaces on disk the cost was O(N) opendirs
    /// plus `palace.json` reads on every write, blocking the async runtime.
    /// Caching the name in-memory turns the lookup into a `DashMap::get`.
    /// What: `DashMap<String, String>` populated by `create_palace` and
    /// `load_palaces_from_disk`, kept in sync by rename / delete paths.
    /// Missing entries are treated as "name unknown" so callers fall back to
    /// the palace id and the emit path never fails.
    /// Test: `palace_name_cache_populated_after_hydration` and
    /// `palace_name_cache_updates_on_create`.
    pub palace_names: Arc<dashmap::DashMap<String, String>>,
    /// Single-pass startup pin-file map: palace id → project root path (issue #470).
    ///
    /// Why: after daemon startup we have no record of which on-disk project
    /// directories correspond to which palace ids — that information only
    /// existed inside the pin files on disk. Eager-opening every palace on
    /// startup is too expensive. This field captures the scan-only result of
    /// `startup_scan::scan_pin_map` so handlers that want to locate a project
    /// by its palace id (e.g. future cwd-inference, project-health checks)
    /// can do a single `DashMap::get` instead of a filesystem walk.
    /// Populated once, shortly after `load_palaces_from_disk` returns, by
    /// `spawn_startup_tasks`. Never mutated after population — it is a
    /// snapshot of what the filesystem looked like at startup.
    /// What: `DashMap<String (palace_id), PathBuf (project root)>`.
    /// The outer `Arc` lets `spawn_startup_tasks` (which holds only a clone
    /// of `AppState`) write to the same backing map that request handlers
    /// read. Population is asynchronous so callers must treat an absent entry
    /// as "not yet scanned" (or "no pin found"), never as "palace unknown".
    /// Test: `startup_scan::tests::scan_pin_map_*` validate the underlying
    /// scanner function; the wiring in `spawn_startup_tasks` is covered by
    /// the integration-test daemon start path.
    pub pin_project_map: Arc<dashmap::DashMap<String, PathBuf>>,
    /// Bounded sender for the BM25 index worker (issue #231).
    ///
    /// Why: the previous fire-and-forget design `tokio::spawn`ed one task per
    /// `memory_remember` / `memory_note` call, so a write burst against a slow
    /// or unreachable BM25 daemon grew an unbounded in-flight task queue. A
    /// single long-lived worker draining a bounded mpsc channel caps that
    /// back-pressure: writers `try_send` (never block), full-queue requests
    /// are dropped with a `warn!`, and the worker exits cleanly when the last
    /// sender is dropped on shutdown.
    /// What: an `mpsc::Sender` cloned to every `AppState` clone (cheap). The
    /// matching receiver is consumed by the worker spawned in
    /// [`AppState::new`] via [`tools::spawn_bm25_index_worker`]. Capacity is
    /// [`tools::BM25_INDEX_QUEUE_CAPACITY`] (256).
    /// Test: `bm25_index_queue_drops_when_full` exercises the full-queue
    /// branch via `bm25_index_enqueue`.
    pub bm25_index_tx: tokio::sync::mpsc::Sender<tools::Bm25IndexRequest>,
    /// Palaces whose BM25 coverage is known to be incomplete (#5048 review).
    ///
    /// Why: `bm25_index_enqueue` drops on a full queue so `memory_remember`
    /// never waits on daemon RTT. That trade is only defensible if a drop is
    /// actually repaired, and before this field the sole production trigger for
    /// a backfill was daemon startup — so a drop stayed invisible until the
    /// next restart. Every observer of lost coverage marks the palace here and
    /// [`bm25_repair::spawn_repair_sweep`] consumes it on an interval.
    /// What: a `DashSet` of palace ids, shared by every `AppState` clone.
    /// Idempotent — forty drops for one palace queue one repair.
    /// Test: `bm25_repair_tests.rs`, `bm25_index_queue_drops_when_full`.
    pub bm25_dirty: bm25_repair::DirtyPalaces,
    /// Cached result of the startup update check (issue #537).
    ///
    /// Why: `/health` should report `update_available` without hitting crates.io
    /// on every probe. A single background check at daemon startup stores the
    /// result here; the health handler reads it lock-free (well, a brief mutex
    /// lock) without a network call.
    /// What: `None` = up-to-date or check not yet done; `Some("x.y.z")` = newer
    /// version available. The field is populated by a `tokio::spawn` in
    /// `spawn_startup_tasks` (main.rs) after the daemon binds.
    /// Test: indirectly by the `/health` endpoint tests in `web.rs`.
    pub update_available: Arc<std::sync::Mutex<Option<String>>>,
    /// Two-phase readiness state — `Warming` until the embedder is initialised,
    /// then `Ready` (issues #910 / #911).
    ///
    /// Why: `AppState::embedder()` used to call `FastEmbedder::new()` without
    /// any timeout, so the first `memory_recall`/`memory_remember` that arrived
    /// before CoreML finished compiling would block for 5–11 hours until the
    /// OnceCell resolved (issue #910). Exposing this state lets the preflight
    /// guards in `tools.rs` return an explicit fast error immediately —
    /// `"trusty-memory is warming up, retry shortly"` — instead of queueing
    /// behind an open-ended init.
    /// What: An `AtomicU8` starting at `DaemonReadiness::Warming` (0) and flipped
    /// to `DaemonReadiness::Ready` (1) by `spawn_startup_tasks` after the embedder
    /// warm-up succeeds.  The transition is one-way and lock-free.
    /// Test: `daemon_readiness_transitions_warming_to_ready`.
    pub daemon_readiness: Arc<AtomicU8>,
    /// Total wall-clock ceiling for one MCP write operation (issue #4002).
    ///
    /// Why: a write waits for the per-palace write mutex and then waits again
    /// to enter the per-palace open queue. Each leg was bounded by its own
    /// timeout, so the effective ceiling was their sum (60 s + ~63 s), not
    /// either configured bound. The handlers stamp one
    /// [`trusty_common::memory_core::timeouts::OpBudget`] from this value and
    /// clamp every leg through it, so the later leg spends what the earlier
    /// leg left.
    /// What: defaults to
    /// [`trusty_common::memory_core::timeouts::write_op_budget`]
    /// (`TRUSTY_WRITE_OP_BUDGET_SECS`, 60 s). Stored per-instance rather than
    /// re-read from the environment so
    /// [`AppState::with_write_op_budget`] can inject a short deadline in tests
    /// without mutating process-wide state, which would race parallel tests —
    /// the same reason `PalaceRegistry::with_open_queue_timeout` exists.
    /// Test: `tools::tests::write_budget_tests`.
    pub write_op_budget: std::time::Duration,
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
        // Issue #231: bounded mpsc channel + single long-lived worker
        // replaces the per-write `tokio::spawn` fire-and-forget pattern so
        // BM25 indexing back-pressure is capped. The worker is spawned here
        // unconditionally so the channel always has a drain — even when
        // the lane is off, the worker just consumes and discards
        // each request so senders never block on a full queue.
        let (bm25_index_tx, bm25_index_rx) =
            tokio::sync::mpsc::channel::<tools::Bm25IndexRequest>(tools::BM25_INDEX_QUEUE_CAPACITY);
        // `bm25` starts as `None`; the builder `with_bm25_lane_from_env`
        // rebuilds the worker with the real lane once env-gated opt-in is
        // resolved.
        let bm25_dirty: bm25_repair::DirtyPalaces = Arc::new(dashmap::DashSet::new());
        tools::spawn_bm25_index_worker(bm25_index_rx, None, Arc::clone(&bm25_dirty));
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            // Idle-to-disk: honour TRUSTY_MEMORY_MAX_OPEN_PALACES (default 64)
            // so operators can bound resident-palace RAM without a rebuild.
            registry: Arc::new(PalaceRegistry::from_env()),
            data_root,
            default_palace: None,
            chat_provider: Arc::new(OnceCell::new()),
            // #4639: bounded LRU (TRUSTY_MEMORY_MAX_OPEN_SESSION_STORES,
            // default 32) so chat_sessions.redb handles stop accumulating.
            session_stores: Arc::new(SessionStoreCache::from_env()),
            events: Arc::new(events_tx),
            started_at: std::time::Instant::now(),
            // Default to an empty buffer — `with_log_buffer` overrides this
            // when the daemon installs the `LogBufferLayer` (HTTP mode).
            log_buffer: trusty_common::log_buffer::LogBuffer::new(
                trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
            ),
            // Bug-reporting #478: `None` until `with_error_store` is called
            // during daemon startup (HTTP mode). Tests keep `None` so no
            // unexpected files are written to the OS data dir.
            error_store: None,
            multi_tenant_mode: false,
            disk_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sys_metrics: Arc::new(tokio::sync::Mutex::new(
                trusty_common::sys_metrics::SysMetrics::new(),
            )),
            bound_addr: Arc::new(OnceLock::new()),
            prompt_context_cache: Arc::new(RwLock::new(prompt_facts::PromptFactsCache::default())),
            tier_s_admission_lock: Arc::new(tokio::sync::Mutex::new(())),
            activity_log,
            bm25: None,
            palace_write_locks: Arc::new(dashmap::DashMap::new()),
            pending_activity_writes: Arc::new(AtomicUsize::new(0)),
            worker_liveness: Arc::new(worker_liveness::WorkerLiveness::new()),
            wedge_threshold: worker_liveness::wedge_threshold(),
            palace_names: Arc::new(dashmap::DashMap::new()),
            pin_project_map: Arc::new(dashmap::DashMap::new()),
            bm25_index_tx,
            bm25_dirty,
            update_available: Arc::new(std::sync::Mutex::new(None)),
            // Start in Warming state; flipped to Ready by spawn_startup_tasks
            // once the embedder warm-up succeeds (issues #910/#911).
            daemon_readiness: Arc::new(AtomicU8::new(DaemonReadiness::Warming as u8)),
            // #4002: one ceiling for the whole write, read once at construction.
            write_op_budget: trusty_common::memory_core::timeouts::write_op_budget(),
        }
    }

    /// Override the per-operation write budget (issue #4002).
    ///
    /// Why: tests must prove the two write legs draw on ONE budget without
    /// setting `TRUSTY_WRITE_OP_BUDGET_SECS`, which is process-wide and would
    /// race any test running in parallel.
    /// What: consuming builder that overwrites [`AppState::write_op_budget`].
    /// Test: `tools::tests::write_budget_tests`.
    #[must_use]
    pub fn with_write_op_budget(mut self, budget: std::time::Duration) -> Self {
        self.write_op_budget = budget;
        self
    }

    /// Acquire (lazily, then clone) the per-palace write mutex.
    ///
    /// Why (issue #230): the dedup-check + `remember_with_options` write
    /// sequence in `tools.rs` must be atomic per palace to prevent two
    /// concurrent identical writes from both passing the dedup gate.
    /// Callers hold the returned `Arc<Mutex<()>>`'s guard across the gate
    /// check and the write so the second writer blocks until the first
    /// write is visible to `list_drawers`. Returning a clone of the `Arc`
    /// rather than a borrow into the `DashMap` lets the caller `.await`
    /// while holding the lock without risking a deadlock against any
    /// future map mutation (DashMap shards are sync mutexes).
    /// What: looks up the palace id in `palace_write_locks` and returns
    /// a clone of the existing mutex; on the first call for a palace,
    /// inserts a freshly-constructed `tokio::sync::Mutex<()>` first. The
    /// `DashMap::entry().or_insert_with` API guarantees the lazy
    /// construction is racy-safe — only one mutex is ever inserted per
    /// palace id.
    /// Test: `tools::tests::dedup_gate_blocks_concurrent_duplicate_writes`.
    pub fn palace_write_lock(&self, palace_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        if let Some(existing) = self.palace_write_locks.get(palace_id) {
            return existing.clone();
        }
        self.palace_write_locks
            .entry(palace_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Look up a project root path by palace id in the startup pin-scan map.
    ///
    /// Why: provides a stable, cheap accessor so handlers do not reach directly
    /// into the `DashMap` field and so the accessor can be mocked in future
    /// tests without touching `AppState` internals. The map is populated
    /// asynchronously by `spawn_startup_tasks` — an absent entry means either
    /// the scan has not completed yet or no pin file claimed that id.
    /// What: returns `Some(project_path)` when the palace id was found during
    /// startup scan; `None` otherwise.
    /// Test: covered indirectly via the startup-scan integration path; the
    /// underlying map data is validated by `startup_scan::tests`.
    pub fn pinned_project_path(&self, palace_id: &str) -> Option<PathBuf> {
        self.pin_project_map.get(palace_id).map(|e| e.clone())
    }

    /// Builder-style: opt-in to the BM25 lexical lane (issue #156, #5329).
    ///
    /// Why: the lane stays gated behind `TRUSTY_BM25_DAEMON=1` even though
    /// #5329 removed the daemon that name refers to. Renaming the variable
    /// would break the only enablement path anyone could have configured, in
    /// the one PR whose purpose is not losing that lane — the compatibility is
    /// worth more than the accuracy.
    /// What: when the gate is set, builds a [`bm25_lane::Bm25Lane`] over this
    /// state's `data_root` — which is where the retired daemon wrote its
    /// snapshots, so an existing corpus is picked up in place — and rebuilds
    /// the bounded indexer channel so its worker holds the lane. Returns `self`
    /// unchanged when the var is unset or set to anything other than `1`.
    /// Test: `bm25_lane_disabled_by_default`, `bm25_lane_enabled_when_env_set`.
    #[must_use]
    pub fn with_bm25_lane_from_env(self) -> Self {
        // #5329: the gate keeps its daemon-era name so an operator who set it
        // does not silently lose the lane this change exists to preserve.
        if std::env::var("TRUSTY_BM25_DAEMON").as_deref() != Ok("1") {
            return self;
        }
        let lane = bm25_lane::Bm25Lane::new(self.data_root.clone());
        tracing::info!(
            max_resident = lane.max_resident(),
            text_budget_bytes = ?lane.text_budget_bytes(),
            "in-process BM25 lane enabled (TRUSTY_BM25_DAEMON=1)"
        );
        self.with_bm25_lane(lane)
    }

    /// Builder-style: install an explicit BM25 lane, bypassing the env gate.
    ///
    /// Why: setting `bm25` on its own is a footgun. `AppState::new` spawns the
    /// indexer worker with no lane, so a caller that assigns the field and
    /// nothing else gets a state whose reads use the lane and whose WRITES are
    /// silently discarded by the placeholder worker — which is exactly what
    /// `writes_through_the_tool_surface_survive_eviction` caught. Every path
    /// that installs a lane goes through here so the two cannot drift apart.
    /// What: rebuilds the bounded indexer channel + worker so the worker holds
    /// the lane, then stores it. The placeholder worker installed by
    /// `AppState::new` exits cleanly when the replaced sender closes its
    /// receiver. Tests use this to pin explicit limits without mutating
    /// process-global env vars.
    /// Test: `tests/bm25_lane_concurrency.rs`, `bm25_lane_enabled_when_env_set`.
    #[must_use]
    pub fn with_bm25_lane(mut self, lane: Arc<bm25_lane::Bm25Lane>) -> Self {
        // Issue #231: the bounded channel is what keeps a write burst from
        // growing an unbounded task queue; rebuilding it here is what points
        // its single worker at the lane.
        let (tx, rx) =
            tokio::sync::mpsc::channel::<tools::Bm25IndexRequest>(tools::BM25_INDEX_QUEUE_CAPACITY);
        tools::spawn_bm25_index_worker(rx, Some(Arc::clone(&lane)), Arc::clone(&self.bm25_dirty));
        self.bm25_index_tx = tx;
        self.bm25 = Some(lane);
        self
    }

    /// The BM25 lane, if one is installed.
    ///
    /// Why: the read half of the `pub(crate)` field above. Callers outside this
    /// crate — the integration tests, and anything that wants to flush or query
    /// the lane directly — need to reach it without being able to swap it for
    /// one the indexer worker has never heard of.
    /// Test: `tests/bm25_lane_concurrency.rs`, `tests/bm25_alias_write.rs`.
    pub fn bm25_lane(&self) -> Option<&Arc<bm25_lane::Bm25Lane>> {
        self.bm25.as_ref()
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
    /// What: runs the blocking filesystem walk + per-palace
    /// `PalaceHandle::open_with_intent` on a `spawn_blocking` thread (so it
    /// never stalls the async runtime), registers each successfully opened
    /// palace via `register_arc`, logs every load at `debug!`, and returns the
    /// count loaded. A palace that fails to open (corrupt index, unreadable
    /// `kg.db`, etc.) is logged at `warn!` and skipped — one bad palace must not
    /// abort startup or crash the daemon — but the skip is RECORDED (#4911) and
    /// readable via [`PalaceRegistry::unopenable`], so a palace whose bytes
    /// survive and whose contents cannot be read stays observable instead of
    /// reading as absent. `data_root` is expected to already be the palace
    /// registry directory — `main.rs` resolves it via
    /// [`resolve_palace_registry_dir`] before constructing the `AppState`, so
    /// the flat / legacy-`palaces/` layout difference is handled exactly once.
    ///
    /// Intent (#1487, #4911): every open uses the registry's own
    /// `OpenIntent`, NOT the zero-arg `PalaceHandle::open` default. This is
    /// the path a restarting daemon takes for every palace it already has on
    /// disk, so hardcoding `ReadOnlyClient` here made the daemon's
    /// `with_writer_intent()` guarantee false in the common case.
    /// Test: `tests::load_palaces_from_disk_rehydrates_registry` writes two
    /// palaces into a tempdir, constructs an `AppState`, calls this method, and
    /// asserts the returned count and registry contents;
    /// `load_palaces_from_disk_honours_registry_open_intent` covers the intent
    /// contract in both directions and
    /// `load_palaces_from_disk_records_an_unopenable_palace` the skip record.
    pub async fn load_palaces_from_disk(&self) -> Result<usize> {
        let registry_dir = self.data_root.clone();
        let registry = self.registry.clone();
        let palace_names = self.palace_names.clone();
        // The directory walk and each `PalaceHandle::open` perform blocking
        // filesystem + redb/usearch I/O — run the whole hydration on the
        // blocking pool so it never parks an async worker thread.
        let count = tokio::task::spawn_blocking(move || -> Result<usize> {
            let palaces = PalaceRegistry::list_palaces(&registry_dir)?;
            let total = palaces.len();
            let mut loaded = 0usize;
            let mut skipped = 0usize;
            // #4911: hydrate under the registry's own intent, not the zero-arg
            // `PalaceHandle::open` default (`ReadOnlyClient`).
            let intent = registry.open_intent();
            for palace in palaces {
                match trusty_common::memory_core::PalaceHandle::open_with_intent(&palace, intent) {
                    Ok(handle) => {
                        tracing::debug!(
                            palace = %palace.id,
                            data_dir = %palace.data_dir.display(),
                            "loaded palace from disk"
                        );
                        // Issue #228: seed the in-memory name cache so write
                        // hot paths (memory_remember / memory_note) can resolve
                        // the friendly palace name without re-walking the data
                        // root. Insert here (during hydration) is the single
                        // point of truth for restart-time population.
                        palace_names.insert(palace.id.0.clone(), palace.name.clone());
                        registry.register_arc(handle);
                        loaded += 1;
                    }
                    Err(e) => {
                        // Why (issue #467): a single bad palace (corrupt kg.db,
                        // stale WAL, EMFILE — "Too many open files", permissions)
                        // must never abort startup or block the HTTP server from
                        // binding. Log per-palace and keep going; the summary
                        // below tells operators how many were skipped without
                        // trawling the log.
                        // The palace is NOT registered in the in-memory registry,
                        // so the next `open_palace` call for this id will attempt
                        // a fresh open from disk — the lazy-reopen path. If the
                        // root cause was EMFILE and the fd-limit fix (#462) raised
                        // the soft limit to 8192, that first request will succeed.
                        tracing::warn!(
                            palace = %palace.id,
                            data_dir = %palace.data_dir.display(),
                            "skipping palace during startup hydration: {e:#}; \
                             will retry lazily on first access"
                        );
                        // #4911: a skipped palace is absent from the handle
                        // cache, so record it or it reads as never having
                        // existed. Cleared by `register_arc` if it later opens.
                        registry.record_unopenable(palace.id.clone(), format!("{e:#}"));
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

    /// Builder-style: attach the daemon's shared `LogBuffer` so the
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

    /// Builder-style: mark this daemon as the sole palace writer so palace
    /// redb files open with `OpenIntent::Writer` (issue #1487).
    ///
    /// Why: The HTTP daemon owns the write lock on every palace's `kg.redb`
    /// and `index.usearch.redb`. Before this fix, when a *second* daemon
    /// instance opened the same store it silently degraded to a read-only
    /// snapshot and rejected every `memory_remember` for its lifetime —
    /// effectively silent data loss when an MCP client routed a write to the
    /// rogue instance. Opening as `Writer` makes the second instance fail
    /// loud (after a short handoff-retry window that absorbs a graceful
    /// launchd `bootout`→`bootstrap` overlap) instead of serving broken
    /// reads-only. CLI, stdio-proxy, and test code paths never call this, so
    /// they keep the snapshot read-fallback (issue #59).
    /// What: Replaces `self.registry` with a fresh `PalaceRegistry` carrying
    /// `OpenIntent::Writer`.
    ///
    /// Invariant: MUST be called on a fresh, unhydrated, unshared registry —
    /// during startup, before `spawn_startup_tasks`/`load_palaces_from_disk`
    /// registers any `PalaceHandle` and before the `AppState` (hence its
    /// `Arc<PalaceRegistry>`) is cloned to a handler. Replacing the registry
    /// discards the prior `Arc`; doing so after hydration would silently drop
    /// live handles (data loss), and doing so after the state is shared would
    /// leave other clones on the stale read-only registry. The guard is a
    /// `debug_assert!` on the strongest cheap signals the registry exposes —
    /// `is_empty()` (no handles hydrated) and `Arc::strong_count == 1` (not yet
    /// shared) — so an ordering violation fails fast as the programmer error it
    /// is (the call site is startup-only and fixed). Release builds elide the
    /// assert; the real call site (`run_serve`) always satisfies it.
    /// Test: `with_writer_intent_marks_registry_writer` and
    /// `with_writer_intent_panics_on_hydrated_registry` in `lib_tests`.
    #[must_use]
    pub fn with_writer_intent(mut self) -> Self {
        // Fail fast on an ordering bug: a hydrated registry (`!is_empty`) or a
        // shared one (`strong_count > 1`) would silently drop live handles or
        // strand other clones on the stale read-only registry (issue #1487).
        debug_assert!(self.registry.is_empty() && Arc::strong_count(&self.registry) == 1);
        // Idle-to-disk: preserve the configurable open-handle cap
        // (TRUSTY_MEMORY_MAX_OPEN_PALACES) while marking the registry a writer.
        self.registry = Arc::new(PalaceRegistry::from_env().with_writer_intent());
        self
    }

    /// Builder-style: attach the bug-capture `ErrorStore` handle (bug-reporting #478).
    ///
    /// Why: Phase 2 MCP / HTTP endpoints need a handle to the in-memory error
    ///      ring so they can serve `recent_errors` / `errors_by_fingerprint`
    ///      without disk I/O on the hot path. Installing it here — rather than
    ///      adding it as a separate global — keeps the state graph explicit and
    ///      lets tests skip it by never calling this method.
    /// What: stores `Some(store)` in `AppState::error_store`; the `BugCaptureLayer`
    ///      that writes to this store is already installed in the tracing
    ///      subscriber by `init_tracing_with_buffer_and_capture`. The store is
    ///      `Clone` (cheap `Arc` clone internally) so both the layer and this
    ///      field share the same underlying ring.
    /// Test: Phase 2 will add `error_store_captures_and_queries` in `web.rs`.
    #[must_use]
    pub fn with_error_store(mut self, store: trusty_common::error_capture::ErrorStore) -> Self {
        self.error_store = Some(store);
        self
    }

    /// Builder-style: opt into multi-tenant authorization mode (issue #1714).
    ///
    /// Why: mirrors `with_bm25_client_from_env`'s pattern of keeping env-var
    /// gating in one place. Unset (the default) preserves today's
    /// single-tenant behaviour with zero change for existing callers. Issue
    /// #2522 review: activation is silent otherwise, which makes a
    /// misconfigured (or unexpectedly enabled) deployment hard to diagnose
    /// from logs alone — log once at startup when the mode flips on.
    /// What: sets `multi_tenant_mode` from `TRUSTY_MEMORY_MULTI_TENANT=1`; see
    /// the `authz` module for what the flag then enforces. Logs via
    /// `tracing::info!` (stderr only) when enabled; stays silent when
    /// disabled (the default).
    /// Test: `authorize_force_palace_create_denies_multi_tenant_without_capability`.
    #[must_use]
    pub fn with_multi_tenant_mode_from_env(mut self) -> Self {
        self.multi_tenant_mode = std::env::var("TRUSTY_MEMORY_MULTI_TENANT").as_deref() == Ok("1");
        if self.multi_tenant_mode {
            tracing::info!(
                "multi-tenant mode enabled (TRUSTY_MEMORY_MULTI_TENANT=1): force=true palace_create will be refused"
            );
        }
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
    ///
    /// Issue #232: the activity-log append is a synchronous redb write +
    /// fsync. Calling it directly on the async caller's task parked a tokio
    /// worker thread on disk I/O for every SSE event. We now offload the
    /// append to the blocking thread pool via `spawn_blocking` and return
    /// immediately — `emit` stays synchronous so every existing caller
    /// (including the sync `dispatch_hook_fired` JSON-RPC handler) keeps
    /// compiling unchanged. The fire-and-forget pattern matches the
    /// pre-fix semantics (best-effort, never blocks the SSE broadcast)
    /// while freeing the async runtime to do real work during the write.
    /// What: serialises the event for the log (skipping `StatusChanged`
    /// which is a recomputed aggregate, not a mutation), spawns the redb
    /// append on `tokio::task::spawn_blocking` keyed by a clone of the
    /// `Arc<ActivityLog>` and the cloned event, then sends the event over
    /// the broadcast channel. A `pending_activity_writes` counter is bumped
    /// before the spawn and decremented inside the closure so
    /// [`Self::flush_activity_writes`] can drain in tests.
    /// Test: `web::tests::sse_stream_receives_palace_created` confirms a
    /// subscriber observes the emitted event;
    /// `activity_endpoint_lists_recent_emits` confirms persistence via
    /// `flush_activity_writes`.
    pub fn emit(&self, event: DaemonEvent) {
        if let Some(source) = event.source() {
            let event_type = event.type_str();
            let palace_id = event.palace_id().map(|s| s.to_string());
            let log = Arc::clone(&self.activity_log);
            let event_for_log = event.clone();
            let pending = Arc::clone(&self.pending_activity_writes);
            // Pre-allocate the sequence id in the emitting thread so the
            // persisted order matches the emission order even when blocking-pool
            // workers execute the writes concurrently (issue #247). Without
            // this, four rapid emits would assign IDs inside their respective
            // `spawn_blocking` closures in a non-deterministic order.
            let id = log.alloc_id();
            pending.fetch_add(1, Ordering::SeqCst);
            // Why: the synchronous redb append + fsync must not park an
            // async worker thread (issue #232). Spawn the write on the
            // blocking pool; the JoinHandle is intentionally dropped —
            // the write is best-effort and any failure is logged below.
            tokio::task::spawn_blocking(move || {
                let result = log.append_with_id(id, source, palace_id, event_type, &event_for_log);
                if let Err(e) = result {
                    tracing::warn!("activity_log.append failed for {event_type}: {e:#}");
                }
                pending.fetch_sub(1, Ordering::SeqCst);
            });
        }
        let _ = self.events.send(event);
    }

    /// Block (asynchronously) until every in-flight activity-log write
    /// spawned by [`Self::emit`] has settled.
    ///
    /// Why: `emit` offloads its redb append to `tokio::task::spawn_blocking`
    /// and returns immediately (issue #232). Tests that observe the
    /// activity log right after a burst of emits would otherwise race the
    /// blocking-pool worker; this helper gives them a deterministic
    /// synchronization point. Production code never needs to call this —
    /// the dashboard reads through `GET /api/v1/activity`, which already
    /// tolerates writes settling asynchronously.
    /// What: spins on `pending_activity_writes` with a 1 ms yield until the
    /// counter is zero. Cheap: tests typically emit a handful of events
    /// and the loop exits within a single scheduler tick.
    /// Test: covered indirectly by `emit_persists_mutations_but_skips_status_changed`
    /// and `web::tests::activity_endpoint_lists_recent_emits`.
    pub async fn flush_activity_writes(&self) {
        while self.pending_activity_writes.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    /// Open (or return cached) the chat-session store for a palace.
    ///
    /// Why: Chat session persistence lives in a dedicated redb file under
    /// the palace's data dir (`chat_sessions.redb`) so it doesn't intermingle
    /// with the KG's transactional load. The store is cheap to clone via
    /// `Arc` but the underlying connection should be reused, so cache by id.
    /// What: delegates to the LRU-bounded [`SessionStoreCache`], which creates
    /// the palace data dir if missing, opens (or reuses) a `ChatSessionStore`,
    /// and evicts cold, *unused* stores once more than the cap are resident.
    /// Callers keep the returned `Arc` for as long as they need it — eviction
    /// never closes a store someone still holds.
    /// Test: `session_store_cache::tests::open_handles_are_bounded_by_cap`,
    /// `session_store_cache::tests::in_use_store_is_never_evicted`; the call
    /// path is covered indirectly by the session HTTP handlers in `web::tests`.
    pub fn session_store(&self, palace_id: &str) -> Result<Arc<ChatSessionStore>> {
        // #4639: bounded cache replaces the unbounded, never-evicting DashMap.
        self.session_stores
            .get_or_open(palace_id, &self.data_root.join(palace_id))
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
                // Why (issue #226): `service::load_user_config` is the
                //      axum-free home of the loader; the `web::load_user_config`
                //      re-export only exists for the HTTP handlers. Going
                //      direct to `service` keeps this method usable when
                //      the `axum-server` feature is disabled.
                let cfg = crate::service::load_user_config().unwrap_or_default();
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

    /// Return the current readiness state.
    ///
    /// Why: tool handlers and the `/health` endpoint need a cheap, lock-free
    /// way to check whether the embedder has been initialised yet.
    ///
    /// What: loads `daemon_readiness` with `Acquire` ordering so the caller
    /// sees all writes the startup task made before setting the state.
    ///
    /// #4836: the value read here is only as good as the writes that reach it.
    /// It used to be written exactly once, by the startup warm-up task, so a
    /// single failed attempt pinned the daemon at `Warming` for the rest of its
    /// life. [`AppState::embedder`] now also flips it on success, which is the
    /// signal that actually proves a vector search can run.
    /// Test: `daemon_readiness_transitions_warming_to_ready`,
    /// `resolving_the_embedder_marks_a_warming_daemon_ready`.
    pub fn readiness(&self) -> DaemonReadiness {
        DaemonReadiness::from_u8(self.daemon_readiness.load(Ordering::Acquire))
    }

    /// Flip the readiness state from `Warming` to `Ready`.
    ///
    /// Why: called by `spawn_startup_tasks` in `main.rs` once the embedder
    /// warm-up succeeds — this is the single state-transition site.
    /// What: `store(Ready, Release)` so subsequent `Acquire` loads in handlers
    /// observe a consistent state.  Idempotent: calling it multiple times is
    /// harmless.
    /// Test: `daemon_readiness_transitions_warming_to_ready`.
    pub fn set_ready(&self) {
        self.daemon_readiness
            .store(DaemonReadiness::Ready as u8, Ordering::Release);
    }

    /// Obtain the shared `FastEmbedder` instance, initialising it on first call.
    ///
    /// Why: centralises lazy embedder access so every tool handler goes through
    /// one bounded init path (tracks #910 internally).
    /// What: wraps `OnceCell::get_or_try_init` with a timeout so a slow
    /// CoreML/CUDA first-compile cannot block a handler indefinitely.  On
    /// timeout the `OnceCell` is left unresolved and the next caller retries.
    ///
    /// **Callers on the request path SHOULD check `readiness()` before this
    /// method** (issue #1970) — every recall handler now checks
    /// `readiness() == Ready` first and only calls `embedder()` on that
    /// branch, falling back to a BM25/L0/L1-only path while `Warming` instead
    /// of paying this method's cold-init cost. Reaching this method while
    /// still `Warming` is not a bug (the warm-up task itself calls
    /// `embedder()` while in `Warming` state), just unusual on the request
    /// path.
    ///
    /// This timeout is a backstop against a pathological init delay (e.g. the
    /// warm-up task's own call, or a handler that skips the `readiness()`
    /// check). If this timeout fires the `OnceCell` is left in the unresolved
    /// state and the next call retries from scratch.
    pub async fn embedder(&self) -> Result<Arc<dyn Embedder + Send + Sync>> {
        // #4836: delegate to the ONE process-wide cell instead of a private
        // second one, so initialising the embedder here is the same event the
        // startup warm-up latches readiness off. `shared_embedder` already
        // applies the bounded `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` init timeout
        // and the CoreML auto-fallback, so no wrapper timeout is needed here.
        let embedder = trusty_common::memory_core::retrieval::shared_embedder().await?;
        // #4836: a resolved embedder is proof a vector search can run, so it is
        // the authoritative readiness signal — not the startup task's one-shot
        // attempt. Without this the daemon stays `Warming` forever whenever that
        // single attempt failed, and every MCP recall serves the degraded
        // L0/L1 fallback that ignores the query entirely.
        self.set_ready();
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
        // service description so orchestrators (trusty-agents, etc.) can
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

#[cfg(test)]
mod lib_tests;
