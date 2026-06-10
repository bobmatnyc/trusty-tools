//! Shared state and live event types for the trusty-search HTTP daemon.
//!
//! Why: `SearchAppState` (wrapped in `Arc`) is the single shared object
//! injected into every axum handler. `DaemonEvent` is the broadcast-channel
//! enum pushed to SSE dashboard subscribers.
//! What: struct definition + builder methods; see also `state_impl.rs` for
//! the full `impl` block.
//! Test: see `../tests` and the handler test modules.
use dashmap::DashMap;
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, watch, OnceCell, RwLock};
use trusty_common::{ChatProvider, LocalModelConfig};

use crate::core::{
    embed::Embedder,
    registry::{IndexId, IndexRegistry},
};
use crate::service::reindex::ReindexProgress;

/// Live daemon events pushed to dashboard subscribers via the `/status/stream`
/// SSE feed.
///
/// Why: Mirrors the trusty-memory broadcast-channel pattern — a single tagged
/// enum fanned out to every connected browser tab so the UI updates without
/// per-tab polling.
/// What: Tagged-enum (snake_case) serialised as `{"type": "status_changed",
/// ...fields}`. Only `StatusChanged` exists today; new variants (e.g.
/// `IndexCreated`, `ReindexCompleted`) plug in here without touching the
/// handler.
/// Test: subscribe to `/status/stream`, wait > 2s, parse a `status_changed`
/// frame and assert the four fields are present.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent {
    StatusChanged {
        indexes: u64,
        total_chunks: u64,
        uptime_secs: u64,
        version: String,
    },
    /// Emitted by `POST /indexes` when a brand-new index is registered.
    ///
    /// Why: The dashboard's "Recent indexes" table is populated by a one-shot
    /// `GET /indexes` fan-out at mount time; without a push event a user
    /// running `trusty-search index <path>` would have to refresh the page to
    /// see the new index. Emitting a tagged event lets the SPA call
    /// `refreshIndexes()` immediately.
    /// What: `{"type":"index_registered","id":"<index-id>"}`.
    /// Test: subscribe to `/status/stream`, `POST /indexes`, assert an
    /// `index_registered` frame with the matching id arrives.
    IndexRegistered { id: String },
    /// Emitted by `DELETE /indexes/:id` when an index is actually evicted.
    ///
    /// Why: Same rationale as `IndexRegistered` — keep dashboards reactive
    /// without page refreshes.
    /// What: `{"type":"index_removed","id":"<index-id>"}`.
    /// Test: register → delete, subscribe before delete, assert an
    /// `index_removed` frame arrives.
    IndexRemoved { id: String },
}

/// Shared state injected into every axum handler.
#[derive(Clone)]
pub struct SearchAppState {
    pub registry: IndexRegistry,
    /// Per-index reindex progress (live counters + SSE replay buffer). Started
    /// by `POST /indexes/:id/reindex`, consumed by
    /// `GET /indexes/:id/reindex/stream`. Lazily populated.
    pub reindex_progress: Arc<DashMap<IndexId, Arc<ReindexProgress>>>,
    /// Issue #120: per-index timestamp of the most recent reindex that
    /// aborted at the memory limit. Used by `reindex_handler` to apply a
    /// cooldown (`TRUSTY_REINDEX_COOLDOWN_SECS`, default 300 s) before
    /// honouring another reindex request — re-running immediately would
    /// hit the same limit and produce a tight loop.
    ///
    /// Why: when a reindex aborts at the memory limit, some files have no
    /// content-hash entry yet, so a follow-up reindex sees them as "new"
    /// and re-processes them — hitting the limit again. The cooldown gives
    /// operators time to lower batch size / raise the limit before another
    /// attempt.
    /// What: written by `spawn_reindex_with_cleanup` when `mem_limit_hit`
    /// is true; read by `reindex_handler` before queuing.
    /// Test: covered by `reindex_handler_rejects_within_cooldown` in
    /// `src/service/server.rs#tests`.
    pub last_reindex_aborted_at: Arc<DashMap<IndexId, std::time::Instant>>,
    /// Process-wide embedder shared across every index so the (expensive)
    /// fastembed ONNX session is initialized once. `None` keeps the daemon
    /// in BM25-only mode — useful for tests that don't want to download the
    /// model. The vector dimensionality is read from the embedder.
    pub embedder: Option<Arc<dyn Embedder>>,
    /// Mutable embedder slot used by the deferred-init flow: the daemon binds
    /// its HTTP port immediately, then a background task loads the fastembed
    /// model and writes it here before flipping `embedder_ready` to `true`.
    ///
    /// Why: ONNX/CoreML model loading takes 15–30 s on first run, but the
    /// outer `Option<Arc<dyn Embedder>>` is captured by reference in many
    /// places. A separate `Arc<RwLock<…>>` lets the init task replace the
    /// value once without rewriting handler signatures.
    /// Test: start daemon; `/health` returns `embedder: "initializing"` for a
    /// few seconds, then flips to `"ready"`.
    pub embedder_slot: Arc<RwLock<Option<Arc<dyn Embedder>>>>,
    /// Watch channel signalling embedder readiness. Handlers that need the
    /// embedder (search, create_index in hybrid mode, index-file) check
    /// `*embedder_ready.borrow()` and return `503 Service Unavailable` until
    /// the value flips to `true`.
    ///
    /// Why: lets `trusty-search index` and `trusty-search start` connect to
    /// the daemon within ~1 s instead of waiting 15–30 s for the embedder to
    /// finish loading. Callers can poll `/health` (cheap) or just hit the
    /// real endpoint and retry on 503.
    /// Test: start daemon; `POST /indexes` immediately returns 503 with
    /// `{"error":"embedder initializing"}`; after a few seconds the same call
    /// succeeds.
    pub embedder_ready: watch::Receiver<bool>,
    /// Sender half of the readiness watch, held by the AppState so the
    /// background embedder-init task can flip readiness from `false` to
    /// `true` once `FastEmbedder::new()` completes.
    ///
    /// Why: kept inside the state (rather than handed off as a free variable)
    /// so test code constructing a fresh `SearchAppState` doesn't have to
    /// thread a sender through every helper. The Arc lets `start.rs` clone
    /// it into the background task.
    pub embedder_ready_tx: Arc<watch::Sender<bool>>,
    /// Last error message captured by the embedder background-init task, or
    /// `None` when init is still in flight or succeeded.
    ///
    /// Why (issue #121): on Intel Xeon AVX-512 hosts, `ort-2.0.0-rc.12`'s
    /// CPU session init can block forever — the daemon stays alive but every
    /// `POST /indexes` hangs (or returns "initializing" indefinitely). With
    /// no visible error, operators waste hours debugging. Surfacing the
    /// init-task error here lets `/health` report `embedder: "error"` with a
    /// human-readable message and lets `POST /indexes` fail fast with a 503
    /// instead of dangling forever.
    /// What: an `Arc<RwLock<Option<String>>>` set by `install_embedder_error`
    /// when `build_embedder()` returns `Err`, or when the init task times out.
    /// Test: `health_reports_embedder_error_when_init_fails` verifies the
    /// `/health` response includes `embedder: "error"` and an `embedder_error`
    /// string after the init task sets an error.
    pub embedder_error: Arc<RwLock<Option<String>>>,
    /// Port the daemon ended up listening on. Injected into the served
    /// `index.html` as `window.__DAEMON_PORT__` so the SPA knows which host
    /// to call when opened directly. `None` falls back to 7878 in the UI.
    pub daemon_port: Option<u16>,
    /// Whether `OPENROUTER_API_KEY` is set when the daemon starts. Toggles
    /// the Chat panel in the SPA via `window.__OPENROUTER_ENABLED__`.
    pub openrouter_enabled: bool,
    /// Monotonic timestamp captured when the AppState was constructed.
    /// Used to compute `uptime_secs` in the `/health` response (issue #34).
    pub started_at: Instant,
    /// Local-model (Ollama / LM Studio / llama.cpp server) configuration loaded
    /// from `~/.trusty-search/config.toml`. Drives `auto_detect_local_provider`
    /// and the `/api/chat/providers` payload.
    pub local_model: LocalModelConfig,
    /// OpenRouter model id (loaded from config; default
    /// `anthropic/claude-haiku-4.5`). Used by the OpenRouter fallback provider.
    pub openrouter_model: String,
    /// OpenRouter API key resolved at startup. May be empty when the user
    /// only configured a local model; the chat handler returns 503 in that case.
    pub openrouter_api_key: String,
    /// Lazily-initialised active chat provider. Auto-detection happens on the
    /// first chat call and the result is cached for the daemon's lifetime.
    pub chat_provider: Arc<OnceCell<Option<Arc<dyn ChatProvider>>>>,
    /// Broadcast sender for live `DaemonEvent` pushes to SSE subscribers.
    ///
    /// Why: Lets the periodic status-ticker (and any future mutating handler)
    /// emit events that every connected dashboard receives instantly. Mirrors
    /// the trusty-memory pattern: cap of 128 buffers transient slow readers;
    /// if a receiver lags it gets `RecvError::Lagged` and we emit a `lag` frame.
    /// What: A `tokio::sync::broadcast::Sender<DaemonEvent>` wrapped in `Arc`
    /// so it's cheap to clone across the AppState.
    /// Test: `emit_propagates_to_subscriber` verifies a subscriber observes
    /// the emitted event.
    pub events: Arc<broadcast::Sender<DaemonEvent>>,
    /// In-memory ring buffer of recent tracing log lines, fed by the
    /// `LogBufferLayer` wired into the subscriber at daemon startup.
    ///
    /// Why (issue #35): the `GET /logs/tail` endpoint serves the last N log
    /// lines so operators can inspect a running daemon without tailing a file
    /// or restarting with a different `RUST_LOG`. The buffer must be shared
    /// between the tracing layer (writer) and the HTTP handler (reader).
    /// What: a cheap `Arc`-backed clone of the same buffer the subscriber
    /// writes to. Defaults to an empty buffer for test states that never
    /// install the layer.
    /// Test: `logs_tail_returns_recent_lines` pushes lines then GETs them.
    pub log_buffer: trusty_common::log_buffer::LogBuffer,
    /// Most recent on-disk footprint of the daemon's data directory, in bytes.
    ///
    /// Why (issue #35): `GET /health` reports `disk_bytes` (redb + usearch +
    /// snapshot files). Walking the directory tree on every health request
    /// would make a 2 s health poll do unbounded I/O; instead a background
    /// task recomputes it every 10 s and stores the result here so the
    /// handler reads it lock-free.
    /// What: an `AtomicU64` updated by the task spawned in `build_router`.
    /// `0` until the first walk completes (typically within 10 s of startup).
    /// Test: `health_includes_resource_fields` asserts the field is present.
    pub disk_bytes: Arc<std::sync::atomic::AtomicU64>,
    /// Per-process RSS + CPU sampler, refreshed on each `/health` request.
    ///
    /// Why (issue #35): `GET /health` reports `rss_mb` and `cpu_pct`. CPU
    /// usage is a delta between two `sysinfo` refreshes, so the sampler must
    /// persist between requests — hence the shared `Mutex`.
    /// What: a `tokio::sync::Mutex<SysMetrics>` so the async health handler
    /// can sample without blocking the runtime. `/health` is polled at ~2 s
    /// intervals so lock contention is negligible.
    /// Test: `health_includes_resource_fields`.
    pub sys_metrics: Arc<tokio::sync::Mutex<trusty_common::sys_metrics::SysMetrics>>,
    /// Embedder worker pool with priority lanes (issue #41 Phase 1).
    ///
    /// Why: Centralises every embedding call so interactive search queries
    /// never wait behind a long-running reindex. Wrapped in
    /// `Arc<RwLock<Option<…>>>` so the background embedder-init task can
    /// install the pool after `run_daemon` has already started serving
    /// requests — handlers observe the pool atomically via
    /// `embed_pool.read().await.clone()`.
    /// What: `None` until `install_embed_pool` is called; subsequent reads
    /// see a cloneable `Arc<EmbedPool>`.
    /// Test: covered indirectly — `start_brings_pool_online`.
    pub embed_pool: Arc<RwLock<Option<Arc<crate::service::embed_pool::EmbedPool>>>>,
    /// Prometheus recorder handle, populated by `start.rs` when the recorder
    /// is installed. `None` in tests / when the recorder is skipped.
    ///
    /// Why: routes `/metrics` only when the recorder has been wired so tests
    /// constructing an AppState without metrics don't accidentally surface
    /// an empty metrics endpoint.
    /// What: `Some(MetricsState)` enables the `/metrics` route; `None` skips
    /// it. The render itself is lock-free (PrometheusHandle is Clone).
    /// Test: covered by `metrics_handler_returns_prometheus_text`.
    pub metrics: Option<crate::service::metrics::MetricsState>,
    /// Current OS PID of the `trusty-embedderd` sidecar process (issue #282).
    ///
    /// Why: the daemon's own RSS (`rss_mb` on `/health`) excludes the sidecar,
    /// which owns the ONNX arena. Surfacing the sidecar's RSS separately gives
    /// operators the full memory picture. `0` means the sidecar is not running
    /// (in-process / HTTP remote / UDS mode, or sidecar has exited).
    ///
    /// What: an `Arc<AtomicU32>` set by `install_embedderd_pid_slot()` after the
    /// sidecar spawns. The `EmbedderSupervisor` loop owns the same Arc and
    /// updates it automatically on crash-restart (new PID) and exit (0).
    /// Initialised to 0 so reads before the sidecar spawns return `None` from
    /// `current_embedderd_pid()`.
    ///
    /// Test: `health_includes_embedderd_rss_field` in `server.rs#tests` verifies
    /// the field is present in the health response.
    pub embedderd_pid_slot: Arc<std::sync::atomic::AtomicU32>,
    /// Handle of the currently-running pid-slot forwarder task (issue #829).
    ///
    /// Why: `install_embedderd_pid_slot` spawns a background task that copies
    /// the sidecar PID from the supervisor's Arc to the AppState's slot every
    /// 500 ms. Without tracking this handle, each sidecar restart (idle-shutdown
    /// cycle) accumulates one leaked task because the previous slot's value
    /// never resets to 0. The fix: store the last `AbortHandle` here so
    /// `install_embedderd_pid_slot` can abort the old task before spawning a new
    /// one, bounding the number of live forwarder tasks to exactly one.
    /// What: `Arc<tokio::sync::Mutex<Option<AbortHandle>>>` so multiple clones of
    /// `SearchAppState` (e.g. the flush clone in `run_daemon`) share the handle.
    /// `None` until the first `install_embedderd_pid_slot` call.
    /// Test: `pid_slot_forwarder_does_not_leak_tasks` in `tests_state.rs`.
    pub embedderd_pid_forwarder_handle: Arc<tokio::sync::Mutex<Option<tokio::task::AbortHandle>>>,
    /// In-process graceful-shutdown trigger (issue #829 — ungraceful admin_stop).
    ///
    /// Why: `POST /admin/stop` previously called `std::process::exit(0)` directly
    /// from a detached task, which bypasses Rust destructors, the redb flush in
    /// `flush_all_indexes_on_shutdown`, and axum's graceful-connection drain.
    /// Hard-exiting mid-write can corrupt the redb corpus (the B-tree file is not
    /// guaranteed to be in a consistent state if the write-half of a transaction
    /// is aborted by SIGKILL-equivalent). Using a `watch` channel lets
    /// `admin_stop_handler` signal `run_daemon` (which holds the send half wrapped
    /// in `Arc`) to drop the axum server cleanly, flushing all data first.
    ///
    /// What: a `watch::Sender<bool>` whose receiver is polled by `run_daemon` as
    /// an additional shutdown trigger alongside the OS SIGTERM/SIGINT handler.
    /// When the value becomes `true`, the axum `with_graceful_shutdown` future
    /// resolves and the normal post-serve flush path runs.
    ///
    /// Test: `admin_stop_triggers_graceful_shutdown` in `tests_state.rs`.
    pub shutdown_tx: Arc<watch::Sender<bool>>,
    /// Cached result of the startup update check (issue #537).
    ///
    /// Why: `/health` should report `update_available` without hitting crates.io
    /// on every probe. A single background check at daemon startup stores the
    /// result here; the health handler reads it without a network call.
    /// What: `None` = up-to-date or check not yet done; `Some("x.y.z")` = newer
    /// version available. Populated by a `tokio::spawn` in `start.rs`.
    /// Test: indirectly by the `/health` endpoint tests in this module.
    pub update_available: Arc<std::sync::Mutex<Option<String>>>,
    /// Count of indexes from `indexes.toml` that failed to warm-boot on the
    /// current daemon start (issue #764).
    ///
    /// Why: operators need a machine-readable signal that some registered
    /// indexes did NOT load — without it, a TCC-denied or corrupt index is
    /// silently absent from search results and `/health`, with no visible
    /// error. Surfacing the count lets `/health` flag `warmboot_failed_indexes`
    /// and lets `trusty-search health` warn the operator.
    /// What: an `AtomicUsize` incremented by `start.rs` once per failed
    /// warm-boot restore; reset to 0 on each daemon start. `0` = all
    /// registered indexes loaded successfully.
    /// Test: `health_reports_warmboot_failures` in server tests.
    pub warmboot_failed_indexes: Arc<std::sync::atomic::AtomicUsize>,
    /// Warm-boot summary surfaced on `GET /health` (issue #873).
    ///
    /// Why: when `cargo install` changes the binary cdhash, macOS TCC revokes
    /// Full Disk Access and the daemon silently loads only ~2 indexes instead
    /// of ~102. Operators have no machine-readable way to detect this without
    /// tailing logs. `WarmBootSummary` gives `indexes_loaded`,
    /// `indexes_skipped_tcc`, `indexes_skipped_timeout`, and a
    /// `warm_boot_degraded` flag so monitoring or a simple `curl /health`
    /// shows the regression immediately.
    /// What: written once by `restore_indexes` in `start.rs` after warm-boot
    /// completes; read by the health handler. Protected by `Mutex` because
    /// `WarmBootSummary` contains non-atomic fields.
    /// Test: `health_surfaces_warmboot_summary` in server tests.
    pub warmboot_summary: Arc<std::sync::Mutex<WarmBootSummary>>,
    /// Count of indexes registered on the PREVIOUS successful daemon start,
    /// persisted to `daemon.env` (or a sibling file) so a fresh boot can
    /// compare its `indexes_loaded` against the prior-known count (issue #873).
    ///
    /// Why: `cargo install` silently drops FDA after changing the cdhash;
    /// without a prior-count baseline, the daemon has no way to know whether
    /// loading only 2 of 102 indexes is expected or a regression.
    /// What: an `AtomicUsize` loaded from `prior_index_count.txt` in
    /// `daemon_dir()` at startup by `start.rs`, written by the same module
    /// after warm-boot completes. `0` = no prior run known (first run).
    /// Test: `health_emits_fda_hint_when_loaded_below_prior_count`.
    pub prior_index_count: Arc<std::sync::atomic::AtomicUsize>,
    /// Cached RSS (MB) from the most recent successful `sys_metrics.sample()`.
    ///
    /// Why (issue #1016): when `sys_metrics.try_lock()` fails in the health
    /// handler (i.e. another health poll is concurrently sampling), returning 0
    /// causes monitors that alert on `rss_mb == 0` to false-alarm. Caching the
    /// last good sample and returning it on contention prevents false alarms.
    /// What: an `AtomicU64` written on every successful `SysMetrics::sample()`
    /// call; read by `health_handler` as a fallback when `try_lock()` fails.
    /// `0` only on the very first health poll before any sample has ever landed.
    /// Test: `health_rss_fallback_on_contention` in tests_state.rs.
    pub last_rss_mb: Arc<std::sync::atomic::AtomicU64>,
    /// Cached CPU percentage (f32 bits) from the last successful sample.
    ///
    /// Why (issue #1016): same rationale as `last_rss_mb` — avoids returning
    /// `cpu_pct == 0.0` when the metrics lock is transiently contended.
    /// What: stores `f32::to_bits()` in an `AtomicU32`; `health_handler` reads
    /// it via `f32::from_bits()` on contention.
    /// Test: `health_rss_fallback_on_contention` in tests_state.rs.
    pub last_cpu_pct_bits: Arc<std::sync::atomic::AtomicU32>,
}

/// Per-boot summary of warm-boot index loading, surfaced on `GET /health`.
///
/// Why (issue #873): `cargo install` changes the binary cdhash and silently
/// revokes macOS TCC Full Disk Access, causing the daemon to load only 2 of
/// ~102 indexes. Making this visible on `/health` turns a silent degradation
/// into a loud, machine-readable signal.
/// What: counts of loaded and skipped indexes split by skip reason; a boolean
/// `warm_boot_degraded` flag set when at least one TCC-skip happened or when
/// loaded < 80% of prior known count.
/// Test: `health_surfaces_warmboot_summary` in server tests.
#[derive(Clone, Default, serde::Serialize)]
pub struct WarmBootSummary {
    /// Number of indexes successfully loaded during warm-boot.
    pub indexes_loaded: usize,
    /// Number of indexes skipped because their volume was TCC-denied
    /// (PermissionDenied error or probe timeout on an external volume).
    pub indexes_skipped_tcc: usize,
    /// Number of indexes skipped due to timeout (not TCC — slow or
    /// network-backed filesystem).
    pub indexes_skipped_timeout: usize,
    /// `true` when `indexes_skipped_tcc > 0` OR when `indexes_loaded` is
    /// less than 80% of the prior-known count (suggesting a large fraction
    /// of indexes are missing, e.g. after FDA was revoked).
    pub warm_boot_degraded: bool,
}
