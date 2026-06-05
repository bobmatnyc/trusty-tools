//! HTTP daemon: axum router exposing the trusty-search REST API.
//!
//! Why: Single shared `SearchAppState` (wrapped in `Arc`) lets every handler
//! read from the `IndexRegistry` concurrently. `DashMap` shard-locks per index
//! so different indexes never contend, and `Arc<RwLock<CodeIndexer>>` allows
//! many simultaneous readers per index.
//!
//! What: Routes implement the API described in `CLAUDE.md`:
//! - `GET /health`
//! - `GET /indexes`                       list registered indexes
//! - `POST /indexes`                      register a new (empty) index
//! - `GET /indexes/:id/status`            chunk count + root path
//! - `POST /indexes/:id/search`           hybrid search
//! - `POST /indexes/:id/index-file`       add/update one file
//! - `POST /indexes/:id/remove-file`      drop a file's chunks
//! - `POST /indexes/:id/reindex`          fire-and-forget full reindex
//!
//! Test: `cargo test -p trusty-search-service` boots the router with an
//! in-process registry and exercises each endpoint.

use crate::core::{
    classifier::QueryClassifier,
    embed::Embedder,
    indexer::SearchQuery,
    registry::{IndexHandle, IndexId, IndexRegistry},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect, Response},
    routing::{delete, get, post},
    Router,
};
use dashmap::DashMap;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::{broadcast, watch, OnceCell, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use trusty_common::{ChatProvider, LocalModelConfig};

use crate::service::reindex::{spawn_reindex_with_cleanup, ReindexProgress, ReindexStatus};

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
    /// Cached result of the startup update check (issue #537).
    ///
    /// Why: `/health` should report `update_available` without hitting crates.io
    /// on every probe. A single background check at daemon startup stores the
    /// result here; the health handler reads it without a network call.
    /// What: `None` = up-to-date or check not yet done; `Some("x.y.z")` = newer
    /// version available. Populated by a `tokio::spawn` in `start.rs`.
    /// Test: indirectly by the `/health` endpoint tests in this module.
    pub update_available: Arc<std::sync::Mutex<Option<String>>>,
}

impl SearchAppState {
    /// Convenience constructor for callers (`daemon`, tests) that want default
    /// reindex tracking without hand-rolling the `Arc<DashMap<…>>`. Defaults
    /// to BM25-only mode (no embedder); use [`Self::with_embedder`] to enable
    /// the vector lane.
    pub fn new(registry: IndexRegistry) -> Self {
        let openrouter_api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
        let (events_tx, _) = broadcast::channel::<DaemonEvent>(128);
        // Default-constructed state has no embedder and the readiness watch
        // stays at `false`. Tests exercising BM25-only paths use this default.
        // Production daemon boot overrides via `with_embedder_ready_channel`
        // so the background init task can flip readiness once the model loads.
        let (ready_tx, ready_rx) = watch::channel(false);
        Self {
            registry,
            reindex_progress: Arc::new(DashMap::new()),
            last_reindex_aborted_at: Arc::new(DashMap::new()),
            embedder: None,
            embedder_slot: Arc::new(RwLock::new(None)),
            embedder_ready: ready_rx,
            embedder_ready_tx: Arc::new(ready_tx),
            embedder_error: Arc::new(RwLock::new(None)),
            daemon_port: None,
            openrouter_enabled: !openrouter_api_key.is_empty(),
            started_at: Instant::now(),
            local_model: LocalModelConfig::default(),
            openrouter_model: "anthropic/claude-haiku-4.5".to_string(),
            openrouter_api_key,
            chat_provider: Arc::new(OnceCell::new()),
            events: Arc::new(events_tx),
            // Default to an empty buffer — `build_router` callers that have
            // installed the `LogBufferLayer` override this via
            // `with_log_buffer`. Test states keep the empty default.
            log_buffer: trusty_common::log_buffer::LogBuffer::new(
                trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
            ),
            disk_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sys_metrics: Arc::new(tokio::sync::Mutex::new(
                trusty_common::sys_metrics::SysMetrics::new(),
            )),
            embed_pool: Arc::new(RwLock::new(None)),
            metrics: None,
            embedderd_pid_slot: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            update_available: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Builder-style: attach a pre-built embedder worker pool (issue #41
    /// Phase 1). Production callers (`start.rs`) build the pool once the
    /// background embedder-init task completes; tests can skip this.
    #[must_use]
    pub fn with_embed_pool(self, pool: Arc<crate::service::embed_pool::EmbedPool>) -> Self {
        if let Ok(mut slot) = self.embed_pool.try_write() {
            *slot = Some(pool);
        }
        self
    }

    /// Builder-style: attach the Prometheus recorder handle (issue #41
    /// Phase 1). Calling this enables the `/metrics` route in `build_router`.
    #[must_use]
    pub fn with_metrics(mut self, metrics: crate::service::metrics::MetricsState) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Install the pool after the deferred embedder init completes.
    ///
    /// Why: the embedder pool must be built *after* `install_embedder` has
    /// populated `embedder_slot` — otherwise the pool's workers would hold a
    /// reference to the unloaded model. Calling this from the spawned init
    /// task keeps the wiring atomic from the caller's perspective.
    /// What: writes the pool into the shared `Arc<RwLock<…>>`. Handlers
    /// observe the change on their next `embed_pool.read().await.clone()`.
    /// Test: hand-checked via the integration `start_brings_pool_online`
    /// scenario.
    pub async fn install_embed_pool(&self, pool: Arc<crate::service::embed_pool::EmbedPool>) {
        let mut slot = self.embed_pool.write().await;
        *slot = Some(pool);
    }

    /// Snapshot the currently-installed embed pool (or `None` while the
    /// embedder is still warming up).
    pub async fn current_embed_pool(&self) -> Option<Arc<crate::service::embed_pool::EmbedPool>> {
        self.embed_pool.read().await.clone()
    }

    /// Builder-style: attach the daemon's shared [`LogBuffer`] so the
    /// `GET /logs/tail` endpoint serves the same lines the tracing subscriber
    /// captures.
    ///
    /// Why (issue #35): `start.rs` builds the buffer (via
    /// `init_tracing_with_buffer`) before constructing the `SearchAppState`,
    /// then hands a clone here so the HTTP handler and the tracing layer
    /// observe the same ring.
    /// What: replaces the empty default buffer with the supplied one.
    /// Test: `logs_tail_returns_recent_lines`.
    #[must_use]
    pub fn with_log_buffer(mut self, buffer: trusty_common::log_buffer::LogBuffer) -> Self {
        self.log_buffer = buffer;
        self
    }

    /// Send a `DaemonEvent` to all connected SSE subscribers.
    ///
    /// Why: Best-effort fan-out — `broadcast::Sender::send` only fails when
    /// there are no live receivers, which is fine (no listeners == no work).
    /// What: Drops the result, callers don't need to check anything.
    /// Test: `emit_propagates_to_subscriber` subscribes then emits and asserts
    /// the event arrives.
    pub fn emit(&self, event: DaemonEvent) {
        let _ = self.events.send(event);
    }

    /// Builder-style: install user-loaded `local_model` settings (e.g. from
    /// `~/.trusty-search/config.toml`). Replaces the default Ollama address.
    pub fn with_local_model(mut self, cfg: LocalModelConfig) -> Self {
        self.local_model = cfg;
        self
    }

    /// Builder-style: override the OpenRouter model id (defaults to
    /// `anthropic/claude-haiku-4.5`).
    pub fn with_openrouter_model(mut self, model: impl Into<String>) -> Self {
        self.openrouter_model = model.into();
        self
    }

    /// Builder-style: set the OpenRouter API key (loaded from config or env).
    pub fn with_openrouter_api_key(mut self, api_key: impl Into<String>) -> Self {
        let api_key_str = api_key.into();
        self.openrouter_enabled = !api_key_str.is_empty();
        self.openrouter_api_key = api_key_str;
        self
    }

    /// Resolve the active chat provider, auto-detecting on first call.
    ///
    /// Why: Provider selection depends on (a) filesystem-loaded config and (b)
    /// a network probe to a local Ollama / LM Studio instance, so it must be
    /// lazily initialised at runtime. Caching the choice in a `OnceCell` keeps
    /// it stable across concurrent chat requests without re-probing.
    /// What: On first use prefers an auto-detected local server when
    /// `local_model.enabled`, otherwise falls back to OpenRouter when an API
    /// key is configured. Returns `None` when neither is available so the
    /// caller can emit a 503.
    /// Test: Covered by `chat_provider_endpoint_returns_payload` in this crate.
    pub async fn chat_provider(&self) -> Option<Arc<dyn ChatProvider>> {
        self.chat_provider
            .get_or_init(|| async {
                if self.local_model.enabled {
                    if let Some(mut p) =
                        trusty_common::auto_detect_local_provider(&self.local_model.base_url).await
                    {
                        p.model = self.local_model.model.clone();
                        return Some(Arc::new(p) as Arc<dyn ChatProvider>);
                    }
                }
                if !self.openrouter_api_key.is_empty() {
                    return Some(Arc::new(trusty_common::OpenRouterProvider::new(
                        self.openrouter_api_key.clone(),
                        self.openrouter_model.clone(),
                    )) as Arc<dyn ChatProvider>);
                }
                None
            })
            .await
            .clone()
    }

    /// Builder-style: record the actual port the daemon bound. Used by
    /// the UI handler to inject `window.__DAEMON_PORT__`.
    pub fn with_daemon_port(mut self, port: u16) -> Self {
        self.daemon_port = Some(port);
        self
    }

    /// Builder-style: attach a shared embedder so newly registered indexes
    /// run the full hybrid pipeline. The embedder is shared across every
    /// index registered after this point.
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(Arc::clone(&embedder));
        // Tests that wire a pre-built embedder expect the daemon to behave as
        // if init has already completed. Mirror that to the slot + watch so
        // handlers using the deferred-init path see a ready embedder too.
        if let Ok(mut slot) = self.embedder_slot.try_write() {
            *slot = Some(embedder);
        }
        let _ = self.embedder_ready_tx.send(true);
        self
    }

    /// Install the embedder produced by the background init task and flip the
    /// readiness watch to `true`.
    ///
    /// Why: the daemon starts serving HTTP before the embedder is loaded so
    /// readiness probes from `trusty-search start` / `trusty-search index`
    /// don't time out waiting for ONNX model load (15–30 s on first run).
    /// The init task calls this when `FastEmbedder::new()` completes; any
    /// in-flight handler observes the readiness flip via the watch channel.
    /// What: writes the embedder into `embedder_slot` and broadcasts `true`
    /// on `embedder_ready_tx` so all `*embedder_ready.borrow()` callers
    /// transition out of the "initializing" branch.
    /// Test: spawn a task that calls this after 1 s; assert `embedder_ready`
    /// flips and subsequent `POST /indexes` calls succeed.
    pub async fn install_embedder(&self, embedder: Arc<dyn Embedder>) {
        let mut slot = self.embedder_slot.write().await;
        *slot = Some(embedder);
        drop(slot);
        // Clear any previously recorded init error — the embedder is now ready.
        {
            let mut err = self.embedder_error.write().await;
            *err = None;
        }
        let _ = self.embedder_ready_tx.send(true);
    }

    /// Record a fatal embedder-init error so `/health` can surface it and
    /// `POST /indexes` can fail fast with a useful message instead of hanging
    /// on "initializing" forever.
    ///
    /// Why (issue #121): the background init task may abort because (a) ORT
    /// session init returned `Err` or (b) the init-timeout fired. In either
    /// case the embedder slot stays empty AND `embedder_ready` stays `false`;
    /// previously this was indistinguishable from "still loading", so callers
    /// retried forever. Capturing the error lets handlers and `/health`
    /// distinguish "transient" from "broken".
    /// What: writes the error message into `embedder_error`. Does NOT flip
    /// `embedder_ready` — `is_embedder_ready()` still returns `false`, so
    /// hybrid-pipeline code paths keep returning 503 rather than producing a
    /// BM25-only index by accident.
    /// Test: `install_embedder_error_surfaces_in_health` verifies the message
    /// is visible via `/health`.
    pub async fn install_embedder_error(&self, message: impl Into<String>) {
        let msg = message.into();
        tracing::error!("embedder init failed: {msg}");
        let mut err = self.embedder_error.write().await;
        *err = Some(msg);
    }

    /// Snapshot the current embedder-init error, if any. `None` means the
    /// background init task is still running or completed successfully.
    pub fn current_embedder_error(&self) -> Option<String> {
        self.embedder_error.try_read().ok().and_then(|g| g.clone())
    }

    /// Snapshot the currently-installed embedder (post-init) or `None` when
    /// the daemon is still warming up. Handlers prefer this over
    /// `self.embedder` so the deferred-init flow works transparently.
    pub async fn current_embedder(&self) -> Option<Arc<dyn Embedder>> {
        let slot = self.embedder_slot.read().await;
        slot.clone()
    }

    /// Cheap, non-blocking readiness check. Returns `true` once the
    /// background embedder-init task has flipped the watch channel.
    pub fn is_embedder_ready(&self) -> bool {
        *self.embedder_ready.borrow()
    }

    /// Install the live `child_pid_slot` Arc from the `EmbedderSupervisor`
    /// after the sidecar spawns (issue #282).
    ///
    /// Why: `build_embedder` in `start.rs` obtains the pid-slot Arc from
    /// `spawn_stdio` and calls this method from the background init task.
    /// The supervisor loop updates the same Arc on every respawn and clears
    /// it to 0 on final exit, so the daemon always reads the current PID
    /// without holding any lock.
    /// What: atomically copies the PID from the new slot into the field's
    /// existing Arc, then replaces the field's Arc with the new slot so
    /// future updates from the supervisor are visible directly.
    /// Test: `health_includes_embedderd_rss_field` in this module.
    pub async fn install_embedderd_pid_slot(&self, slot: Arc<std::sync::atomic::AtomicU32>) {
        use std::sync::atomic::Ordering;
        // Overwrite the AppState's Arc with the supervisor-owned Arc so all
        // subsequent reads — health handler, reindex poller — go through the
        // same object the supervisor loop writes to.
        // `Arc::swap` doesn't exist; use `AtomicU32` copy-then-pointer-replace via
        // a shared wrapper. Since the field is `Arc<AtomicU32>` (not `AtomicArc`),
        // we can't atomically replace the Arc pointer itself. The safest approach:
        // copy the current PID into the existing slot so any reader that already
        // holds a clone of the old Arc starts seeing the right value, AND then
        // atomically propagate future updates via a tiny background task.
        let initial_pid = slot.load(Ordering::Acquire);
        self.embedderd_pid_slot
            .store(initial_pid, Ordering::Release);
        // Keep in sync for future restarts: spawn a compact forwarder that
        // copies from the supervisor's Arc to the AppState's Arc every tick.
        // Uses 500 ms cadence — same as the reindex RSS poller.
        let src = Arc::clone(&slot);
        let dst = Arc::clone(&self.embedderd_pid_slot);
        tokio::spawn(async move {
            loop {
                let pid = src.load(Ordering::Acquire);
                dst.store(pid, Ordering::Release);
                if pid == 0 {
                    // Sidecar exited for the last time; stop forwarding.
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    /// Current OS PID of the embedderd sidecar, or `None` if no sidecar is
    /// running (in-process mode, sidecar not yet spawned, or sidecar exited).
    ///
    /// Why: the health handler uses this to sample the sidecar RSS; `0` is
    /// the "no process" sentinel.
    /// What: loads `embedderd_pid_slot` with `Relaxed` ordering — a slightly
    /// stale PID is fine (the caller will just get `None` from sysinfo if the
    /// process already exited).
    /// Test: see `health_includes_embedderd_rss_field`.
    pub fn current_embedderd_pid(&self) -> Option<u32> {
        use std::sync::atomic::Ordering;
        let pid = self.embedderd_pid_slot.load(Ordering::Relaxed);
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    indexes: usize,
    uptime_secs: u64,
    /// Why: operators previously had no way to tell whether the daemon
    /// loaded the embedding model. Silent BM25-only fallback wasted hours
    /// of debugging on "17k files indexed in 12 seconds" symptoms. Now
    /// `/health` reports `"ready"` when an embedder is attached and
    /// `"unavailable"` when the daemon is running BM25-only.
    /// What: `"ready"` if `state.embedder.is_some()` else `"unavailable"`.
    /// Test: start daemon, GET /health, assert `embedder == "ready"`.
    embedder: &'static str,
    /// Human-readable error message captured from a failed embedder-init task,
    /// surfaced alongside `embedder == "error"` (issue #121). `None` when the
    /// embedder is healthy or still warming up. Omitted from JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    embedder_error: Option<String>,
    /// Current process Resident Set Size in megabytes (issue #35). Sampled via
    /// the shared `SysMetrics` on each health request.
    rss_mb: u64,
    /// Soft RSS ceiling (MB) configured for the daemon's indexing pipeline —
    /// the `TRUSTY_MEMORY_LIMIT_MB` value resolved by `MemoryPolicy` (issue
    /// #35). `0` means no limit is configured. Lets operators see `rss_mb`
    /// relative to its budget without a second request.
    rss_limit_mb: u64,
    /// On-disk footprint of the daemon's data directory in bytes (issue #35):
    /// the sum of every redb / usearch / snapshot file. Refreshed by a
    /// background task every 10 s; `0` until the first walk completes.
    disk_bytes: u64,
    /// Current process CPU usage as a percentage (issue #35), where `100.0`
    /// means one fully-saturated core. Sampled via `SysMetrics`; the first
    /// reading after daemon start may be `0.0` until a delta window exists.
    cpu_pct: f32,
    /// Embedding-model detail block, populated once the embedder is ready
    /// (issue #38). `None` while the daemon is still warming up or running
    /// BM25-only. Lets the admin UI's Health view show the model name,
    /// vector dimension, and active ONNX execution provider without a second
    /// request.
    #[serde(skip_serializing_if = "Option::is_none")]
    embedder_info: Option<EmbedderInfo>,
    /// Current RSS of the `trusty-embedderd` sidecar process in megabytes
    /// (issue #282). `None` when the sidecar is not running (in-process mode,
    /// HTTP / UDS remote, not yet spawned, or sidecar exited). Sampled on
    /// each `/health` request using `current_rss_mb_for_pid`; the first
    /// health poll after a crash-restart may briefly return `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    embedderd_rss_mb: Option<u64>,
    /// Number of background (startup/auto-discover) reindex tasks currently
    /// queued or in-flight on the background semaphore (issue #458). Operators
    /// can watch this tick to zero to confirm the startup storm has drained.
    /// Interactive (user-initiated) reindexes are NOT counted here — they use
    /// a separate semaphore and are never blocked by this backlog.
    ///
    /// Why: without this field, an operator has no way to tell from `/health`
    /// whether the daemon is still processing its startup reindex storm.
    /// What: mirrors `reindex::background_reindex_queue_depth()`.
    /// Test: `health_includes_reindex_queue_depth` below.
    background_reindex_queue_depth: usize,
    /// Newer crates.io version available, if any (issue #537).
    ///
    /// Why: surfaces update availability without polling crates.io on every
    /// health call — a single background check at startup stores the result
    /// here for the health handler to read cheaply.
    /// What: `null`/absent = up to date or check not completed; `"x.y.z"` =
    /// the available newer version.
    #[serde(skip_serializing_if = "Option::is_none")]
    update_available: Option<String>,
    /// Running count of warm-boot probe threads abandoned due to a deadline
    /// timeout (review #727 finding 3, issue #723).
    ///
    /// Why: each timed-out volume probe leaks exactly one OS thread (a bare
    /// `std::thread::spawn` that is intentionally detached so a frozen
    /// `stat()` cannot consume a tokio pool slot). On a launchd-managed daemon
    /// that restarts repeatedly these can accumulate silently. Surfacing the
    /// count here lets operators detect accumulation before it matters —
    /// typically 0 on a healthy machine; >0 indicates one or more external
    /// volumes were TCC-blocked during this daemon's lifetime.
    /// What: mirrors `warm_boot::leaked_probe_thread_count()`. Zero means no
    /// probes have ever timed out in this process; N means N volumes timed out
    /// across all warm-boot phases since the daemon started.
    /// Test: `health_includes_warmboot_leaked_probe_threads` below.
    warmboot_leaked_probe_threads: usize,
    /// Whether at least one chat provider (OpenRouter or a verified local model)
    /// is actually available on this daemon instance (issue #781).
    ///
    /// Why: `LocalModelConfig::default()` has `enabled: true` (Ollama default),
    /// so using `local_model.enabled` alone made the flag `true` even when Ollama
    /// was not running and no OpenRouter key was configured — misleading the Svelte
    /// dashboard into showing a chat panel that immediately 503'd. The corrected
    /// semantics: `true` when `OPENROUTER_API_KEY` is set, OR when the lazy
    /// `chat_provider` OnceCell has resolved to `Some` (a local model was
    /// auto-detected and confirmed reachable on the first chat call). Reports
    /// `false` in all other cases (no key, local model default not yet probed).
    /// Use `/api/chat/providers` for a synchronous live reachability check.
    ///
    /// What: `state.openrouter_enabled || state.chat_provider.get().is_some_and(|p| p.is_some())`.
    ///
    /// Test: `health_chat_available_semantics` verifies the three cases.
    chat_available: bool,
}

/// Embedding-model metadata surfaced by `GET /health` (issue #38).
///
/// Why: the redesigned web UI's Health view shows which model is loaded, its
/// output dimension, and whether ONNX is dispatching to CPU / CoreML / CUDA.
/// Operators previously had to read the daemon startup log for this.
/// What: a small serialisable struct derived from the live `Arc<dyn Embedder>`
/// — `dimension` comes from `Embedder::dimension()`, `provider` from
/// `Embedder::provider()`, and `quantized` is inferred from the provider-
/// agnostic default model (the daemon ships the INT8 `AllMiniLML6V2Q`).
/// Test: `health_includes_embedder_info_when_ready` builds a state with a
/// `MockEmbedder` and asserts the block is present with a 384-dim value.
#[derive(Serialize)]
struct EmbedderInfo {
    /// Vector dimensionality reported by the embedder (384 for all-MiniLM-L6).
    dimension: usize,
    /// Active ONNX execution provider: `"CPU"`, `"CoreML"`, or `"CUDA"`.
    provider: String,
    /// Whether the loaded model is the INT8-quantized variant. The daemon
    /// defaults to `AllMiniLML6V2Q` (quantized); a missing quantized model
    /// falls back to full precision.
    quantized: bool,
}

#[derive(Serialize)]
struct IndexListResponse {
    indexes: Vec<String>,
}

/// Per-index entry returned by `GET /indexes?details=true` (issue #312).
///
/// Why: the flat list (`GET /indexes`) returns bare id strings for backward
/// compatibility; adding `?details=true` returns richer objects so the MCP
/// `list_indexes` tool and UI can display per-index disk usage without a
/// separate round-trip.  `root_path` is also exposed here so callers (e.g.
/// trusty-review's auto-derive logic, issue #661) can match an index to the
/// current project directory without a separate per-index status round-trip.
/// What: `id` + `root_path` (canonical absolute path stored on the handle) +
/// `size_bytes` (sum of all file sizes under the index data directory; `null`
/// when the directory has not been created yet).
/// Test: `list_indexes_details_includes_size_bytes`,
/// `list_indexes_details_includes_root_path`.
#[derive(serde::Serialize)]
struct IndexDetailEntry {
    id: String,
    /// Canonical absolute path of the indexed directory.
    ///
    /// Why: trusty-review and other callers need to map the current project
    /// root to an index without issuing N status requests (issue #661).
    /// What: the `root_path` stored on the `IndexHandle` at registration time,
    /// serialised as a UTF-8 string (lossless on all supported platforms).
    /// Absent (null) only when the handle's path cannot be converted to UTF-8
    /// (a practically impossible edge case on macOS / Linux).
    /// Test: `list_indexes_details_includes_root_path`.
    root_path: Option<String>,
    size_bytes: Option<u64>,
}

#[derive(Deserialize)]
pub struct CreateIndexRequest {
    pub id: String,
    pub root_path: std::path::PathBuf,
    /// Subtrees (relative to `root_path`) to restrict indexing to. Forwarded
    /// from `trusty-search.yaml`'s `paths:` field by `trusty-search index`.
    /// Empty / missing = walk the entire `root_path`.
    #[serde(default)]
    pub include_paths: Option<Vec<String>>,
    /// Glob patterns to exclude on top of the built-in ignores.
    #[serde(default)]
    pub exclude_globs: Option<Vec<String>>,
    /// Extension allow-list (e.g. `["rs", "py"]`, without leading dot).
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
    /// Domain vocabulary for the per-index intent classifier.
    #[serde(default)]
    pub domain_terms: Option<Vec<String>>,
    /// Glob patterns (issue #111) matched against the immediate subdirectory
    /// name under `root_path`. When non-empty, only files inside subdirectories
    /// whose basename matches at least one pattern are indexed. Supports `*`
    /// wildcards (no `**`). Distinct from `include_paths` (absolute subtrees
    /// from `trusty-search.yaml`) — `path_filter` is the API-level glob filter
    /// intended for filtering polyrepo monorepos by repo-name pattern.
    #[serde(default)]
    pub path_filter: Option<Vec<String>>,
    /// Index prose docs (`*.md`, `*.rst`, README, CHANGELOG, …) —
    /// issues #77 and #118. Default `None` is now treated as `true` (was
    /// `false` through v0.8.2); the per-mode filter
    /// (`is_allowed_for_mode`) keeps these chunks out of `mode=code`
    /// results, but `mode=text` needs them indexed at all. Set `false`
    /// from `trusty-search.yaml` only when the operator genuinely does
    /// not want any prose chunked (saves chunks on docs-heavy projects).
    #[serde(default)]
    pub include_docs: Option<bool>,
    /// Honour `.gitignore` (plus `.ignore`, `.rgignore`, `.git/info/exclude`,
    /// global gitignore) during the reindex walk — issue #100. Default
    /// `None` (treated as `true` by the daemon, matching ripgrep semantics).
    /// Set `false` from `trusty-search.yaml` when the operator wants to
    /// index a gitignored / vendored subtree on purpose.
    ///
    /// Why: previously the walker used `walkdir` and ignored `.gitignore`,
    /// which combined with the chunk budget caused silent partial-index
    /// failures — a gitignored subtree dominated the budget before the
    /// walker reached the real source. Exposing the toggle on the wire keeps
    /// the opt-out reachable for callers that need it.
    #[serde(default)]
    pub respect_gitignore: Option<bool>,
    /// Staged-pipeline opt-out (issue #109, Phase 1). When `true`, the
    /// reindex pipeline returns after Stage 1 (lexical / BM25 / redb) and
    /// permanently skips the embedder + symbol-graph stages. Useful for
    /// callers who want a daemonized ripgrep without the embedder overhead.
    /// Persisted to `indexes.toml` so the choice survives daemon restarts.
    /// Default `None` (treated as `false` — full pipeline).
    #[serde(default)]
    pub lexical_only: Option<bool>,

    /// Stage-1-minimal mode (issue #313): when `true`, the Phase 3 KG
    /// rebuild is skipped entirely during reindex. The graph stage is
    /// permanently `Skipped`. `get_call_chain` and `search_kg` return a
    /// 503 `kg_unavailable` error. Orthogonal to `lexical_only`.
    /// Default `None` (treated as `false` — KG is built as normal).
    ///
    /// Why: exposes the per-index KG-suppression flag on the wire so callers
    /// can register a skip_kg index in one `POST /indexes` call.
    /// What: `None` maps to `false`; `true` is stored in `indexes.toml` and
    /// survives daemon restarts.
    /// Test: `skip_kg_index_never_runs_phase3` in `service::reindex::tests`.
    #[serde(default)]
    pub skip_kg: Option<bool>,
}

#[derive(Deserialize)]
pub struct IndexFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct RemoveFileRequest {
    pub path: String,
}

/// Build a well-formed `404 Not Found` JSON response for an unknown index id.
///
/// Why (issue #750): per-index endpoints previously returned a bare 404 with
/// no body when the index id was not registered. Clients decode the body as
/// JSON and fail with `error decoding response body` instead of seeing a clear
/// "index not found" message. Centralising the response in one helper ensures
/// all `/indexes/{id}/*` routes are consistent.
/// What: `HTTP 404` with `Content-Type: application/json` and body
/// `{"error":"unknown index","index_id":"<id>"}`.
/// Test: `search_unknown_index_returns_404_json`,
/// `status_unknown_index_returns_404_json`, and similar per-handler tests.
fn unknown_index_response(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "unknown index",
            "index_id": id,
        })),
    )
        .into_response()
}

/// Build the axum router with the shared state.
///
/// Wraps `state` in an `Arc` so every handler clones the pointer cheaply.
pub fn build_router(state: SearchAppState) -> Router {
    use crate::service::ui::{
        chat_handler, list_chat_providers, ui_asset_handler, ui_index_handler,
    };
    // Why: Vite builds the UI bundle with `base: './'` so `index.html` references
    // assets via relative paths (e.g. `./assets/index-XXX.js`). When the browser
    // loads the page at `/ui` (no trailing slash) it resolves those relative
    // URLs against `/`, requesting `/assets/...` which 404s. Redirecting
    // `/ui` → `/ui/` forces the browser to use `/ui/` as the base so asset
    // requests land on `/ui/assets/...` and hit `ui_asset_handler`. The root
    // `/` redirect makes the daemon's landing page friendly (mirrors the
    // `.fallback(static_handler)` shape trusty-memory uses to serve its SPA
    // at `/`).
    let state_arc = Arc::new(state);
    spawn_status_ticker(Arc::clone(&state_arc));
    spawn_disk_size_ticker(Arc::clone(&state_arc));
    spawn_idle_chunk_eviction_ticker(Arc::clone(&state_arc));

    // Issue #41 Phase 1: concurrency limiter applied selectively to expensive
    // endpoints. Cheap endpoints (/health, /metrics, /indexes list, /ui/*)
    // bypass it entirely. The limiter is constructed once per router build
    // so its semaphore is shared across every request.
    let limiter = crate::service::concurrency::ConcurrencyLimiter::from_env();

    // "Expensive" subtree: search + reindex + index-file. These are routed
    // through the concurrency limiter middleware. The limiter is injected as
    // an Extension so the middleware fn can pull it out per-request.
    let limited = Router::new()
        .route("/search", post(global_search_handler))
        .route("/grep", post(global_grep_handler))
        .route("/indexes/{id}/grep", post(grep_handler))
        .route("/indexes/{id}/search", post(search_handler))
        .route("/indexes/{id}/search_similar", post(search_similar_handler))
        .route("/indexes/{id}/index-file", post(index_file_handler))
        .route("/indexes/{id}/remove-file", post(remove_file_handler))
        .route("/indexes/{id}/reindex", post(reindex_handler))
        .route_layer(axum::middleware::from_fn(
            crate::service::concurrency::apply_limiter,
        ))
        .layer(axum::Extension(Arc::clone(&limiter)))
        .with_state(Arc::clone(&state_arc));

    // "Free" subtree: light endpoints + streaming endpoints that should not
    // be queued behind heavy work (SSE streams hold the connection for the
    // lifetime of a reindex, so wrapping them in the limiter would deadlock).
    let free = Router::new()
        .route("/", get(|| async { Redirect::permanent("/ui/") }))
        .route("/health", get(health_handler))
        .route("/logs/tail", get(logs_tail_handler))
        .route("/admin/stop", post(admin_stop_handler))
        .route("/status/stream", get(status_stream_handler))
        .route(
            "/indexes",
            get(list_indexes_handler).post(create_index_handler),
        )
        .route("/indexes/{id}", delete(delete_index_handler))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(ui_index_handler))
        .route("/ui/{*path}", get(ui_asset_handler))
        .route("/chat", post(chat_handler))
        .route("/api/chat/providers", get(list_chat_providers))
        .route("/indexes/{id}/status", get(index_status_handler))
        .route("/indexes/{id}/graph", get(graph_handler))
        .route("/indexes/{id}/graph/stats", get(graph_stats_handler))
        .route("/indexes/{id}/reindex/stream", get(reindex_stream_handler))
        .route("/indexes/{id}/chunks", get(get_index_chunks_handler))
        .route("/indexes/{id}/call_chain", get(call_chain_handler))
        .route(
            "/config",
            get(get_config_handler).patch(patch_config_handler),
        )
        .route("/upgrade", post(upgrade_handler))
        .with_state(Arc::clone(&state_arc));

    let mut router = free.merge(limited);

    // Issue #41 Phase 1: install Prometheus /metrics endpoint if the recorder
    // has been wired into the state. Production callers (`start.rs`) install
    // the recorder before constructing the AppState and stash the resulting
    // handle here. Test/integration callers that skip the install simply
    // don't expose /metrics.
    if let Some(metrics_state) = state_arc.metrics.clone() {
        router = router
            .route("/metrics", get(crate::service::metrics::metrics_handler))
            .layer(axum::Extension(metrics_state));
    }

    // Wrap the entire router in the per-request metrics middleware so every
    // endpoint contributes to `trusty_requests_total` /
    // `trusty_request_latency_ms`. The recorder is a global, so emit macros
    // are no-ops when no recorder has been installed (test path).
    router = router.layer(axum::middleware::from_fn(
        crate::service::metrics::request_metrics_middleware,
    ));

    // Standard middleware stack (CORS, tracing, gzip) lives in trusty-common
    // so every trusty-* daemon ships with the same defaults.
    trusty_common::server::with_standard_middleware(router)
}

/// Spawn a background ticker that emits `StatusChanged` every 2 seconds.
///
/// Why: trusty-memory's pattern is push-driven via mutating handlers, but
/// trusty-search's headline stats (chunk count) change continuously during
/// reindex without a discrete event. A 2s ticker keeps the dashboard's
/// stat cards live (same cadence as the previous poll-based implementation)
/// while still routing through the broadcast channel so the SSE handler
/// stays purely subscription-driven.
/// What: Spawns a detached tokio task holding a `Weak<SearchAppState>` so
/// the ticker terminates automatically when the daemon shuts down (drops the
/// last `Arc`). Each tick recomputes counts and emits one event.
/// Test: subscribe to `/status/stream`, wait > 2s, observe a `status_changed`
/// frame.
fn spawn_status_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        // Skip the immediate first tick — subscribers get an explicit
        // `connected` frame, and a snapshot follows on the next tick.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let (indexes, total_chunks) = collect_status_counts(&state).await;
            state.emit(DaemonEvent::StatusChanged {
                indexes: indexes as u64,
                total_chunks: total_chunks as u64,
                uptime_secs: state.started_at.elapsed().as_secs(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            });
        }
    });
}

/// Spawn a background ticker that recomputes the data-directory size every
/// 10 seconds and stores it in `state.disk_bytes`.
///
/// Why (issue #35): `GET /health` reports `disk_bytes`. Walking the data
/// directory (redb + usearch + snapshot files) on every health request would
/// turn a 2 s health poll into unbounded recursive I/O. Computing it off the
/// request path on a fixed cadence keeps `/health` cheap and bounds the
/// staleness to ~10 s — fine for an at-a-glance footprint figure.
/// What: spawns a detached tokio task holding a `Weak<SearchAppState>` so the
/// ticker stops automatically when the daemon drops its last `Arc`. Each tick
/// runs the (blocking) directory walk on `spawn_blocking` so it never stalls
/// the async runtime, then stores the byte total atomically.
/// Test: covered indirectly — `health_includes_resource_fields` asserts the
/// `disk_bytes` field is present and non-negative.
fn spawn_disk_size_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            // The directory walk is blocking filesystem I/O — run it on the
            // blocking pool so it never parks an async worker thread.
            let bytes =
                tokio::task::spawn_blocking(|| match crate::service::persistence::data_dir() {
                    Ok(dir) => trusty_common::sys_metrics::dir_size_bytes(&dir),
                    Err(e) => {
                        tracing::debug!("disk_size_ticker: could not resolve data dir: {e}");
                        0
                    }
                })
                .await
                .unwrap_or(0);
            state
                .disk_bytes
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Spawn a background ticker that evicts each index's in-memory `chunks` map
/// after it has been idle past the configured window (issue #83 follow-up).
///
/// Why (idle-memory audit): the durable redb corpus already serves the query
/// hot path, so an index that hasn't been queried or ingested for a while is
/// holding hundreds of MB of `RawChunk` text in the process heap for nothing.
/// `CodeIndexer::evict_chunks_if_idle` reclaims that heap and lazily rehydrates
/// from redb on the next access; this ticker is what drives it on a fixed
/// cadence across every registered index. It mirrors the `spawn_*_ticker`
/// pattern: a detached task holding a `Weak<SearchAppState>` so it stops when
/// the daemon drops its last `Arc`.
/// What: every 60 s, resolves the idle window via
/// `crate::core::indexer::idle_evict_secs()` (env-overridable;
/// `0` disables eviction and the ticker idles), then walks the registry and
/// calls `evict_chunks_if_idle` on each indexer. The per-indexer call is itself
/// a no-op for active indexes, indexes without a durable corpus, and
/// already-empty maps, so the walk is cheap. The eviction acquires each
/// indexer's read lock only to check `corpus`/idle state and a brief write lock
/// only when it actually clears the map.
/// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks` covers the
/// per-indexer logic directly; the ticker is a thin scheduling wrapper.
fn spawn_idle_chunk_eviction_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        // Skip the immediate first tick so a freshly-started daemon isn't
        // evicting before it has served anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let secs = crate::core::indexer::idle_evict_secs();
            if secs == 0 {
                // Eviction disabled by env; keep ticking cheaply so an operator
                // re-enabling it (next process) is honoured without a restart
                // of this loop — but do no work this tick.
                continue;
            }
            let threshold = Duration::from_secs(secs);
            for id in state.registry.list() {
                let Some(handle) = state.registry.get(&id) else {
                    continue;
                };
                let indexer = handle.indexer.read().await;
                indexer.evict_chunks_if_idle(threshold).await;
            }
        }
    });
}

async fn health_handler(State(state): State<Arc<SearchAppState>>) -> Json<HealthResponse> {
    // Why: open-mpm (and other external integrators) probe `/health` to detect
    // a running trusty-search daemon before spawning their own. Including
    // `indexes` count lets the caller verify the daemon is not only alive but
    // also has the expected registry populated (issue #34).
    // What: returns `{ status, version, indexes, uptime_secs }` where
    // `indexes` is the number of registered IndexHandles in the registry
    // and `uptime_secs` is wall-clock seconds since AppState construction.
    // Test: register N indexes, GET /health, assert `indexes == N` and
    // `uptime_secs >= 0`.
    let embedder_error = state.current_embedder_error();
    let embedder_status = if state.is_embedder_ready() {
        "ready"
    } else if state.embedder.is_some()
        || state
            .embedder_slot
            .try_read()
            .map(|g| g.is_some())
            .unwrap_or(false)
    {
        // Slot populated but readiness flag not yet flipped — treat as ready.
        "ready"
    } else if embedder_error.is_some() {
        // Init task failed or timed out (issue #121). Callers must not retry
        // forever — report a terminal error state so operators can intervene.
        "error"
    } else {
        // Daemon is up but embedder still loading. Callers should retry
        // mutating endpoints; `/health` itself always returns 200 so
        // `trusty-search start`'s readiness probe succeeds quickly.
        "initializing"
    };
    // Issue #35: sample process RSS + CPU. The sampler is shared behind a
    // Mutex because sysinfo derives CPU% from the delta between refreshes.
    let (rss_mb, cpu_pct) = {
        let mut metrics = state.sys_metrics.lock().await;
        metrics.sample()
    };
    // `rss_limit_mb` mirrors the resolved TRUSTY_MEMORY_LIMIT_MB soft cap.
    // `memory_limit_mb()` returns `None` when no limit is configured.
    let rss_limit_mb = crate::core::memguard::memory_limit_mb().unwrap_or(0);
    let disk_bytes = state.disk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    // Issue #38: surface model detail (dimension + provider) once the embedder
    // is wired so the admin UI's Health view doesn't need a separate request.
    let embedder_info = state.current_embedder().await.map(|e| {
        let dimension = e.dimension();
        EmbedderInfo {
            dimension,
            provider: e.provider().as_str().to_string(),
            // The daemon defaults to the INT8-quantized AllMiniLML6V2Q model;
            // a 384-dim embedder is the quantized all-MiniLM-L6 variant.
            quantized: dimension == trusty_common::embedder::EMBED_DIM,
        }
    });
    // Issue #282: sample the sidecar's current RSS (None when not running).
    let embedderd_rss_mb = state
        .current_embedderd_pid()
        .and_then(crate::core::memguard::current_rss_mb_for_pid);
    let update_available = state.update_available.lock().ok().and_then(|g| g.clone());

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        indexes: state.registry.list().len(),
        uptime_secs: state.started_at.elapsed().as_secs(),
        embedder: embedder_status,
        embedder_error,
        rss_mb,
        rss_limit_mb,
        disk_bytes,
        cpu_pct,
        embedder_info,
        embedderd_rss_mb,
        // Issue #458: expose the background reindex backlog so operators can
        // watch the startup storm drain without reading daemon logs.
        background_reindex_queue_depth: crate::service::reindex::background_reindex_queue_depth(),
        update_available,
        // Review #727 finding 3: surface the count of probe threads abandoned
        // due to a warm-boot deadline timeout. Zero on healthy machines; >0
        // indicates TCC-blocked external volumes during this daemon's lifetime.
        warmboot_leaked_probe_threads: crate::service::warm_boot::leaked_probe_thread_count(),
        // Issue #781: `chat_available` reflects ACTUAL availability.
        // `LocalModelConfig::default()` has `enabled: true` (Ollama), so using
        // `local_model.enabled` alone was always true even when Ollama is not
        // running. The corrected logic:
        //   - `openrouter_enabled` → key is present, so OpenRouter will accept calls.
        //   - `chat_provider` OnceCell already resolved to `Some` → a local model
        //     was auto-detected and is alive (network probe ran on first chat call).
        // If no key and the OnceCell hasn't resolved yet (first boot, no chat calls),
        // we conservatively report `false` so the UI doesn't show a chat panel that
        // will immediately 503 on first use.
        chat_available: state.openrouter_enabled
            || state.chat_provider.get().is_some_and(|p| p.is_some()),
    })
}

/// Request body for `POST /upgrade` (issue #537).
///
/// Why: typed body avoids raw JSON field extraction in the handler, and serde
/// provides friendly error messages for malformed requests.
/// What: mirrors the MCP tool schema: `check` (default true) and `confirm`.
/// Test: the MCP `upgrade` tool calls this endpoint.
#[derive(Deserialize)]
struct UpgradeRequest {
    #[serde(default = "bool_true")]
    check: bool,
    #[serde(default)]
    confirm: bool,
}

/// `POST /upgrade` — check for or install a new trusty-search version (issue #537).
///
/// Why: Exposes the upgrade workflow over HTTP so the MCP dispatcher (which
/// calls the daemon's REST API) can trigger an upgrade and receive the response
/// before the daemon self-exits. Never silently auto-installs.
///
/// What:
/// - `check=true` or `confirm=false`: query crates.io and return version info.
/// - `confirm=true`: install via `cargo install --locked`, health-gate, then
///   schedule a 500 ms delayed exit (to flush this response) and return the
///   result. When launchd-supervised the daemon exits non-zero so launchd
///   respawns with the new binary. When unsupervised a restart hint is returned.
///
/// Test: manual via `curl -X POST http://127.0.0.1:$(trusty-search port)/upgrade \
///  -H 'Content-Type: application/json' -d '{"check":true}'`.
async fn upgrade_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(body): Json<UpgradeRequest>,
) -> Json<serde_json::Value> {
    let crate_name = env!("CARGO_PKG_NAME");
    let current = env!("CARGO_PKG_VERSION");

    let info = trusty_common::update::check_crates_io(crate_name, current).await;

    let (latest, is_update) = match &info {
        Some(u) => (u.latest.as_str(), true),
        None => (current, false),
    };

    if body.check || !body.confirm {
        let msg = if is_update {
            format!(
                "Update available: {crate_name} {latest} (you have {current}). \
                 POST with confirm=true to install."
            )
        } else {
            format!("{crate_name} {current} is already up to date.")
        };
        return Json(serde_json::json!({
            "status": "checked",
            "current": current,
            "latest": latest,
            "update_available": is_update,
            "message": msg
        }));
    }

    if !is_update {
        return Json(serde_json::json!({
            "status": "up_to_date",
            "current": current,
            "message": format!("{crate_name} {current} is already up to date.")
        }));
    }

    let latest_owned = latest.to_string();
    let crate_name_owned = crate_name.to_string();
    let update_slot = state.update_available.clone();
    let response = serde_json::json!({
        "status": "installing",
        "current": current,
        "latest": latest_owned,
        "message": format!(
            "Installing {crate_name} {latest_owned} — daemon will restart \
             under launchd (or print a restart hint if not supervised)."
        )
    });

    // Spawn the install on a delayed task so this handler can return the
    // response to the HTTP client (and thus to the MCP caller) before the
    // process might exit. 500 ms gives the TCP stack time to flush.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        match trusty_common::update::upgrade_and_restart(&crate_name_owned, &crate_name_owned).await
        {
            Ok(Some(hint)) => {
                tracing::info!("{hint}");
                eprintln!("{hint}");
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("upgrade_and_restart failed: {e:#}");
                eprintln!("[trusty-search] upgrade failed: {e:#}");
                if let Ok(mut g) = update_slot.lock() {
                    *g = None;
                }
            }
        }
    });

    Json(response)
}

fn bool_true() -> bool {
    true
}

/// Query parameters for `GET /logs/tail`.
///
/// Why (issue #35): callers ask for a bounded number of recent log lines;
/// `n` defaults to a useful page size and is clamped server-side so a
/// misconfigured client cannot request more lines than the buffer holds.
/// What: `n` is optional; absent → [`DEFAULT_LOGS_TAIL_N`]. Clamped to
/// `[1, MAX_LOGS_TAIL_N]` in the handler.
/// Test: `logs_tail_clamps_n` exercises the clamp.
#[derive(Deserialize)]
pub struct LogsTailParams {
    #[serde(default = "default_logs_tail_n")]
    pub n: usize,
}

/// Default number of log lines returned by `GET /logs/tail` when `n` is
/// absent. 100 lines is enough context for a glance without a huge payload.
const DEFAULT_LOGS_TAIL_N: usize = 100;

/// Hard ceiling on `GET /logs/tail?n=` — equal to the ring-buffer capacity,
/// so a request can never ask for more lines than the buffer can hold.
const MAX_LOGS_TAIL_N: usize = trusty_common::log_buffer::DEFAULT_LOG_CAPACITY;

fn default_logs_tail_n() -> usize {
    DEFAULT_LOGS_TAIL_N
}

/// `GET /logs/tail?n=200` — return the most recent N tracing log lines.
///
/// Why (issue #35): operators debugging a running daemon want recent logs
/// over HTTP without SSHing to the box or restarting with a different
/// `RUST_LOG`. The in-memory ring buffer (fed by the `LogBufferLayer` wired
/// into the subscriber at startup) makes this near-free.
/// What: clamps `n` to `[1, MAX_LOGS_TAIL_N]`, drains the tail of
/// `state.log_buffer`, and returns `{ "lines": [...], "total": <buffered> }`
/// where `total` is the number of lines currently buffered (so callers can
/// tell whether the ring has wrapped).
/// Test: `logs_tail_returns_recent_lines` and `logs_tail_clamps_n`.
async fn logs_tail_handler(
    State(state): State<Arc<SearchAppState>>,
    Query(params): Query<LogsTailParams>,
) -> Json<serde_json::Value> {
    let n = params.n.clamp(1, MAX_LOGS_TAIL_N);
    let lines = state.log_buffer.tail(n);
    Json(serde_json::json!({
        "lines": lines,
        "total": state.log_buffer.len(),
    }))
}

/// `POST /admin/stop` — request a graceful shutdown of the daemon.
///
/// Why (issue #35): the admin UI and operators want a one-call way to stop
/// the daemon without resolving its PID and sending a signal. The daemon is
/// localhost-only and trusts every caller, so no auth is required.
/// What: spawns a detached task that sleeps 200 ms (giving this HTTP response
/// time to flush to the client) and then calls `std::process::exit(0)`.
/// Returns `{ "ok": true, "message": "shutting down" }` immediately.
/// Test: `admin_stop_returns_ok` asserts the response shape (it does not
/// drive the real exit — that would terminate the test process).
async fn admin_stop_handler(State(_state): State<Arc<SearchAppState>>) -> Json<serde_json::Value> {
    tracing::warn!("admin_stop: shutdown requested via POST /admin/stop");
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true, "message": "shutting down" }))
}

/// Request body for `PATCH /config`. Any field may be omitted to leave that
/// limit unchanged; an explicit `null` disables the limit. Unknown JSON keys
/// are tolerated (serde's default `deny_unknown_fields` is off) so future
/// versions can introduce new keys without breaking older clients.
///
/// Why: backs `trusty-search config set <key> <value>` — operators tune the
/// daemon's memory limits without dropping the embedder model or any indexes
/// (which a full restart would cost). Patch semantics are the right HTTP
/// shape because only the fields the client cares about are sent.
/// What: serde flags distinguish "absent" (`Option::None`, leave alone) from
/// "explicit null" (`Some(None)`, disable). We use the
/// `serde_with::rust::double_option` idiom by representing each field as
/// `Option<Option<u64>>`.
/// Test: `tests::patch_config_partial_update` exercises both arms.
#[derive(Debug, Deserialize, Default)]
struct PatchConfigRequest {
    #[serde(default, deserialize_with = "deserialize_optional_option_u64")]
    memory_limit_mb: Option<Option<u64>>,
    #[serde(default, deserialize_with = "deserialize_optional_option_u64")]
    index_memory_limit_mb: Option<Option<u64>>,
}

/// Response body for `GET /config` and `PATCH /config`.
///
/// Why: always returns the resolved current values for both limits after any
/// changes have been applied. Lets the CLI print "before → after" without
/// issuing a follow-up GET.
/// What: `null` field means the limit is disabled (no cap). Field names match
/// the env-var-derived keys (`memory_limit_mb` / `index_memory_limit_mb`) for
/// symmetry with the request body.
#[derive(Debug, Serialize)]
struct ConfigResponse {
    memory_limit_mb: Option<u64>,
    index_memory_limit_mb: Option<u64>,
}

/// Custom deserializer for `Option<Option<u64>>` so we can tell "field absent"
/// (no change) from "field present and null" (disable the limit). Serde's
/// default skips `null` for `Option<u64>`, collapsing both cases — we need to
/// preserve the distinction to support partial updates.
///
/// Why: PATCH semantics require this three-state encoding.
/// What: returns `Some(Some(n))` for a numeric value, `Some(None)` for null,
/// and the outer `Option::None` is supplied by `#[serde(default)]` when the
/// field is absent entirely.
/// Test: `tests::patch_config_partial_update`.
fn deserialize_optional_option_u64<'de, D>(deserializer: D) -> Result<Option<Option<u64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<u64>::deserialize(deserializer)?;
    Ok(Some(v))
}

/// `GET /config` — return the daemon's current memory-limit configuration.
///
/// Why: `trusty-search config get` reads this to print the live values, which
/// may differ from what's in `daemon.env` if the operator has already issued
/// a `PATCH /config` call. Pure read; no side effects.
/// What: snapshots `memory_limit_mb()` and `index_memory_limit_mb()` and
/// returns them as JSON. `null` means "no limit configured".
/// Test: `tests::get_config_returns_current_values`.
async fn get_config_handler(State(_state): State<Arc<SearchAppState>>) -> Json<ConfigResponse> {
    use crate::core::memguard::{index_memory_limit_mb, memory_limit_mb};
    Json(ConfigResponse {
        memory_limit_mb: memory_limit_mb(),
        index_memory_limit_mb: index_memory_limit_mb(),
    })
}

/// `PATCH /config` — update the daemon's runtime memory-limit configuration.
///
/// Why: lets `trusty-search config set <key> <value>` retune memory limits
/// without a daemon restart (preserves the 86 MB embedder session and all
/// loaded indexes). The `AtomicU64` cells in `core::memguard` mean the
/// background memory poller observes the change on its next tick.
/// What: applies `memory_limit_mb` and/or `index_memory_limit_mb` from the
/// request body, logs each change at `INFO`, and returns the resolved
/// post-update values. Omitted fields are not touched. `null` disables the
/// corresponding limit. Always returns `200 OK` with a `ConfigResponse`.
/// Test: `tests::patch_config_partial_update` and
/// `tests::patch_config_disables_limit_with_null`.
async fn patch_config_handler(
    State(_state): State<Arc<SearchAppState>>,
    Json(req): Json<PatchConfigRequest>,
) -> Json<ConfigResponse> {
    use crate::core::memguard::{
        index_memory_limit_mb, memory_limit_mb, set_index_memory_limit_mb, set_memory_limit_mb,
    };

    let fmt = |v: Option<u64>| match v {
        Some(mb) => mb.to_string(),
        None => "unlimited".to_string(),
    };

    if let Some(new) = req.memory_limit_mb {
        let before = memory_limit_mb();
        set_memory_limit_mb(new);
        let after = memory_limit_mb();
        tracing::info!(
            "config updated: memory_limit_mb {} → {}",
            fmt(before),
            fmt(after)
        );
    }
    if let Some(new) = req.index_memory_limit_mb {
        let before = index_memory_limit_mb();
        set_index_memory_limit_mb(new);
        let after = index_memory_limit_mb();
        tracing::info!(
            "config updated: index_memory_limit_mb {} → {}",
            fmt(before),
            fmt(after)
        );
    }

    Json(ConfigResponse {
        memory_limit_mb: memory_limit_mb(),
        index_memory_limit_mb: index_memory_limit_mb(),
    })
}

/// Snapshot used by both `/health` (one-shot) and `/status/stream` (SSE tick).
///
/// Why: The dashboard needs live counts of registered indexes + total chunks
/// across the whole daemon. Computing this requires acquiring a read-lock on
/// every indexer, so the work is centralised here to keep the SSE loop tidy.
/// What: Returns `(indexes_count, total_chunks)` summed across the registry.
/// Test: Register two indexes seeded with one file each; the helper returns
/// `(2, chunks_in_file_a + chunks_in_file_b)`.
async fn collect_status_counts(state: &SearchAppState) -> (usize, usize) {
    let ids = state.registry.list();
    let indexes_count = ids.len();
    let mut total_chunks: usize = 0;
    for id in ids {
        if let Some(handle) = state.registry.get(&id) {
            let indexer = handle.indexer.read().await;
            // Issue #681: prefer durable corpus count (accurate post-eviction).
            let count = indexer
                .corpus_arc()
                .and_then(|c| c.chunk_count().ok())
                .unwrap_or_else(|| indexer.chunk_count());
            total_chunks = total_chunks.saturating_add(count);
        }
    }
    (indexes_count, total_chunks)
}

/// `GET /status/stream` — Server-Sent Events stream of live daemon stats.
///
/// Why: The admin dashboard's headline stat cards (Indexes, Documents,
/// Uptime, Version) should update without a manual refresh. Mirrors the
/// trusty-memory `/sse` pattern — subscribers receive `DaemonEvent` frames
/// pushed via the shared `broadcast::Sender` on `SearchAppState`.
/// What: Subscribes to `state.events`, emits an initial `{"type":"connected"}`
/// frame, then forwards every `DaemonEvent` as `data: <json>\n\n`. Lagged
/// subscribers receive a `{"type":"lag","skipped":N}` frame. The 2s status
/// cadence is supplied by the background ticker spawned in `build_router`.
/// Test: `curl -N http://127.0.0.1:7878/status/stream` shows a `connected`
/// frame immediately and a `status_changed` frame every ~2s.
async fn status_stream_handler(State(state): State<Arc<SearchAppState>>) -> impl IntoResponse {
    let rx = state.events.subscribe();
    let initial = stream::once(async {
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

    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .expect("valid SSE response")
}

/// Query parameters accepted by `GET /indexes`.
///
/// Why: the `?format=tree` variant returns hierarchy metadata (parent/child
/// relationships derived from `root_path` prefix containment) without breaking
/// the default flat-string response that existing callers depend on.
/// What: `format = "tree"` → object-array response; any other value (or
/// absent) → the existing `{ "indexes": ["id1", "id2"] }` flat response.
/// Test: `list_indexes_tree_format_shape`, `list_indexes_flat_default_unchanged`,
/// and `list_indexes_details_includes_size_bytes`.
#[derive(Deserialize, Default)]
struct ListIndexesParams {
    #[serde(default)]
    format: Option<String>,
    /// Issue #312: when `true`, return `[{id, size_bytes}]` objects instead of
    /// bare strings so callers can display per-index disk usage.
    #[serde(default)]
    details: bool,
}

/// `GET /indexes[?format=tree][?details=true]` — list registered indexes.
///
/// Why: the default flat format is byte-compatible with today's response so
/// existing callers (CLI, MCP, integrators) see no breaking change.  The
/// optional `?format=tree` variant exposes the index hierarchy derived from
/// `root_path` prefix containment (#404 MVP).  The optional `?details=true`
/// variant returns `[{id, size_bytes}]` objects so callers can show per-index
/// disk usage without a separate status round-trip (#312).
/// What: without query params, returns `{ "indexes": ["id1", …] }`.
/// With `?format=tree`, returns object array with hierarchy fields.
/// With `?details=true`, returns `{ "indexes": [{"id": …, "size_bytes": …}] }`.
/// Test: `list_indexes_flat_default_unchanged`, `list_indexes_tree_format_shape`,
/// `list_indexes_details_includes_size_bytes`.
async fn list_indexes_handler(
    State(state): State<Arc<SearchAppState>>,
    Query(params): Query<ListIndexesParams>,
) -> Response {
    let want_tree = params
        .format
        .as_deref()
        .map(|f| f == "tree")
        .unwrap_or(false);

    if want_tree {
        let handles = state.registry.list_handles();
        let entries = crate::core::search::hierarchy::build_tree_entries(&state.registry, &handles);
        Json(serde_json::json!({ "indexes": entries })).into_response()
    } else if params.details {
        // Issue #312: return per-index disk usage alongside each id.
        // Issue #661: also include root_path so callers can derive the index
        // from the current project directory without N status round-trips.
        let entries: Vec<IndexDetailEntry> = state
            .registry
            .list_handles()
            .into_iter()
            .map(|handle| {
                let (size_bytes, _) = index_disk_and_mtime(&handle.id.0);
                let root_path = handle.root_path.to_str().map(|s| s.to_string());
                IndexDetailEntry {
                    id: handle.id.0.clone(),
                    root_path,
                    size_bytes,
                }
            })
            .collect();
        Json(serde_json::json!({ "indexes": entries })).into_response()
    } else {
        Json(IndexListResponse {
            indexes: state.registry.list().into_iter().map(|id| id.0).collect(),
        })
        .into_response()
    }
}

async fn create_index_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(mut req): Json<CreateIndexRequest>,
) -> Response {
    let id = IndexId::new(req.id.clone());
    // Issue #63: validate root_path is absolute and points at an existing
    // directory before registering. Previously the handler accepted any
    // `PathBuf` the client supplied, so a relative path (e.g. `claude-mpm`)
    // was silently resolved against the daemon's startup CWD by every
    // downstream walker / file reader — producing an index whose root
    // pointed at the wrong project on disk. Rejecting non-absolute or
    // non-directory paths up front gives the caller a clear error and
    // prevents the bleed described in #64.
    //
    // Issue (indexed-paths-mismatch): the validator also returns the
    // *canonical* (symlink-resolved) form. We replace `req.root_path` with
    // it so every downstream consumer — the persistence layer, the indexer's
    // root reference, `include_paths` joins, git-head probing, and the
    // registry handle — stores a single canonical identity for the project.
    // Without this, registering via `/Users/foo` when that's a symlink to
    // `/Volumes/Kemono/...` stored the symlink path and search queries from
    // the canonical mount returned zero hits.
    let canonical_root = match validate_root_path(&req.root_path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    req.root_path = canonical_root;

    // Issue #767: opt-in allowlist — default-deny + hard sensitive-path denylist.
    //
    // Skip the allowlist check in two cases:
    // 1. `cfg!(test)` — unit tests call this handler directly with synthetic
    //    temp-dir paths that cannot be in the real allowlist; bypassing the
    //    check lets them test the surrounding logic (path validation, embedder
    //    readiness, registry state) without coupling to the on-disk config.
    // 2. `TRUSTY_ALLOW_UNLISTED=1` env var — operators or scripts that need
    //    to pre-populate an index registry during controlled setup can opt out
    //    of the allowlist check at runtime. NEVER set this on a production
    //    host that handles untrusted input.
    let skip_allowlist = cfg!(test)
        || std::env::var("TRUSTY_ALLOW_UNLISTED")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
    if !skip_allowlist {
        match crate::allowlist::check_path(&req.root_path, None) {
            Ok(crate::allowlist::AllowlistCheck::Allowed) => {
                // Path is explicitly allowlisted and safe — proceed.
            }
            Ok(crate::allowlist::AllowlistCheck::Denied { reason }) => {
                tracing::warn!(
                    path = %req.root_path.display(),
                    reason = %reason,
                    "POST /indexes rejected: sensitive path"
                );
                return (
                    axum::http::StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": format!(
                            "path '{}' matches a sensitive-path denylist pattern and cannot be indexed: {}",
                            req.root_path.display(), reason
                        )
                    })),
                )
                    .into_response();
            }
            Ok(crate::allowlist::AllowlistCheck::NotAllowlisted) => {
                tracing::warn!(
                    path = %req.root_path.display(),
                    "POST /indexes rejected: path not in allowlist (~/.config/trusty-search/indexes.toml)"
                );
                return (
                    axum::http::StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": format!(
                            "path '{}' is not in the allowlist. \
                             Run `trusty-search index add {}` to approve it, \
                             or add an [[index]] entry to ~/.config/trusty-search/indexes.toml.",
                            req.root_path.display(), req.root_path.display()
                        )
                    })),
                )
                    .into_response();
            }
            Err(e) => {
                tracing::error!(
                    path = %req.root_path.display(),
                    error = %e,
                    "POST /indexes: failed to read allowlist"
                );
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("could not check allowlist: {e}")
                    })),
                )
                    .into_response();
            }
        }
    }

    if state.registry.get(&id).is_some() {
        return Json(serde_json::json!({
            "id": req.id,
            "created": false,
            "reason": "already exists",
        }))
        .into_response();
    }
    // Why (issue: 10s readiness timeout): the embedder may still be loading
    // when the daemon accepts its first request. Reject hybrid-index creation
    // with `503 Service Unavailable` so the caller (`trusty-search index`)
    // retries instead of producing a BM25-only index that will quietly miss
    // the vector lane forever.
    let Some(embedder) = state.current_embedder().await else {
        // Issue #121: distinguish "still warming up" from "init failed
        // permanently". When the background task has recorded an error,
        // surface it in the 503 so callers stop polling and operators see
        // a useful message in logs / dashboards.
        if let Some(err) = state.current_embedder_error() {
            return embedder_error_response(&err);
        }
        return embedder_initializing_response();
    };
    // Bug A fix: when an embedder is attached to the shared state, wire the
    // newly created indexer with both an `Embedder` and a `VectorStore` so
    // the HNSW lane actually contributes results. Previously every index
    // was BM25-only because `with_components` was never called, which is
    // why the benchmark observed `match_reason: "bm25"` for 100% of hits.
    //
    // Issue #85: if a previously-saved HNSW snapshot + chunks file exist for
    // this id, restore them so the daemon warm-boots without re-indexing.
    //
    // Fix #483/#485: use `build_indexer_from_entry` with `colocated: true`
    // instead of `build_indexer_with_persisted_state` (which hard-codes
    // `colocated: false`).  The entry-aware builder routes the corpus store
    // to `<root>/.trusty-search/index.redb` via `corpus_redb_path_for_entry`,
    // and crucially `colocated_redb_path` → `colocated_storage_dir` calls
    // `create_dir_all` — so the `.trusty-search/` directory exists on-disk
    // BEFORE the first reindex.  Every write-path probe
    // (`has_colocated_storage` in persist.rs / reindex.rs) then sees the dir
    // and routes HNSW + corpus writes to the colocated path too.  Without this
    // fix the writer used the app-data path while the loader used the colocated
    // path (because `indexes.toml` recorded `colocated = true`), producing 0
    // chunks and no corpus store after the first restart.  A missing corpus
    // store also causes `write_schema_version` to return
    // "cannot write schema_version: no durable corpus" (#485).
    let init_entry = crate::service::persistence::PersistedIndex {
        id: req.id.clone(),
        root_path: req.root_path.clone(),
        colocated: true,
        ..Default::default()
    };
    let mut indexer =
        crate::service::persistence_loader::build_indexer_from_entry(&init_entry, &embedder).await;

    // Resolve repo-config filters (issue: trusty-search.yaml wiring). The
    // CLI sends `paths:` as relative strings; resolve them against `root_path`
    // here so the registry handle carries absolute subtrees ready for the
    // reindex walker. `domain_terms` is attached to the indexer so its
    // `classify_with_domain` lookup runs on every search without needing to
    // reach back into the handle.
    let include_paths: Vec<std::path::PathBuf> = req
        .include_paths
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.trim().is_empty() && p.trim() != ".")
        .map(|p| req.root_path.join(p.trim()))
        .collect();
    let exclude_globs: Vec<String> = req.exclude_globs.clone().unwrap_or_default();
    let extensions: Vec<String> = req
        .extensions
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .collect();
    let domain_terms: Vec<String> = req.domain_terms.clone().unwrap_or_default();
    let path_filter: Vec<String> = req
        .path_filter
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect();
    indexer.set_domain_terms(domain_terms.clone());

    // Persist the registration so a daemon restart can re-register
    // automatically. Best-effort: a write failure is logged but doesn't fail
    // the request — the in-memory registry still has the index.
    // Issue #118: default `include_docs` is now `true` (was `false` through
    // v0.8.2). `mode=text` searches were silently empty because docs were
    // never indexed; the per-mode `is_allowed_for_mode` filter keeps
    // `mode=code` results clean even with docs in the index.
    let include_docs: bool = req.include_docs.unwrap_or(true);
    // Issue #100: honour `.gitignore` by default. `None` on the wire ⇒ `true`
    // so existing callers (CLI, MCP, integrators) get the fix automatically
    // without having to pass a new field.
    let respect_gitignore: bool = req.respect_gitignore.unwrap_or(true);
    // Issue #109, Phase 1: staged-pipeline opt-out. `None` on the wire ⇒
    // `false` (full pipeline) so existing callers see no behaviour change.
    let lexical_only: bool = req.lexical_only.unwrap_or(false);
    // Issue #313: KG-skip flag. `None` on the wire ⇒ `false` (KG built as
    // normal). TRUSTY_NO_KG=1 provides a machine-wide default that operators
    // can set without modifying per-index config.
    let skip_kg: bool = req.skip_kg.unwrap_or_else(|| {
        let v = std::env::var("TRUSTY_NO_KG").unwrap_or_default();
        matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
    });
    // Issue #403: new indexes use colocated storage (`<root>/.trusty-search/`).
    // Register the root in `roots.toml` so the startup scanner can find it on
    // the next daemon boot, and ensure `.trusty-search/` is git-ignored.
    let colocated = true;
    if let Err(e) = crate::service::roots_registry::upsert_root(req.root_path.clone()) {
        tracing::warn!("could not register root in roots.toml for {}: {e}", req.id);
    }
    if let Err(e) = crate::service::colocated_storage::ensure_gitignored(&req.root_path) {
        tracing::warn!(
            "could not add .trusty-search/ to .gitignore for {}: {e}",
            req.id
        );
    }
    if let Err(e) = crate::service::persistence::upsert_index_registry_entry(
        crate::service::persistence::PersistedIndex {
            id: req.id.clone(),
            root_path: req.root_path.clone(),
            include_paths: req.include_paths.clone().unwrap_or_default(),
            exclude_globs: exclude_globs.clone(),
            extensions: extensions.clone(),
            domain_terms: domain_terms.clone(),
            path_filter: path_filter.clone(),
            include_docs,
            respect_gitignore,
            lexical_only,
            skip_kg,
            colocated,
        },
    ) {
        tracing::warn!("could not persist index registry for {}: {e}", req.id);
    }

    // Issue #75: capture the current git HEAD SHA at registration; the search
    // response uses it to flag stale results when the working tree advances.
    let indexed_head_sha = crate::core::git::head_sha(&req.root_path);
    // Issue #109, Phase 1: pre-mark semantic + graph as `Skipped` for
    // lexical-only indexes so the search handler never tries the HNSW lane.
    // Issue #313: pre-mark graph as `Skipped` for skip_kg indexes.
    let stages = if lexical_only {
        crate::core::registry::IndexStages {
            lexical: crate::core::registry::StageState::pending(),
            semantic: crate::core::registry::StageState::skipped(),
            graph: crate::core::registry::StageState::skipped(),
        }
    } else if skip_kg {
        crate::core::registry::IndexStages {
            lexical: crate::core::registry::StageState::pending(),
            semantic: crate::core::registry::StageState::pending(),
            graph: crate::core::registry::StageState::skipped(),
        }
    } else {
        crate::core::registry::IndexStages::default()
    };
    let handle = IndexHandle {
        id: id.clone(),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: req.root_path,
        include_paths,
        exclude_globs,
        extensions,
        domain_terms,
        include_docs,
        respect_gitignore,
        path_filter,
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(indexed_head_sha)),
        lexical_only,
        skip_kg,
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    };
    state.registry.register(handle);
    // Issue #41 Phase 1: refresh the index-count gauge so /metrics reflects
    // the registry size without a separate poll.
    crate::service::metrics::set_index_count(state.registry.list().len());
    // Push event so connected dashboards refresh their index list without a
    // page reload (mirrors the trusty-memory `palace_created` pattern).
    state.emit(DaemonEvent::IndexRegistered { id: req.id.clone() });
    Json(serde_json::json!({ "id": req.id, "created": true })).into_response()
}

/// Validate that a client-supplied `root_path` is absolute and points at an
/// existing directory, and return its canonical (symlink-resolved) form.
///
/// Why: issue #63 — `POST /indexes` and `POST /indexes/:id/reindex` used to
/// accept arbitrary `PathBuf` values, including relative paths and
/// non-existent directories. A relative path silently got resolved against
/// the daemon's startup CWD by every downstream walker / file reader, which
/// caused the wrong files to be indexed under the named id (e.g. a
/// `claude-mpm` index pointing at `/Users/masa/Projects/trusty-tools`).
/// Symlink resolution (issue: indexed-paths-mismatch) is the same problem
/// in different clothing: registering `/Users/masa/Projects/foo` when that
/// path is a symlink to `/Volumes/Kemono/...` stored the symlink path in
/// the registry, the walker emitted file paths under the symlink, and a
/// caller searching from the canonical mount point got zero hits. Resolving
/// the symlink up front makes the registry hold a single canonical identity
/// for the project, regardless of which alias the caller used.
/// What: returns `Ok(canonical_path)` when `path` is absolute, exists, and
/// is a directory — the returned path has all symlink components resolved
/// via `std::fs::canonicalize`. Otherwise returns `Err(Response)` carrying
/// `400 Bad Request` + a JSON `{ "error": "..." }` body explaining the
/// failure mode. Callers MUST use the returned canonical path for storage,
/// not the original request value.
/// Test: `create_index_rejects_relative_root_path`,
/// `create_index_rejects_nonexistent_root_path`,
/// `create_index_canonicalizes_symlinked_root_path`,
/// `reindex_rejects_relative_root_path`.
// `clippy::result_large_err` (added/tightened in clippy 1.94, newer than the
// repo's MSRV 1.88 CI baseline) flags the large axum `Response` Err variant.
// Boxing it would ripple through every `?` call site in the two handlers for
// no runtime benefit on a cold validation path; suppress locally rather than
// churn unrelated code.
#[allow(clippy::result_large_err)]
fn validate_root_path(path: &std::path::Path) -> Result<std::path::PathBuf, Response> {
    if path.as_os_str().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "root_path is required and must not be empty"
            })),
        )
            .into_response());
    }
    if !path.is_absolute() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path must be absolute (got {:?}); relative paths \
                     would be resolved against the daemon's CWD which is \
                     not the caller's CWD",
                    path.display().to_string()
                ),
            })),
        )
            .into_response());
    }
    if !path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path {:?} does not exist or is not a directory",
                    path.display().to_string()
                ),
            })),
        )
            .into_response());
    }
    // Resolve any symlink components so the registry, walker, and persistence
    // layer all agree on the project's canonical identity. `is_dir()` returned
    // true above, so `canonicalize` should succeed; on the off chance it fails
    // (e.g. a TOCTOU unlink between the `is_dir` check and this call) we surface
    // a 400 with the underlying I/O error rather than fall back to the
    // un-canonicalized path — half-resolved paths are exactly what produced the
    // mismatch in the first place.
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path {:?} could not be canonicalized: {}",
                    path.display().to_string(),
                    e
                ),
            })),
        )
            .into_response()),
    }
}

/// Determine whether a chunk's stored `file` field falls within an index's
/// registered root.
///
/// Why: issue #64 — even with `validate_root_path` (#63) preventing future
/// misregistrations, a daemon that previously indexed under the wrong root
/// can have persisted chunks whose `file` paths point at a different
/// project. The search handler post-filters with this predicate so cross-
/// index bleed cannot leak through to clients.
/// Why (issue #541 update): the warm-boot canonicalization in `restore_one_index`
/// prevents the stale-root problem going forward; this predicate adds a
/// canonicalize fallback for absolute paths so that any residual mismatch
/// (e.g. chunks indexed before the fix, volume mount alias, macOS /private/var
/// ↔ /var) also never causes a valid result to be dropped.
/// What: returns `true` when `file` is either (a) a clean relative path
/// (no leading `/`, no `..` segments) — the normal case, since the reindex
/// walker stores chunk paths relative to the index root — or (b) an
/// absolute path that starts with `root` (cheap lexical check). If (b) fails
/// and the file path exists on disk, falls back to a canonicalized comparison
/// so symlink aliases never cause a false drop (approach (b) from issue #541
/// — only results that fail the cheap check pay the `canonicalize` syscall
/// cost). Everything else (relative path with `..`, absolute path pointing
/// genuinely elsewhere) returns `false`.
/// Test: `file_is_within_root_*` unit tests below; `file_is_within_root_symlinked_root`
/// covers the symlink-alias case added for #541.
fn file_is_within_root(file: &str, root: &std::path::Path) -> bool {
    let p = std::path::Path::new(file);
    if p.is_absolute() {
        // Fast path: lexical prefix check — no syscalls.
        if p.starts_with(root) {
            return true;
        }
        // Slow-path fallback for symlink / alias mismatches (issue #541): only
        // pay the `canonicalize` cost for absolute-path results that failed the
        // cheap check (so the hot path for relative-path chunks is unaffected).
        //
        // Strategy: canonicalize the index root (resolves symlink aliases, macOS
        // /var ↔ /private/var, etc.), then check whether the stored file path
        // starts with that canonical root. We do NOT canonicalize the file path
        // itself because the file may have been deleted since indexing; we only
        // need the root to resolve correctly.
        let canonical_root = match std::fs::canonicalize(root) {
            Ok(r) => r,
            Err(_) => return false,
        };
        return p.starts_with(&canonical_root);
    }
    // Relative path: must not climb out via `..`. We accept `.` and any
    // forward-only sequence of components. Empty paths are rejected
    // defensively.
    if file.is_empty() {
        return false;
    }
    !p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Build a `503 Service Unavailable` response for handlers that require the
/// embedder before the background init task has finished.
///
/// Why: callers (CLI, MCP, integrators) need to distinguish "transient — try
/// again in a few seconds" from real failures. A standard 503 with a typed
/// JSON body lets `trusty-search index` retry, while exposing a clear
/// `embedder initializing` reason for human operators reading logs.
/// What: returns `(503, {"error": "embedder initializing, retry in a few seconds"})`.
/// Test: hit `POST /indexes` immediately after daemon boot; assert 503 and
/// JSON body shape.
fn embedder_initializing_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "embedder initializing, retry in a few seconds"
        })),
    )
        .into_response()
}

/// Build a `503 Service Unavailable` response when the embedder background
/// init task has recorded a permanent failure (issue #121).
///
/// Why: previously a hung/failed init left the daemon stuck in
/// `"initializing"` forever, so retry loops in `trusty-search index` and
/// downstream clients spun indefinitely. Returning a typed error body with
/// the recorded message lets callers fail fast and surfaces the root cause
/// (e.g. "init timed out after 60s") in logs and CLI output.
/// What: returns `(503, {"error": "embedder init failed: <message>"})`.
/// Test: `create_index_returns_503_with_error_when_embedder_failed`.
fn embedder_error_response(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": format!("embedder init failed: {message}"),
        })),
    )
        .into_response()
}

/// `DELETE /indexes/:id` — drop an index from the registry.
///
/// Why: The admin UI needs a way to evict mistakes / abandoned projects
/// without restarting the daemon. The on-disk redb store (if any) is left
/// alone — re-registering with the same id reuses it.
/// What: Calls `IndexRegistry::unregister`. Returns `{removed: bool}`.
/// Test: register → delete → list returns empty.
async fn delete_index_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let index_id = IndexId::new(id.clone());
    let removed = state.registry.unregister(&index_id);
    state.reindex_progress.remove(&index_id);
    if removed {
        // Issue #85: drop the on-disk footprint so the index doesn't come
        // back on the next daemon restart. Best-effort — log on failure.
        if let Err(e) = crate::service::persistence::remove_index_registry_entry(&id) {
            tracing::warn!("could not remove '{id}' from indexes.toml: {e}");
        }
        if let Err(e) = crate::service::persistence::remove_index_data_dir(&id) {
            tracing::warn!("could not remove on-disk data for '{id}': {e}");
        }
        // Push event so connected dashboards drop the row without refresh.
        state.emit(DaemonEvent::IndexRemoved { id: id.clone() });
        // Issue #41 Phase 1: keep the index-count gauge in sync.
        crate::service::metrics::set_index_count(state.registry.list().len());
    }
    Json(serde_json::json!({ "id": id, "removed": removed }))
}

async fn search_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(mut query): Json<SearchQuery>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    // Use the same domain-aware classifier as `CodeIndexer::search` so the
    // intent reported back to the caller matches what was used for routing.
    let intent = QueryClassifier::classify_with_domain(&query.text, &handle.domain_terms);
    // Issue #109 Phase 1: derive lane availability from the staged-pipeline
    // status surface. The search handler MUST consult `search_capabilities`
    // (NOT the legacy top-level `status` field) when deciding whether the
    // semantic / KG lanes are queryable. The indexer's `search` honours
    // `query.stage = Some(Lexical)`, so we down-shift the query to lexical
    // when either (a) the caller explicitly asked for it, or (b) the
    // semantic stage is not yet ready. Doing this here keeps the indexer
    // unaware of the index-handle-level capability surface.
    let caps = { handle.stages.read().await.search_capabilities() };
    let semantic_ready = caps.contains(&"vector");
    if query.stage.is_none() && !semantic_ready {
        // Force lexical lane until the embedder catches up. The caller's
        // request is preserved if they explicitly asked for `mode = all`
        // / similar; we only override the lane selector, not the file-type
        // filter.
        query.stage = Some(crate::core::indexer::SearchStage::Lexical);
    }
    // Issue #109 Phase 1 backpressure stub: ping the per-index pressure
    // notifier so the background Stage-2 task briefly yields. The notifier
    // is a hint — the embedder loop waits at most 100 ms.
    handle.search_pressure.notify_one();
    let started = std::time::Instant::now();
    let indexer = handle.indexer.read().await;
    let mut results = indexer
        .search(&query)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    // Issue #64: defense-in-depth post-filter. Chunks are stored with `file`
    // paths relative to the index root, so anything that escapes the root
    // (absolute path pointing elsewhere, `..` traversal, or simply a path
    // that's also absolute and outside `root_path`) is a sign of stale data
    // from a previously-misregistered index (see #63) or a bug elsewhere in
    // the pipeline. Drop those rows rather than returning cross-project
    // results to the caller. `file_is_within_root` uses a cheap lexical
    // check first; only absolute-path results that fail the fast path pay the
    // `canonicalize` syscall cost (issue #541 approach b).
    let root = handle.root_path.clone();
    let before = results.len();
    results.retain(|r| file_is_within_root(&r.file, &root));
    let filtered_out = before.saturating_sub(results.len());
    if filtered_out > 0 {
        // Issue #541: increment the process-wide Prometheus counter so operators
        // can alert on a rising drop rate without log scraping.
        metrics::counter!(
            "trusty_search_dropped_out_of_root_total",
            "index_id" => index_id.0.clone(),
        )
        .increment(filtered_out as u64);
        tracing::warn!(
            index_id = %index_id,
            root = %root.display(),
            dropped = filtered_out,
            "search_handler: dropped {} result(s) whose file path falls outside index root {} \
             — index root is stale (symlink rename or daemon restart without \
             re-canonicalization). Re-register to fix: `trusty-search index {}`",
            filtered_out,
            root.display(),
            root.display(),
        );
    }
    drop(indexer);

    let latency_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        index_id = %index_id,
        intent = %format!("{intent:?}"),
        latency_ms = latency_ms,
        results = results.len(),
        query = %&query.text[..query.text.len().min(80)],
        "search"
    );

    // Issue #75: surface index freshness in the response `meta` block so
    // callers can show staleness banners without a follow-up status call.
    //
    // `last_indexed` is the mtime of `chunks.json` (rewritten on every
    // successful commit) and matches what `GET /indexes/:id/status`
    // already returns.
    //
    // `results_may_be_stale` compares the current git HEAD SHA against the
    // SHA captured at index-registration time. False whenever either SHA
    // is unavailable (non-git directory, missing git binary) or the SHAs
    // match — i.e. defaults to "not stale" rather than scaring callers
    // about indexes whose freshness we cannot verify.
    let (_disk_bytes, last_indexed) = index_disk_and_mtime(&index_id.0);
    let indexed_sha = handle.indexed_head_sha.read().await.clone();
    let current_sha = crate::core::git::head_sha(&handle.root_path);
    let results_may_be_stale = match (indexed_sha.as_deref(), current_sha.as_deref()) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    Ok(Json(serde_json::json!({
        "results": results,
        "intent": format!("{:?}", intent),
        "latency_ms": latency_ms,
        "meta": {
            "last_indexed": last_indexed,
            "results_may_be_stale": results_may_be_stale,
            // Issue #109 Phase 1: surface which lanes contributed to this
            // result set. Lets clients display "lexical-only" badges or
            // retry once the semantic lane is ready.
            "search_capabilities": caps,
            // Issue #541: machine-readable signal that results were dropped
            // because the index root is stale. Clients (Claude Code, UI) can
            // show a remediation banner without log scraping. `false` is the
            // normal case (no drops); `true` means the operator should run
            // `trusty-search index <path>` to re-register with a fresh root.
            "stale_index_root": filtered_out > 0,
        },
    })))
}

/// Body for the global `POST /search` endpoint (issue #10 — cross-project
/// search fan-out).
///
/// Why: callers (LLM agents, the UI search bar) often don't know which
/// project an answer lives in. A single fan-out search across every
/// registered index, with results re-ranked via Reciprocal Rank Fusion, lets
/// them ask one question and get one merged answer.
#[derive(Deserialize)]
pub struct GlobalSearchRequest {
    pub query: String,
    #[serde(default = "default_global_top_k")]
    pub top_k: usize,
    /// When true, response chunks include the full `content` field. When
    /// false (default), the daemon still returns chunks with content — clients
    /// that want compact responses can read `compact_snippet`.
    #[serde(default)]
    pub full_content: bool,
    /// Optional allow-list of index ids to fan out to (issue #110). When
    /// present, only the named indexes are searched; unknown ids are
    /// silently skipped (logged at debug). When absent / empty, the daemon
    /// fans out to every registered index (legacy behaviour).
    #[serde(default)]
    pub indexes: Option<Vec<String>>,

    /// Fan-out routing strategy (issue #112). Controls how the daemon
    /// weights or filters the per-index lanes by cosine similarity between
    /// the query embedding and each index's stored `context_embedding`.
    ///
    /// - `"all"` (default): every index is searched; each index's RRF lane
    ///   is multiplied by its cosine similarity weight (indexes with no
    ///   context embedding use the neutral 1.0).
    /// - `"top_n"`: only the top-N indexes (by cosine similarity) are
    ///   searched; `routing_n` controls N (default 3).
    /// - `"threshold"`: indexes with cosine similarity below
    ///   `routing_threshold` (default 0.3) are skipped.
    #[serde(default)]
    pub routing: Option<String>,
    /// Number of indexes to keep for `routing = "top_n"`. Default 3.
    #[serde(default)]
    pub routing_n: Option<usize>,
    /// Cosine-similarity cutoff for `routing = "threshold"`. Default 0.3.
    #[serde(default)]
    pub routing_threshold: Option<f32>,
}

fn default_global_top_k() -> usize {
    10
}

/// `POST /search` — fan-out hybrid search across every registered index.
///
/// Why: see [`GlobalSearchRequest`] doc. This is distinct from
/// `POST /indexes/:id/search`, which targets a single index.
/// What: runs per-index search concurrently, tags each result with its
/// `index_id`, then re-runs RRF (k=60) over the per-index ranked lists
/// (each index treated as an equally-weighted lane) and returns the top-k
/// merged results. Indexes that error during search are skipped (logged) so
/// one bad index doesn't take down the whole fan-out.
/// Test: `test_global_search_fans_out_and_merges` registers two indexes,
/// indexes a file into each, and asserts both contribute results tagged with
/// the right `index_id`.
async fn global_search_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(req): Json<GlobalSearchRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use crate::core::search::rrf::{rrf_fuse, RRF_K};

    let all_ids = state.registry.list();
    // Issue #110: when caller supplies `indexes`, restrict fan-out to that
    // set. Unknown ids are dropped here (the per-index branch below would
    // emit a 404; we'd rather silently skip so a stale caller doesn't
    // poison an otherwise-good fan-out).
    let index_ids: Vec<IndexId> = if let Some(requested) = req.indexes.as_ref() {
        let allow: std::collections::HashSet<&str> = requested.iter().map(|s| s.as_str()).collect();
        all_ids
            .into_iter()
            .filter(|id| allow.contains(id.0.as_str()))
            .collect()
    } else {
        all_ids
    };
    let total_indexes = index_ids.len();
    if index_ids.is_empty() {
        return Ok(Json(serde_json::json!({
            "results": Vec::<crate::core::indexer::CodeChunk>::new(),
            "indexes_searched": Vec::<String>::new(),
            "total_indexes": 0_usize,
            "latency_ms": 0_u64,
            "intent": format!("{:?}", QueryClassifier::classify(&req.query)),
        })));
    }

    let started = std::time::Instant::now();
    let intent = QueryClassifier::classify(&req.query);

    // Issue #112: compute per-index context weights, then apply the routing
    // strategy to decide which indexes participate in the fan-out.
    let routing_mode = RoutingMode::from_request(&req);
    let weights = compute_context_weights(&state.registry, &index_ids, &req.query).await;
    let (mut active_ids, mut weight_map) = routing_mode.apply(&index_ids, &weights);

    // Issue #404 — nested-index fan-out (MVP):
    // 1. Derive the index hierarchy from root_path prefix containment.
    // 2. For `threshold` routing: include any sub-index whose parent is active,
    //    even if the sub-index's own cosine similarity falls below the threshold.
    //    This prevents small-subtree indexes from being silently excluded when
    //    the parent is clearly relevant.
    //
    // Note: when the caller supplies an explicit `indexes: [...]` restriction,
    // the set is treated as flat peers (no hierarchy applied) to preserve the
    // existing precision-override semantics.
    let hierarchy = if req.indexes.is_none() {
        let h = crate::core::search::hierarchy::IndexHierarchy::from_registry(
            &state.registry,
            &index_ids,
        );
        if matches!(routing_mode, RoutingMode::Threshold(_)) && !h.parent_of.is_empty() {
            let inactive_ids: Vec<IndexId> = index_ids
                .iter()
                .filter(|id| !weight_map.contains_key(id))
                .cloned()
                .collect();
            crate::core::search::hierarchy::apply_threshold_child_inclusion(
                &inactive_ids,
                &mut active_ids,
                &mut weight_map,
                &h,
            );
        }
        h
    } else {
        crate::core::search::hierarchy::IndexHierarchy::default()
    };

    let routing_label = routing_mode.label().to_string();
    let routing_decisions: Vec<serde_json::Value> = index_ids
        .iter()
        .map(|id| {
            let w = weights.get(id).copied().unwrap_or(1.0);
            let included = weight_map.contains_key(id);
            serde_json::json!({
                "index_id": id.0,
                "cosine_similarity": w,
                "included": included,
            })
        })
        .collect();

    // Build the same SearchQuery shape every per-index search uses. We
    // oversample per-index by passing the user's top_k unchanged: each lane
    // contributes up to top_k candidates, then RRF picks the best top_k
    // overall.
    let per_index_query = SearchQuery {
        text: req.query.clone(),
        top_k: req.top_k,
        expand_graph: true,
        compact: !req.full_content,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        // Cross-project fan-out is code-shaped by convention; per-tool
        // search_text / search_data callers use the per-index endpoint
        // directly and carry their own `mode` in the request body.
        mode: crate::core::indexer::SearchMode::default(),
        // Cross-project fan-out keeps the downrank default (issue #74); a
        // caller that wants archived chunks gone uses the per-index endpoint
        // with `exclude_archived: true`.
        exclude_archived: false,
        // Cross-project fan-out leaves stage selection up to each index's
        // own capability surface — the per-index loop below downshifts to
        // lexical when the semantic lane isn't ready (issue #109 Phase 1).
        stage: None,
        refine_query: None,
    };

    // Run all per-index searches concurrently. Any index that errors is
    // skipped with a log line so a single broken index doesn't 500 the
    // whole fan-out.
    let registry = state.registry.clone();
    let futures = active_ids.into_iter().map(|id| {
        let registry = registry.clone();
        let query = per_index_query.clone();
        async move {
            let handle = registry.get(&id)?;
            let indexer = handle.indexer.read().await;
            match indexer.search(&query).await {
                Ok(results) => Some((id, results)),
                Err(e) => {
                    tracing::warn!("global search: index {} errored: {e}", id);
                    None
                }
            }
        }
    });
    let per_index_results: Vec<(IndexId, Vec<crate::core::indexer::CodeChunk>)> =
        futures::future::join_all(futures)
            .await
            .into_iter()
            .flatten()
            .collect();

    // Build a flat lookup table from "namespaced" chunk_id
    // ({index_id}::{chunk.id}) back to the tagged CodeChunk, plus per-index
    // ranked id lists for RRF. Namespacing is required because different
    // indexes can produce colliding chunk_ids (same relative file path in
    // two projects).
    let mut chunk_lookup: std::collections::HashMap<String, crate::core::indexer::CodeChunk> =
        std::collections::HashMap::new();
    let mut lanes: Vec<Vec<(String, f32)>> = Vec::with_capacity(per_index_results.len());
    let mut indexes_searched: Vec<String> = Vec::with_capacity(per_index_results.len());
    for (id, results) in per_index_results {
        indexes_searched.push(id.0.clone());
        // Issue #112: in `"all"` mode, multiply each lane's scores by the
        // index's cosine-similarity weight; in `"top_n"` / `"threshold"`
        // modes the weight is always 1.0 (selection has already happened).
        // Issue #404: also apply the sub-index priority boost so sub-index
        // hits rank above the parent's duplicate coverage after RRF fusion.
        let cosine_weight = weight_map.get(&id).copied().unwrap_or(1.0);
        let weight = crate::core::search::hierarchy::effective_weight_for_index(
            &id,
            cosine_weight,
            &hierarchy,
        );
        let mut lane: Vec<(String, f32)> = Vec::with_capacity(results.len());
        for mut chunk in results {
            let namespaced = format!("{}::{}", id.0, chunk.id);
            // Tag the chunk with its origin index before storing it so the
            // returned CodeChunks know where they came from.
            chunk.index_id = Some(id.0.clone());
            let weighted_score = chunk.score * weight;
            lane.push((namespaced.clone(), weighted_score));
            chunk_lookup.insert(namespaced, chunk);
        }
        lanes.push(lane);
    }

    // RRF fuse across lanes. `rrf_fuse` takes exactly two lanes, so we fold
    // pairwise: start with empty + lane0, then merge each subsequent lane.
    // Each fold step uses alpha=1, beta=1 — every index lane contributes
    // equally. The output is sorted by fused score desc.
    let mut fused: Vec<(String, f32)> = Vec::new();
    let oversample = req.top_k.saturating_mul(4).max(req.top_k).max(10);
    for lane in lanes {
        fused = rrf_fuse(&fused, &lane, 1.0, 1.0, RRF_K, oversample);
    }

    // Issue #404: post-RRF dedup for nested indexes.
    // When a parent index and one of its sub-indexes both contain a chunk for
    // the same `(canonical_absolute_path, start_line, end_line)`, drop the
    // parent's copy (lower-scored after boost) and keep the sub-index's copy.
    // Flat peers that merely share files are NOT deduped.
    let (fused, hierarchy_dedup_count) = crate::core::search::hierarchy::dedup_nested_results(
        fused,
        &chunk_lookup,
        &state.registry,
        &hierarchy,
    );

    let mut fused = fused;
    fused.truncate(req.top_k);

    let results: Vec<crate::core::indexer::CodeChunk> = fused
        .into_iter()
        .filter_map(|(id, fused_score)| {
            let mut chunk = chunk_lookup.remove(&id)?;
            chunk.score = fused_score;
            Some(chunk)
        })
        .collect();

    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(serde_json::json!({
        "results": results,
        "indexes_searched": indexes_searched,
        "total_indexes": total_indexes,
        "latency_ms": latency_ms,
        "intent": format!("{:?}", intent),
        "routing": routing_label,
        "routing_decisions": routing_decisions,
        "hierarchy_dedup_count": hierarchy_dedup_count,
    })))
}

/// Fan-out routing strategy resolved from a `GlobalSearchRequest` (issue #112).
///
/// Why: keeps the per-strategy logic (weight every lane, take top-N, filter
/// below threshold) in one place so the global-search handler stays small.
#[derive(Debug, Clone, Copy)]
enum RoutingMode {
    /// Search every index; multiply each lane's RRF scores by the index's
    /// context cosine similarity (indexes with no context use 1.0).
    All,
    /// Search only the top-N indexes by cosine similarity. Weights are not
    /// applied to lane scores (selection already encodes relevance).
    TopN(usize),
    /// Search only indexes whose cosine similarity ≥ threshold. Weights are
    /// not applied to lane scores.
    Threshold(f32),
}

impl RoutingMode {
    const DEFAULT_TOP_N: usize = 3;
    const DEFAULT_THRESHOLD: f32 = 0.3;

    fn from_request(req: &GlobalSearchRequest) -> Self {
        match req.routing.as_deref() {
            Some("top_n") => Self::TopN(req.routing_n.unwrap_or(Self::DEFAULT_TOP_N).max(1)),
            Some("threshold") => {
                Self::Threshold(req.routing_threshold.unwrap_or(Self::DEFAULT_THRESHOLD))
            }
            // "all" or anything else (or absent) defaults to All.
            _ => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::TopN(_) => "top_n",
            Self::Threshold(_) => "threshold",
        }
    }

    /// Filter `index_ids` according to the strategy and return the active
    /// id list plus the per-id weight map the lane builder will consult.
    ///
    /// - `All`: every id is active; weight = its cosine similarity.
    /// - `TopN`: top N by cosine similarity; weight = 1.0 for selected ids.
    /// - `Threshold`: cosine ≥ threshold; weight = 1.0 for selected ids.
    fn apply(
        self,
        index_ids: &[IndexId],
        weights: &std::collections::HashMap<IndexId, f32>,
    ) -> (Vec<IndexId>, std::collections::HashMap<IndexId, f32>) {
        match self {
            Self::All => {
                let active: Vec<IndexId> = index_ids.to_vec();
                let map: std::collections::HashMap<IndexId, f32> = index_ids
                    .iter()
                    .map(|id| (id.clone(), weights.get(id).copied().unwrap_or(1.0)))
                    .collect();
                (active, map)
            }
            Self::TopN(n) => {
                let mut ranked: Vec<(&IndexId, f32)> = index_ids
                    .iter()
                    .map(|id| (id, weights.get(id).copied().unwrap_or(1.0)))
                    .collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let active: Vec<IndexId> =
                    ranked.iter().take(n).map(|(id, _)| (*id).clone()).collect();
                let map: std::collections::HashMap<IndexId, f32> =
                    active.iter().map(|id| (id.clone(), 1.0)).collect();
                (active, map)
            }
            Self::Threshold(t) => {
                let active: Vec<IndexId> = index_ids
                    .iter()
                    .filter(|id| weights.get(id).copied().unwrap_or(1.0) >= t)
                    .cloned()
                    .collect();
                let map: std::collections::HashMap<IndexId, f32> =
                    active.iter().map(|id| (id.clone(), 1.0)).collect();
                (active, map)
            }
        }
    }
}

/// Embed the query once and compute cosine similarity against every index's
/// stored `context_embedding` (issue #112).
///
/// Why: the fan-out router needs a single relevance score per index. Indexes
/// without a context embedding (no recognised metadata, embedder unavailable
/// during last reindex) default to a neutral 1.0 so they participate
/// normally — the absence of a fingerprint is not a relevance signal.
/// What: returns a `HashMap<IndexId, f32>` where every id in `index_ids` has
/// an entry; the value is either `cosine_similarity(query, context)` or
/// `1.0` for indexes with no context. Failures embedding the query (e.g.
/// embedder not wired) also fall back to 1.0 across the board so the global
/// search keeps working as a plain fan-out.
async fn compute_context_weights(
    registry: &crate::core::registry::IndexRegistry,
    index_ids: &[IndexId],
    query: &str,
) -> std::collections::HashMap<IndexId, f32> {
    use crate::core::mmr::cosine_similarity;

    // Try to obtain a query embedding from any index that has an embedder
    // wired. Every index in the registry shares the same machine-wide
    // FastEmbedder, so the first successful embed is reused for all.
    let mut query_embedding: Option<Vec<f32>> = None;
    for id in index_ids {
        let Some(handle) = registry.get(id) else {
            continue;
        };
        let indexer = handle.indexer.read().await;
        match indexer.embed_text(query).await {
            Ok(Some(vec)) => {
                query_embedding = Some(vec);
                break;
            }
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!("context_routing: embed_text failed on {}: {e}", id.0);
                continue;
            }
        }
    }

    let mut out = std::collections::HashMap::with_capacity(index_ids.len());
    let Some(q) = query_embedding else {
        // Couldn't embed at all — fall back to neutral weights everywhere.
        for id in index_ids {
            out.insert(id.clone(), 1.0);
        }
        return out;
    };

    for id in index_ids {
        let Some(handle) = registry.get(id) else {
            out.insert(id.clone(), 1.0);
            continue;
        };
        let ctx_guard = handle.context_embedding.read().await;
        let weight = match ctx_guard.as_ref() {
            Some(ctx) if ctx.len() == q.len() => cosine_similarity(&q, ctx).max(0.0),
            _ => 1.0,
        };
        out.insert(id.clone(), weight);
    }
    out
}

/// Body for `POST /indexes/:id/search_similar`.
///
/// Why: code-to-code similarity (issue #31). The caller knows the *file +
/// optional function name* of the chunk they want to find neighbours of, not
/// its synthetic chunk id.
#[derive(Deserialize)]
pub struct SearchSimilarRequest {
    pub file: String,
    #[serde(default)]
    pub function: Option<String>,
    #[serde(default = "default_similar_top_k")]
    pub top_k: usize,
}

fn default_similar_top_k() -> usize {
    10
}

/// Handle `POST /indexes/:id/search_similar`.
///
/// Why (issue #484): the original implementation returned 404 whenever the
/// embedding-LRU cache missed, which always happens for `skip_kg=true` indexes
/// (the cache is populated only at commit time; entries age out and are never
/// restored). Re-embedding the seed chunk's text via `embed_text` when the
/// cache misses lets `search_similar` work on any index regardless of KG mode.
/// What: looks up the seed chunk's embedding from the LRU cache; on miss,
/// fetches the chunk's raw content and re-embeds it; falls through to 404 only
/// when neither path can produce an embedding (BM25-only index or unknown
/// chunk).
/// Test: `search_similar_fallback_reembeds_when_cache_misses` in the server
/// integration tests.
async fn search_similar_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<SearchSimilarRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let started = std::time::Instant::now();
    let indexer = handle.indexer.read().await;
    let chunk_id = indexer
        .find_chunk_id(&req.file, req.function.as_deref())
        .await
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    // Issue #484: the LRU embedding cache misses for skip_kg=true indexes
    // (entries are only written at reindex time and are evicted under memory
    // pressure).  When the cache misses, fetch the chunk's text and re-embed
    // it so search_similar works on any index regardless of KG mode.
    let embedding = if let Some(cached) = indexer.get_embedding(&chunk_id) {
        cached
    } else {
        let content = indexer
            .chunk_content_by_id(&chunk_id)
            .await
            .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
        indexer
            .embed_text(&content)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
            .ok_or_else(|| StatusCode::NOT_FOUND.into_response())? // BM25-only: no embedder wired
    };
    let results = indexer
        .similar_by_embedding(&embedding, req.top_k, Some(&chunk_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(serde_json::json!({
        "results": results,
        "seed_chunk_id": chunk_id,
        "latency_ms": latency_ms,
    })))
}

/// Resolve a single index's on-disk footprint and last-indexed timestamp.
///
/// Why (issue #38): the redesigned Indexes view shows a per-index disk-usage
/// column and a last-indexed column. Both are cheap to derive from the
/// per-index data directory (`<data_dir>/indexes/<id>/`) without persisting
/// extra metadata: size is a directory walk; the last-indexed time is the
/// modification time of `chunks.json` (rewritten on every successful commit).
/// What: returns `(disk_bytes, last_indexed_rfc3339)`. Either field is `None`
/// when the directory or file cannot be resolved / read — the handler maps
/// `None` to JSON `null`, so a never-reindexed index still responds 200.
/// Test: `index_disk_and_mtime_handles_missing_dir` covers the absent-dir
/// degrade-to-`None` path.
fn index_disk_and_mtime(index_id: &str) -> (Option<u64>, Option<String>) {
    // Why: `persistence::index_data_dir` creates the directory as a side effect,
    // which would defeat the "missing dir → None" contract this helper relies
    // on. Compute the path manually (mirroring the persistence layer's logic)
    // and only touch the filesystem to *read* metadata.
    let Ok(data_dir) = crate::service::persistence::data_dir() else {
        return (None, None);
    };
    let dir = data_dir
        .join("indexes")
        .join(crate::service::persistence::sanitize_id_for_path(index_id));
    if !dir.exists() {
        return (None, None);
    }
    let disk_bytes = Some(trusty_common::sys_metrics::dir_size_bytes(&dir));
    // Issue #80: after the redb cutover (issue #28), `chunks.json` is no
    // longer rewritten on every commit — the durable corpus lives in
    // `index.redb`. The previous implementation read `chunks.json` mtime
    // unconditionally and returned `null` for every post-cutover index,
    // making `last_indexed` permanently stale.
    //
    // Why: callers (admin UI, MCP `index_status`) rely on this field to
    // show "indexed N seconds ago"; a permanent null hides actual freshness.
    // What: probe `index.redb` first (current authoritative file rewritten
    // by every redb commit / atomic swap), then fall back to `chunks.json`
    // for un-migrated indexes (the legacy JSON snapshot still rewritten by
    // the migration shim). The first existing file wins.
    // Test: `index_disk_and_mtime_handles_missing_dir` (this fn) +
    // `last_indexed_prefers_redb_then_chunks_json` (the pure selector below).
    let last_indexed = first_existing_mtime_rfc3339(&dir, &["index.redb", "chunks.json"]);
    (disk_bytes, last_indexed)
}

/// Return the modification time (as an RFC3339 string) of the first file in
/// `candidates` that exists under `dir`.
///
/// Why (issue #80): after the redb cutover (issue #28) `chunks.json` is no
/// longer rewritten on every commit, so reading its mtime returned `null`
/// for every migrated index. The freshness signal must prefer the current
/// authoritative file (`index.redb`, rewritten by every redb commit / atomic
/// swap) and only fall back to the legacy JSON snapshot for un-migrated
/// indexes. Extracting the selection into a pure function (path in, optional
/// string out) makes the precedence rule unit-testable without mutating the
/// process-wide data-dir env vars that `index_disk_and_mtime` depends on.
/// What: probes each candidate filename in order, returns the RFC3339-encoded
/// mtime of the first one that exists and whose metadata is readable, or
/// `None` when none exist.
/// Test: `last_indexed_prefers_redb_then_chunks_json` and
/// `last_indexed_none_when_no_candidates_exist`.
fn first_existing_mtime_rfc3339(dir: &std::path::Path, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find_map(|name| std::fs::metadata(dir.join(name)).ok())
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339()
        })
}

async fn index_status_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let indexer = handle.indexer.read().await;
    // Issue #111: surface `path_filter` so callers can see which glob filter
    // (if any) is active for the index. Returns `null` when no filter is set.
    let path_filter = if handle.path_filter.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Array(
            handle
                .path_filter
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        )
    };
    // Issue #112: surface whether a context embedding has been computed
    // for this index, plus the truncated human-readable summary that
    // produced it. Helps operators verify metadata scraping found a
    // recognised file.
    let has_context_embedding = handle.context_embedding.read().await.is_some();
    let context_summary = handle
        .context_summary
        .read()
        .await
        .clone()
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    // Issue #38: surface per-index on-disk footprint + last-indexed time for
    // the admin UI's enhanced Indexes table. Both are derived from the
    // per-index data directory; absent / unreadable values degrade to null
    // so a fresh (never-reindexed) index still returns a 200.
    let (disk_bytes, last_indexed) = index_disk_and_mtime(&index_id.0);
    // Issue #80: surface a coarse lifecycle status. The legacy top-level
    // `status` field stays for back-compat — it collapses to `indexing` while
    // any reindex task is running and `ready` otherwise (mirrors the v0.8.x
    // contract). Callers wanting per-stage granularity should consult the
    // `stages` block introduced in v0.9.0 (issue #109, Phase 1) — that field
    // tracks lexical → semantic → graph progress and grows
    // `search_capabilities` as each lane comes online.
    let legacy_status = match state
        .reindex_progress
        .get(&index_id)
        .map(|p| p.status.load())
    {
        Some(ReindexStatus::Running) => "indexing",
        _ => "ready",
    };
    // Issue #109 Phase 1: snapshot the staged-pipeline state so the response
    // can surface per-stage status and derive the public `search_capabilities`
    // array. The legacy `status` field stays at the top level, but
    // integrators wanting "is the vector lane ready" should consult
    // `search_capabilities`.
    let stages_snapshot = handle.stages.read().await.clone();
    let search_capabilities = stages_snapshot.search_capabilities();
    // Issue #100: surface budget-truncation so callers can flag indexes that
    // hit the `TRUSTY_MAX_CHUNKS` cap during the last reindex. Defaults to
    // `false` / `0` when no `ReindexProgress` entry exists (i.e. the index
    // was warm-booted from disk and hasn't been reindexed in this daemon
    // session — exactly the back-compat case the task spec calls out).
    let (walk_truncated_by_budget, chunks_dropped_by_cap) = state
        .reindex_progress
        .get(&index_id)
        .map_or((false, 0), |p| {
            let n = p
                .chunks_dropped_by_cap
                .load(std::sync::atomic::Ordering::Acquire);
            (n > 0, n)
        });
    // Issue #280: snapshot the walk diagnostics so operators can diagnose
    // zero-chunk indexes without reading daemon logs.  Use `clone()` so the
    // read lock is released before we build the JSON response.
    let walk_diag = handle.walk_diagnostics.read().await.clone();
    // Issue #681: prefer durable corpus count; in-memory map returns 0 after
    // idle eviction (TRUSTY_CHUNKS_IDLE_EVICT_SECS default 300s). Falls back to
    // in-memory for BM25-only / test indexers that have no corpus wired.
    let chunk_count = indexer
        .corpus_arc()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or_else(|| indexer.chunk_count());
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "root_path": handle.root_path,
        "chunk_count": chunk_count,
        "status": legacy_status,
        "stages": stages_snapshot,
        "search_capabilities": search_capabilities,
        "lexical_only": handle.lexical_only,
        "skip_kg": handle.skip_kg,
        "path_filter": path_filter,
        "has_context_embedding": has_context_embedding,
        "context_summary": context_summary,
        "disk_bytes": disk_bytes,
        "last_indexed": last_indexed,
        "respect_gitignore": handle.respect_gitignore,
        "walk_truncated_by_budget": walk_truncated_by_budget,
        "chunks_dropped_by_cap": chunks_dropped_by_cap,
        // Issue #280: walk diagnostic fields.
        "last_walk_started_at": walk_diag.last_walk_started_at,
        "last_walk_files_seen": walk_diag.last_walk_files_seen,
        "last_walk_files_skipped": walk_diag.last_walk_files_skipped,
        "last_walk_error": walk_diag.last_walk_error,
    })))
}

/// Optional query parameters for `GET /indexes/{id}/graph` (issue #128).
///
/// Why: a full KG export on a large repo can be tens of thousands of nodes;
/// D3/Cytoscape clients usually want a filtered subgraph. These let the caller
/// narrow the export server-side instead of shipping the whole graph.
/// What: all fields optional; absent params apply no filter.
/// Test: covered by `test_graph_handler_filters` in `tests/integration_tests.rs`.
#[derive(Debug, Default, serde::Deserialize)]
struct GraphQueryParams {
    /// Comma-separated node `type` values to keep (e.g. `Symbol,File`).
    types: Option<String>,
    /// Comma-separated `EdgeKind` display names to keep (e.g.
    /// `CallsFunction,Implements`).
    edge_types: Option<String>,
    /// Minimum edge weight; edges below this are dropped.
    min_weight: Option<f32>,
}

/// Parse a comma-separated filter param into a trimmed, lower-cased set.
///
/// Why: both the node-type and edge-type filters accept comma lists and are
/// matched case-insensitively; this keeps the parsing in one place.
/// What: returns `None` when the param is absent or empty (meaning "no
/// filter"), otherwise the set of non-empty lower-cased tokens.
/// Test: exercised via `graph_handler` integration tests.
fn parse_filter_set(raw: Option<&str>) -> Option<std::collections::HashSet<String>> {
    let raw = raw?;
    let set: std::collections::HashSet<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// Derive the D3/Cytoscape node `type` from a symbol name.
///
/// Why: `SymbolNode` carries no richer type metadata yet (issue #128 note), so
/// the endpoint infers a coarse type from the name shape.
/// What: returns `"File"` when the symbol looks like a file path (contains a
/// `/` and has a file extension), otherwise `"Symbol"`.
/// Test: covered indirectly by `graph_handler` integration tests.
fn node_type_for_symbol(symbol: &str) -> &'static str {
    let looks_like_path = symbol.contains('/')
        && std::path::Path::new(symbol)
            .extension()
            .is_some_and(|e| !e.is_empty());
    if looks_like_path {
        "File"
    } else {
        "Symbol"
    }
}

/// `GET /indexes/{id}/graph` — export the full SymbolGraph as D3/Cytoscape JSON.
///
/// Why: issue #128 — external visualisers (and the admin UI) need the whole
/// knowledge graph, not just the BFS-scoped neighbours the search pipeline
/// uses. This endpoint snapshots the graph and serialises every node and edge.
/// What: snapshots the symbol graph (lock-free after the `Arc` clone), applies
/// the optional `types` / `edge_types` / `min_weight` filters, and returns
/// `{ nodes, edges, stats, generated_at }`. A 1-hour `Cache-Control` header is
/// attached since the graph only changes on reindex.
/// Test: covered by `test_graph_handler_*` in `tests/integration_tests.rs`.
async fn graph_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphQueryParams>,
) -> Result<Response, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.snapshot_symbol_graph().await
    };

    let type_filter = parse_filter_set(params.types.as_deref());
    let edge_filter = parse_filter_set(params.edge_types.as_deref());
    let min_weight = params.min_weight.unwrap_or(f32::MIN);

    // Build node list, tracking which symbols survive the type filter so we
    // can drop edges that reference filtered-out endpoints.
    let mut kept_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut nodes: Vec<serde_json::Value> = Vec::new();
    for (symbol, chunk_id, file) in graph.all_nodes() {
        let node_type = node_type_for_symbol(&symbol);
        if let Some(ref filter) = type_filter {
            if !filter.contains(&node_type.to_ascii_lowercase()) {
                continue;
            }
        }
        kept_symbols.insert(symbol.clone());
        nodes.push(serde_json::json!({
            "id": chunk_id,
            "type": node_type,
            "label": symbol,
            "metadata": { "file": file, "symbol": symbol },
        }));
    }

    let mut edges: Vec<serde_json::Value> = Vec::new();
    for (source, target, kind) in graph.all_edges() {
        // Drop edges whose endpoints were filtered out by the type filter.
        if type_filter.is_some()
            && (!kept_symbols.contains(&source) || !kept_symbols.contains(&target))
        {
            continue;
        }
        let kind_name = format!("{kind:?}");
        if let Some(ref filter) = edge_filter {
            if !filter.contains(&kind_name.to_ascii_lowercase()) {
                continue;
            }
        }
        let weight = kind.score_multiplier();
        if weight < min_weight {
            continue;
        }
        edges.push(serde_json::json!({
            "source": source,
            "target": target,
            "type": kind_name,
            "weight": weight,
        }));
    }

    let body = serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "stats": {
            "node_count": graph.node_count(),
            "edge_count": graph.edge_count(),
        },
        "generated_at": chrono::Utc::now().to_rfc3339(),
    });

    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("max-age=3600"),
    );
    Ok(response)
}

/// `GET /indexes/{id}/graph/stats` — symbol-graph summary statistics
/// (issue #41 phase 2).
///
/// `GET /indexes/{id}/graph/stats` — symbol-graph summary statistics
/// (issue #41 phase 2).
///
/// Why: lets agents and dashboards verify KG health (total nodes/edges plus a
/// per-`EdgeKind` breakdown) without parsing the much larger `/graph` export
/// or scraping Prometheus. The Phase B/C edge counts here are the
/// load-bearing signal that the entity-derived edges are actually wired.
/// What: snapshots the symbol graph (lock-free after the `Arc` clone) and
/// returns `{ node_count, edge_count, edge_kinds: { CallsFunction: …, … } }`.
/// Returns 404 when the index id is unknown.
/// Test: covered by `graph_stats_handler_returns_breakdown` in this module.
async fn graph_stats_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.snapshot_symbol_graph().await
    };
    let breakdown = graph.edge_kind_breakdown();
    let mut edge_kinds = serde_json::Map::with_capacity(breakdown.len());
    for (tag, count) in breakdown {
        edge_kinds.insert(tag, serde_json::Value::from(count));
    }

    Ok(Json(serde_json::json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "edge_kinds": serde_json::Value::Object(edge_kinds),
    })))
}

async fn index_file_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<IndexFileRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let indexer = handle.indexer.read().await;
    indexer
        .index_file(&req.path, &req.content)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "path": req.path,
        "indexed": true,
    })))
}

async fn remove_file_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<RemoveFileRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let indexer = handle.indexer.read().await;
    let removed = indexer
        .remove_file(&req.path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "path": req.path,
        "removed_chunks": removed,
    })))
}

/// Query params for `GET /indexes/:id/chunks` (issue #54).
#[derive(Deserialize)]
pub struct ChunksParams {
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_chunks_limit")]
    pub limit: usize,
}

fn default_chunks_limit() -> usize {
    100
}

/// Hard ceiling on a single `chunks` page so a misconfigured client can't pull
/// the entire corpus into one response. Mirrored in the `list_chunks` MCP tool.
const MAX_CHUNKS_LIMIT: usize = 1_000;

/// `GET /indexes/:id/chunks?offset=&limit=` — paginated enumeration of an index.
///
/// Why: trusty-analyzer (sidecar daemon) and external tooling need to page
/// through every chunk in batches without loading the whole corpus at once.
/// Issue #54 introduces stable-order pagination on top of the existing bulk
/// export.
/// What: Returns
/// `{ index_id, total, offset, limit, chunks: [...] }`. `chunks` is the slice
/// `[offset .. offset+limit]` of the corpus sorted by `(file, start_line)`.
/// `limit` is clamped to `MAX_CHUNKS_LIMIT` (1000); the value echoed back in
/// the response is the post-clamp value so clients can detect the clamp.
/// Test: `test_get_index_chunks_paginates` registers an index, indexes a few
/// files, asserts page1 + page2 cover all chunks without overlap.
async fn get_index_chunks_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<ChunksParams>,
) -> Result<Json<serde_json::Value>, Response> {
    let index_id = IndexId::new(id.clone());
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index_response(&id))?;
    let limit = params.limit.min(MAX_CHUNKS_LIMIT);
    let indexer = handle.indexer.read().await;
    let (total, chunks) = indexer.enumerate_chunks(params.offset, limit).await;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "total": total,
        "offset": params.offset,
        "limit": limit,
        "chunks": chunks,
    })))
}

/// Grep a single index's files and append hits into `out`, honouring the
/// remaining `max_results` budget.
///
/// Why: both the per-index (`POST /indexes/:id/grep`) and the global
/// (`POST /grep`) handlers need the identical "for every file the index knows
/// about, read it from disk and run the matcher" loop. Factoring it out keeps
/// the two handlers thin and guarantees they behave identically.
/// What: snapshots the index's `RawChunk` corpus to discover the distinct set
/// of files (deduped, since one file produces many chunks), then for each file
/// that passes the glob filter and lives within the index root, reads the file
/// fresh from disk under `root_path` and runs [`grep::grep_file_content`]. Files
/// that fail the glob, escape the root, or can't be read are skipped silently
/// (a read failure is logged at debug — the file may have been deleted since it
/// was indexed). Greps the real on-disk bytes, so no embedding is required and
/// line numbers are exact. Stops once `out.len()` reaches `max_results`.
/// Test: `grep::tests` covers the matcher; `grep_endpoint_*` server integration
/// tests cover the file-walking + glob + root-confinement behaviour.
async fn grep_one_index(
    handle: &IndexHandle,
    compiled: &crate::service::grep::CompiledGrep,
    out: &mut Vec<crate::service::grep::GrepMatch>,
    max_results: usize,
) {
    if out.len() >= max_results {
        return;
    }
    let chunks = {
        let indexer = handle.indexer.read().await;
        indexer.raw_chunks_snapshot().await
    };
    // One file produces many chunks; dedupe to a sorted, distinct file set so
    // each file is read and scanned exactly once in a deterministic order.
    let mut files: Vec<String> = chunks.into_iter().map(|c| c.file).collect();
    files.sort();
    files.dedup();

    for rel in files {
        if out.len() >= max_results {
            return;
        }
        // Glob filter (cheap) before defense-in-depth root confinement.
        if !compiled.path_matches(&rel) {
            continue;
        }
        if !file_is_within_root(&rel, &handle.root_path) {
            continue;
        }
        let abs = if std::path::Path::new(&rel).is_absolute() {
            std::path::PathBuf::from(&rel)
        } else {
            handle.root_path.join(&rel)
        };
        match tokio::fs::read_to_string(&abs).await {
            Ok(content) => {
                crate::service::grep::grep_file_content(&rel, &content, compiled, out, max_results);
            }
            Err(e) => {
                tracing::debug!(
                    file = %rel,
                    error = %e,
                    "grep: skipping unreadable file (deleted or non-UTF-8 since index time)"
                );
            }
        }
    }
}

/// `POST /indexes/:id/grep` — grep-parity regex search over one index's files.
///
/// Why: complements `POST /indexes/:id/search` (fuzzy hybrid recall) with exact,
/// deterministic, line-accurate matching for callers who need `grep`/`ripgrep`
/// semantics (regex, `-i`, `-A`/`-B`/`-C`, `--include` glob, multiline) against
/// a known project — without re-embedding.
/// What: compiles the [`grep::GrepRequest`] (400 on bad regex/glob), resolves
/// the index (404 if unknown), runs [`grep_one_index`], and returns a
/// [`grep::GrepResponse`]. `truncated` is set when the `max_results` cap is hit.
/// Test: `grep_endpoint_returns_matches`, `grep_endpoint_bad_regex_is_400`,
/// `grep_endpoint_unknown_index_is_404`.
async fn grep_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<crate::service::grep::GrepRequest>,
) -> Result<Json<crate::service::grep::GrepResponse>, (StatusCode, Json<serde_json::Value>)> {
    let compiled = crate::service::grep::CompiledGrep::compile(&req).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
    })?;
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": format!("unknown index: {}", index_id.0) })),
    ))?;

    let started = std::time::Instant::now();
    let mut matches = Vec::new();
    grep_one_index(&handle, &compiled, &mut matches, req.max_results).await;
    let truncated = matches.len() >= req.max_results;
    tracing::info!(
        index_id = %index_id,
        matches = matches.len(),
        truncated = truncated,
        latency_ms = started.elapsed().as_millis() as u64,
        "grep"
    );
    let total = matches.len();
    Ok(Json(crate::service::grep::GrepResponse {
        matches,
        total,
        truncated,
    }))
}

/// `POST /grep` — grep-parity regex search fanned out across indexes.
///
/// Why: callers that don't know which project a literal lives in want one grep
/// over every (or a chosen) index, mirroring the global `POST /search` fan-out.
/// What: compiles the request (400 on bad regex/glob), then iterates the
/// registered indexes (restricted to `index_id` when supplied — unknown id ⇒
/// empty result set, not 404, matching the global search's tolerant behaviour),
/// running [`grep_one_index`] against each until the shared `max_results` budget
/// is exhausted. Returns a [`grep::GrepResponse`].
/// Test: `grep_global_fans_out`, `grep_global_respects_index_filter`.
async fn global_grep_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(req): Json<crate::service::grep::GrepRequest>,
) -> Result<Json<crate::service::grep::GrepResponse>, (StatusCode, Json<serde_json::Value>)> {
    let compiled = crate::service::grep::CompiledGrep::compile(&req).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
    })?;

    let ids: Vec<IndexId> = match req.index_id.as_deref() {
        Some(only) => state
            .registry
            .list()
            .into_iter()
            .filter(|id| id.0 == only)
            .collect(),
        None => state.registry.list(),
    };

    let started = std::time::Instant::now();
    let mut matches = Vec::new();
    for id in ids {
        if matches.len() >= req.max_results {
            break;
        }
        if let Some(handle) = state.registry.get(&id) {
            grep_one_index(&handle, &compiled, &mut matches, req.max_results).await;
        }
    }
    let truncated = matches.len() >= req.max_results;
    tracing::info!(
        matches = matches.len(),
        truncated = truncated,
        latency_ms = started.elapsed().as_millis() as u64,
        "grep_global"
    );
    let total = matches.len();
    Ok(Json(crate::service::grep::GrepResponse {
        matches,
        total,
        truncated,
    }))
}

/// Query params for `GET /indexes/{id}/call_chain` (issue #76).
///
/// Why: HTTP callers (and the MCP `get_call_chain` tool that proxies through
/// the daemon) need to specify an entry point and traversal options without
/// posting a JSON body.
/// What: mirrors the `get_call_chain` MCP tool args.
/// Test: integration test `test_call_chain_handler_*`.
#[derive(Debug, Deserialize)]
struct CallChainParams {
    entry_point: String,
    direction: Option<String>,
    max_depth: Option<u32>,
    include_source: Option<bool>,
}

/// `GET /indexes/{id}/call_chain?entry_point=...&direction=...&...` —
/// return an annotated call-tree report for a function (issue #76).
///
/// Why: LLM clients consume the response directly as plain text context, so
/// the body is `text/plain` (not JSON). The MCP `get_call_chain` tool calls
/// this endpoint and wraps the result in the standard `content[]` envelope.
/// What: snapshots the indexer's symbol graph + raw chunk corpus, hands them
/// to [`crate::service::call_chain::render_call_chain`], and returns the
/// resulting `String`. Returns 400 for invalid params, 404 for unknown
/// indexes or unresolvable entry points.
/// Test: covered by `service::call_chain::tests` (renderer) and the MCP
/// dispatch tests (transport contract).
async fn call_chain_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<CallChainParams>,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    use crate::service::call_chain::{render_call_chain, CallChainRequest};

    let req = CallChainRequest {
        index_id: id.clone(),
        entry_point: params.entry_point,
        direction: params.direction,
        max_depth: params.max_depth,
        include_source: params.include_source,
    };
    let validated = req.validate().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        )
    })?;

    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": format!("unknown index: {}", index_id.0) })),
        )
    })?;

    // Issue #313: skip_kg indexes have no symbol graph — return a structured
    // 503 so callers can distinguish "KG disabled" from "no symbols found".
    if handle.skip_kg {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "kg_unavailable",
                "reason": "skipped_by_config",
                "index": index_id.0,
            })),
        ));
    }

    let (graph, chunks) = {
        let indexer = handle.indexer.read().await;
        let graph = indexer.snapshot_symbol_graph().await;
        let chunks = indexer.raw_chunks_snapshot().await;
        (graph, chunks)
    };

    let text = render_call_chain(&validated, graph.as_ref(), &chunks).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": e })),
        )
    })?;
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        text,
    )
        .into_response())
}

/// Optional body for `POST /indexes/:id/reindex`: lets the CLI override the
/// `root_path` stored on the handle (useful when registering + reindexing in
/// one CLI flow).
#[derive(Deserialize, Default)]
pub struct ReindexRequest {
    #[serde(default)]
    pub root_path: Option<std::path::PathBuf>,
    /// When `true`, the daemon clears the per-index content-hash cache before
    /// walking the tree, forcing every file to be re-embedded even if its
    /// content hasn't changed. Set by `trusty-search index --force`.
    #[serde(default)]
    pub force: Option<bool>,
    /// When `true`, routes this reindex through the background (low-priority)
    /// semaphore so it cannot starve user-initiated requests (issue #458).
    /// Set by the startup auto-discover path; never sent by interactive CLI or
    /// MCP callers. Defaults to `false` (interactive/priority path).
    #[serde(default)]
    pub background: Option<bool>,
}

async fn reindex_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    body: Option<Json<ReindexRequest>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let index_id = IndexId::new(id.clone());
    let mut handle = state.registry.get(&index_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("unknown index: {}", index_id.0),
        })),
    ))?;

    // Issue #120: cooldown guard. If the most recent reindex for this index
    // aborted at the memory limit, refuse to queue another one for
    // `TRUSTY_REINDEX_COOLDOWN_SECS` (default 300 s). Re-running immediately
    // would just hit the limit again because the un-processed files have no
    // content-hash entries yet, producing an infinite reindex loop. Operators
    // can lower batch size / raise the memory limit and try again after the
    // cooldown elapses.
    if let Some(aborted_at) = state.last_reindex_aborted_at.get(&index_id) {
        let elapsed = aborted_at.elapsed();
        let cooldown = std::time::Duration::from_secs(
            std::env::var("TRUSTY_REINDEX_COOLDOWN_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
        );
        if elapsed < cooldown {
            let remaining_secs = (cooldown - elapsed).as_secs();
            tracing::warn!(
                "reindex_handler: refusing reindex for index {} — last run \
                 aborted at memory limit {}s ago, cooldown {}s remaining",
                index_id.0,
                elapsed.as_secs(),
                remaining_secs,
            );
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "reindex cooldown active after memory-limit abort",
                    "index_id": index_id.0,
                    "retry_after_secs": remaining_secs,
                    "cooldown_secs": cooldown.as_secs(),
                    "hint": "lower TRUSTY_MAX_BATCH_SIZE or raise TRUSTY_MEMORY_LIMIT_MB before retrying",
                })),
            ));
        }
        // Cooldown elapsed — drop the stale entry so the next abort (if any)
        // starts a fresh window. Done outside the `get()` guard to avoid
        // holding a DashMap shard lock across the removal.
        drop(aborted_at);
        state.last_reindex_aborted_at.remove(&index_id);
    }

    // If caller supplied a root_path and the stored handle doesn't have one
    // (or differs), re-register with the new path. We can't mutate the
    // existing Arc in place, but registering replaces the entry.
    let mut force = false;
    // Issue #458: `background=true` routes to the low-priority semaphore so
    // startup auto-discover reindexes never starve interactive requests.
    // Default false (interactive/priority path) when the field is absent.
    let mut is_interactive = true;
    if let Some(Json(req)) = body {
        force = req.force.unwrap_or(false);
        is_interactive = !req.background.unwrap_or(false);
        if let Some(new_root) = req.root_path {
            // Issue #63: a caller-supplied override must pass the same
            // absolute-existing-directory check as `POST /indexes`. Without
            // this, `POST /indexes/:id/reindex { root_path: "." }` would
            // silently re-point an existing index at the daemon's CWD.
            //
            // Issue (indexed-paths-mismatch): use the canonical form so a
            // re-register via a symlink alias normalises to the same identity
            // the original `POST /indexes` stored.
            let new_root = match validate_root_path(&new_root) {
                Ok(canonical) => canonical,
                Err(resp) => {
                    let (parts, body) = resp.into_parts();
                    let status = parts.status;
                    let body_bytes = axum::body::to_bytes(body, 4096).await.unwrap_or_default();
                    let json: serde_json::Value = serde_json::from_slice(&body_bytes)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    return Err((status, Json(json)));
                }
            };
            if handle.root_path.as_os_str().is_empty() || handle.root_path != new_root {
                let indexer = Arc::clone(&handle.indexer);
                // Preserve the filter set / domain vocabulary recorded on the
                // existing handle — only the root_path is being overridden.
                let new_handle = IndexHandle {
                    id: index_id.clone(),
                    indexer,
                    root_path: new_root,
                    include_paths: handle.include_paths.clone(),
                    exclude_globs: handle.exclude_globs.clone(),
                    extensions: handle.extensions.clone(),
                    domain_terms: handle.domain_terms.clone(),
                    include_docs: handle.include_docs,
                    respect_gitignore: handle.respect_gitignore,
                    path_filter: handle.path_filter.clone(),
                    // Preserve the previously inferred context (if any). A
                    // fresh reindex will overwrite this with the metadata
                    // scraped from the new root.
                    context_embedding: Arc::clone(&handle.context_embedding),
                    context_summary: Arc::clone(&handle.context_summary),
                    // Preserve the indexed-HEAD SHA across the root_path
                    // override — a subsequent reindex will refresh it.
                    indexed_head_sha: Arc::clone(&handle.indexed_head_sha),
                    // Issue #109, Phase 1: preserve the lexical-only flag
                    // and stages snapshot across the root override — a
                    // root_path override is not an opt-out change.
                    lexical_only: handle.lexical_only,
                    // Issue #313: preserve the skip_kg flag across the
                    // root_path override — the operator's KG choice is
                    // orthogonal to the path being indexed.
                    skip_kg: handle.skip_kg,
                    stages: Arc::clone(&handle.stages),
                    search_pressure: Arc::clone(&handle.search_pressure),
                    // Preserve walk diagnostics across root-path override — a
                    // subsequent reindex will refresh the snapshot.
                    walk_diagnostics: Arc::clone(&handle.walk_diagnostics),
                };
                handle = state.registry.register(new_handle);
            }
        }
    }

    // Replace any prior progress entry so SSE subscribers see fresh state.
    let progress = Arc::new(ReindexProgress::new());
    state
        .reindex_progress
        .insert(index_id.clone(), Arc::clone(&progress));

    spawn_reindex_with_cleanup(
        handle,
        progress,
        force,
        Some(Arc::clone(&state.reindex_progress)),
        Some(Arc::clone(&state.last_reindex_aborted_at)),
        // Issue #282: forward the live sidecar PID slot so the reindex
        // orchestrator can sample embedderd's RSS during the run and
        // emit `embedderd_peak_rss_mb` in the SSE `complete` event.
        Some(Arc::clone(&state.embedderd_pid_slot)),
        // Issue #458: route based on caller intent. `background=true` in the
        // request body maps to `priority=false` (background semaphore, never
        // blocks interactive requests). Default is interactive (priority=true).
        is_interactive,
    );

    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "queued": true,
        "stream_url": format!("/indexes/{}/reindex/stream", index_id.0),
    })))
}

/// Heartbeat interval for the reindex SSE stream.
///
/// Why: under memory pressure the embedderd sidecar can stall between batches,
/// leaving the SSE body idle for minutes. Without any bytes flowing, the OS
/// (or any intermediate proxy/reverse-proxy) tears down the idle TCP connection
/// before the terminal event is ever written — the client sees a decode error
/// or "stream ended without completion event". Emitting a comment-only SSE
/// frame (`: heartbeat\n\n`) every `SSE_HEARTBEAT_INTERVAL` keeps the body
/// transport alive so the connection survives long stalls.
/// What: used by `reindex_stream_handler` to pace the `IntervalStream`.
/// Test: covered indirectly by the full reindex path; the interval fires even
/// when no real events are produced.
const SSE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// SSE keep-alive heartbeat frame (SSE comment — ignored by all spec-compliant
/// clients including `eventsource-stream`).
const SSE_HEARTBEAT_FRAME: &str = ": heartbeat\n\n";

/// SSE stream of reindex progress events.
///
/// Why: Mirrors the `/status/stream` SSE pattern (manual `Response::builder()`
/// with `text/event-stream` + `no-cache` + `X-Accel-Buffering: no`).
/// Replays any events already buffered (so a late subscriber still sees the
/// `start` event) and then streams live events from the broadcast channel
/// until the reindex completes. Lagged subscribers receive a
/// `{"type":"lag","skipped":N}` frame. A 20 s keep-alive heartbeat (SSE
/// comment frame, ignored by clients) prevents the OS from tearing down the
/// idle TCP connection when the sidecar stalls between batches.
///
/// What: builds a merged stream of (a) broadcast events and (b) 20 s interval
/// heartbeats. The broadcast path produces `data:` frames; the heartbeat path
/// produces ``: heartbeat\n\n`` comment frames. The merged stream is wrapped
/// in `Body::from_stream` and returned as `text/event-stream`.
///
/// Test: `reindex_stream_handler` is exercised by the full-reindex integration
/// path. The heartbeat interval fires independently of real events so it
/// cannot be blocked by a stalled sidecar.
async fn reindex_stream_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let index_id = IndexId::new(id.clone());
    let progress = state
        .reindex_progress
        .get(&index_id)
        .map(|r| Arc::clone(r.value()))
        .ok_or_else(|| unknown_index_response(&id))?;

    // Snapshot the replay buffer first so we don't miss the `start` event,
    // then subscribe for live updates. New events that arrive between the
    // snapshot and subscription will appear in both — duplicates are harmless
    // for SSE consumers and rare in practice.
    let replay = progress.events.lock().await.clone();
    let initial_status = progress.status.load();
    let rx = progress.sender.subscribe();

    fn frame(line: String) -> Result<axum::body::Bytes, std::io::Error> {
        Ok(axum::body::Bytes::from(format!("data: {line}\n\n")))
    }

    let replay_stream = stream::iter(replay).map(frame);

    // If the reindex already finished before the subscriber connected, the
    // replay buffer contains the terminal `complete` event and the live
    // stream would idle forever. Return the replay only in that case.
    let body = if initial_status != ReindexStatus::Running {
        Body::from_stream(replay_stream)
    } else {
        // Live event stream from the broadcast channel.
        let live = BroadcastStream::new(rx).map(|res| match res {
            Ok(line) => frame(line),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => Ok(
                axum::body::Bytes::from(format!("data: {{\"type\":\"lag\",\"skipped\":{n}}}\n\n")),
            ),
        });

        // Keep-alive heartbeat: emit `: heartbeat\n\n` every 20 s so the
        // HTTP body never goes fully idle between events. SSE comment frames
        // (lines starting with ':') are mandated by the spec to be ignored
        // by all compliant clients including `eventsource-stream`.
        //
        // Why merge rather than chain: we need interleaving, not sequencing
        // — the heartbeat must fire even while the live stream is idle, and
        // the live stream must continue after a heartbeat tick.
        let heartbeat = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
            SSE_HEARTBEAT_INTERVAL,
        ))
        .map(|_| -> Result<axum::body::Bytes, std::io::Error> {
            Ok(axum::body::Bytes::from_static(
                SSE_HEARTBEAT_FRAME.as_bytes(),
            ))
        });

        // `stream::select` from `futures` interleaves two streams; the
        // merged stream ends when BOTH inputs end. The broadcast stream ends
        // when the sender (reindex task) drops; the interval stream runs
        // forever. Relying on the broadcast-stream termination is therefore
        // sufficient — the interval side is just a no-cost keep-alive.
        Body::from_stream(replay_stream.chain(stream::select(live, heartbeat)))
    };

    Ok(Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .expect("valid SSE response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `/health` is consumed by external probes (open-mpm,
    /// `ensure_daemon_running`) — the contract `{ status, version, indexes,
    /// uptime_secs }` must remain stable.
    /// What: Builds an AppState with N registered indexes and asserts the
    /// HealthResponse JSON shape and counts.
    /// Test: covers issue #34's acceptance (indexes counter + uptime_secs).
    #[tokio::test]
    async fn health_handler_reports_indexes_and_uptime() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        let id = IndexId::new("health-test");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(CodeIndexer::new(
                "health-test",
                "/tmp/health-test",
            ))),
            "/tmp/health-test".into(),
        ));
        let state = Arc::new(SearchAppState::new(registry));
        let Json(resp) = health_handler(State(state)).await;
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(resp.indexes, 1);
        // uptime_secs is u64 — always >= 0 by type; just exercise the path.
        let _ = resp.uptime_secs;
        // No embedder attached in this test. With the deferred-init flow,
        // a fresh `SearchAppState::new()` reports "initializing" (the
        // background task hasn't installed an embedder yet) rather than
        // "unavailable". "unavailable" is reserved for the post-failure
        // case where the init task explicitly errored.
        assert_eq!(resp.embedder, "initializing");
    }

    /// Issue #128 — `GET /indexes/{id}/graph` exports the full SymbolGraph.
    /// With a registered index holding inter-calling functions, the response
    /// must carry node/edge lists, a `stats` block, a `generated_at` stamp,
    /// and a 1-hour `Cache-Control` header.
    #[tokio::test]
    async fn graph_handler_exports_nodes_and_edges() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        let id = IndexId::new("graph-test");
        let indexer = CodeIndexer::new("graph-test", "/tmp/graph-test");
        // Two functions where `caller` calls `callee` — yields one node per
        // function and one CallsFunction edge.
        indexer
            .index_file(
                "graph-test/lib.rs",
                "fn callee() {}\nfn caller() { callee(); }\n",
            )
            .await
            .expect("index_file ok");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(indexer)),
            "/tmp/graph-test".into(),
        ));
        let state = Arc::new(SearchAppState::new(registry));

        let response = graph_handler(
            State(state),
            Path("graph-test".to_string()),
            Query(GraphQueryParams::default()),
        )
        .await
        .expect("handler ok");

        // 1-hour cache header is present.
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=3600"),
        );

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");

        let nodes = value["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 2, "two function symbols expected");
        for node in nodes {
            assert_eq!(node["type"].as_str(), Some("Symbol"));
            assert!(node["id"].is_string());
            assert!(node["label"].is_string());
            assert!(node["metadata"]["file"].is_string());
        }

        let edges = value["edges"].as_array().expect("edges array");
        assert_eq!(edges.len(), 1, "one CallsFunction edge expected");
        assert_eq!(edges[0]["source"].as_str(), Some("caller"));
        assert_eq!(edges[0]["target"].as_str(), Some("callee"));
        assert_eq!(edges[0]["type"].as_str(), Some("CallsFunction"));
        assert!(edges[0]["weight"].as_f64().is_some());

        assert_eq!(value["stats"]["node_count"].as_u64(), Some(2));
        assert_eq!(value["stats"]["edge_count"].as_u64(), Some(1));
        assert!(value["generated_at"].is_string());
    }

    /// Issue #128 — unknown index id returns 404 from `graph_handler`.
    #[tokio::test]
    async fn graph_handler_unknown_index_returns_404() {
        use crate::core::registry::IndexRegistry;
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let err = graph_handler(
            State(state),
            Path("does-not-exist".to_string()),
            Query(GraphQueryParams::default()),
        )
        .await
        .expect_err("missing index must 404");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(err.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unknown index");
        assert_eq!(json["index_id"], "does-not-exist");
    }

    /// Issue #128 — `edge_types` filter drops edges of other kinds.
    #[tokio::test]
    async fn graph_handler_filters_by_edge_type() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        let id = IndexId::new("graph-filter");
        let indexer = CodeIndexer::new("graph-filter", "/tmp/graph-filter");
        indexer
            .index_file(
                "graph-filter/lib.rs",
                "fn callee() {}\nfn caller() { callee(); }\n",
            )
            .await
            .expect("index_file ok");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(indexer)),
            "/tmp/graph-filter".into(),
        ));
        let state = Arc::new(SearchAppState::new(registry));

        // Filter to Implements only — the lone CallsFunction edge must drop.
        let response = graph_handler(
            State(state),
            Path("graph-filter".to_string()),
            Query(GraphQueryParams {
                types: None,
                edge_types: Some("Implements".to_string()),
                min_weight: None,
            }),
        )
        .await
        .expect("handler ok");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
        assert!(
            value["edges"].as_array().expect("edges").is_empty(),
            "CallsFunction edge must be filtered out",
        );
        // Nodes are unaffected by an edge-type filter.
        assert_eq!(value["nodes"].as_array().expect("nodes").len(), 2);
    }

    /// Issue #10 — `POST /search` fan-out: with two registered indexes each
    /// holding a single file, the global search must return results tagged
    /// with the correct `index_id` and the response must list both indexes
    /// as searched. BM25-only path (no embedder) keeps the test hermetic.
    #[tokio::test]
    async fn global_search_fans_out_and_merges() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        for name in ["proj-a", "proj-b"] {
            let id = IndexId::new(name);
            let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
            // Seed one file per index with content matching the query "alpha".
            indexer
                .index_file(
                    &format!("{name}/lib.rs"),
                    &format!("fn alpha_{name}() {{ println!(\"alpha hit\"); }}"),
                )
                .await
                .expect("index_file ok");
            registry.register(IndexHandle::bare(
                id.clone(),
                Arc::new(RwLock::new(indexer)),
                format!("/tmp/{name}").into(),
            ));
        }

        let state = Arc::new(SearchAppState::new(registry));
        let Json(value) = global_search_handler(
            State(state),
            Json(GlobalSearchRequest {
                query: "alpha".into(),
                top_k: 10,
                full_content: false,
                indexes: None,
                routing: None,
                routing_n: None,
                routing_threshold: None,
            }),
        )
        .await
        .expect("handler ok");

        let total = value["total_indexes"].as_u64().expect("total_indexes");
        assert_eq!(total, 2, "both indexes counted");

        let searched: Vec<String> = value["indexes_searched"]
            .as_array()
            .expect("indexes_searched array")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        assert_eq!(searched.len(), 2);
        assert!(searched.contains(&"proj-a".to_string()));
        assert!(searched.contains(&"proj-b".to_string()));

        let results = value["results"].as_array().expect("results array");
        assert!(!results.is_empty(), "expected at least one hit");
        // Every result must carry an index_id tagged with one of the two
        // registered indexes.
        let mut from_a = false;
        let mut from_b = false;
        for r in results {
            let idx = r["index_id"]
                .as_str()
                .expect("each result must be tagged with index_id");
            assert!(
                idx == "proj-a" || idx == "proj-b",
                "unexpected index_id: {idx}"
            );
            from_a |= idx == "proj-a";
            from_b |= idx == "proj-b";
        }
        // Both indexes share the same query term "alpha", so RRF should
        // surface at least one hit from each.
        assert!(from_a, "expected a result tagged with proj-a");
        assert!(from_b, "expected a result tagged with proj-b");
    }

    /// Issue #10 — `POST /search` with no indexes registered must return an
    /// empty result set (not 500). This guards the empty-registry edge case
    /// the fan-out path checks before spawning per-index futures.
    #[tokio::test]
    async fn global_search_empty_registry_returns_empty_results() {
        use crate::core::registry::IndexRegistry;
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let Json(value) = global_search_handler(
            State(state),
            Json(GlobalSearchRequest {
                query: "anything".into(),
                top_k: 5,
                full_content: false,
                indexes: None,
                routing: None,
                routing_n: None,
                routing_threshold: None,
            }),
        )
        .await
        .expect("handler ok");
        assert_eq!(value["total_indexes"].as_u64(), Some(0));
        assert!(value["results"].as_array().unwrap().is_empty());
        assert!(value["indexes_searched"].as_array().unwrap().is_empty());
    }

    /// Issue #110 — `POST /search` with explicit `indexes: [...]` must only
    /// fan out to the named indexes; results from indexes outside the
    /// allow-list must not appear, even when they match the query.
    #[tokio::test]
    async fn global_search_restricts_to_named_indexes() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        for name in ["proj-a", "proj-b", "proj-c"] {
            let id = IndexId::new(name);
            let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
            indexer
                .index_file(
                    &format!("{name}/lib.rs"),
                    &format!("fn alpha_{name}() {{ println!(\"alpha hit\"); }}"),
                )
                .await
                .expect("index_file ok");
            registry.register(IndexHandle::bare(
                id.clone(),
                Arc::new(RwLock::new(indexer)),
                format!("/tmp/{name}").into(),
            ));
        }
        let state = Arc::new(SearchAppState::new(registry));
        let Json(value) = global_search_handler(
            State(state),
            Json(GlobalSearchRequest {
                query: "alpha".into(),
                top_k: 10,
                full_content: false,
                indexes: Some(vec!["proj-a".into(), "proj-c".into()]),
                routing: None,
                routing_n: None,
                routing_threshold: None,
            }),
        )
        .await
        .expect("handler ok");

        // total_indexes reflects the *filtered* set we actually fanned out to.
        assert_eq!(value["total_indexes"].as_u64(), Some(2));

        let searched: std::collections::HashSet<String> = value["indexes_searched"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        assert!(searched.contains("proj-a"));
        assert!(searched.contains("proj-c"));
        assert!(!searched.contains("proj-b"), "proj-b must be excluded");

        for r in value["results"].as_array().unwrap() {
            let idx = r["index_id"].as_str().unwrap();
            assert_ne!(idx, "proj-b", "no result may come from excluded index");
        }
    }

    /// Issue #112: `RoutingMode::All` keeps every index and surfaces the
    /// cosine-similarity weight verbatim. Indexes without a weight entry
    /// fall back to 1.0.
    #[test]
    fn routing_mode_all_preserves_every_index_with_weights() {
        let ids = vec![IndexId::new("a"), IndexId::new("b"), IndexId::new("c")];
        let weights: std::collections::HashMap<IndexId, f32> = [
            (IndexId::new("a"), 0.9_f32),
            (IndexId::new("b"), 0.2),
            // "c" deliberately absent → falls back to 1.0
        ]
        .into_iter()
        .collect();

        let (active, map) = RoutingMode::All.apply(&ids, &weights);
        assert_eq!(active.len(), 3, "all routing keeps every index");
        assert!((map.get(&IndexId::new("a")).copied().unwrap() - 0.9).abs() < 1e-6);
        assert!((map.get(&IndexId::new("b")).copied().unwrap() - 0.2).abs() < 1e-6);
        assert!((map.get(&IndexId::new("c")).copied().unwrap() - 1.0).abs() < 1e-6);
    }

    /// Issue #112: `RoutingMode::TopN` keeps only the N highest-similarity
    /// indexes (ranked desc) and zeroes weights to 1.0 — selection has
    /// already absorbed relevance.
    #[test]
    fn routing_mode_top_n_keeps_only_highest_similarity() {
        let ids = vec![IndexId::new("low"), IndexId::new("hi"), IndexId::new("mid")];
        let weights: std::collections::HashMap<IndexId, f32> = [
            (IndexId::new("low"), 0.1_f32),
            (IndexId::new("hi"), 0.95),
            (IndexId::new("mid"), 0.5),
        ]
        .into_iter()
        .collect();

        let (active, map) = RoutingMode::TopN(2).apply(&ids, &weights);
        assert_eq!(active.len(), 2);
        let active_set: std::collections::HashSet<&str> =
            active.iter().map(|id| id.0.as_str()).collect();
        assert!(active_set.contains("hi"));
        assert!(active_set.contains("mid"));
        assert!(!active_set.contains("low"));
        // Selected entries normalised to weight 1.0.
        assert!((map.get(&IndexId::new("hi")).copied().unwrap() - 1.0).abs() < 1e-6);
        assert!((map.get(&IndexId::new("mid")).copied().unwrap() - 1.0).abs() < 1e-6);
        assert!(!map.contains_key(&IndexId::new("low")));
    }

    /// Issue #112: `RoutingMode::Threshold` drops anything strictly below
    /// the threshold and keeps entries at/above it.
    #[test]
    fn routing_mode_threshold_drops_below_cutoff() {
        let ids = vec![IndexId::new("a"), IndexId::new("b"), IndexId::new("c")];
        let weights: std::collections::HashMap<IndexId, f32> = [
            (IndexId::new("a"), 0.1_f32),
            (IndexId::new("b"), 0.5),
            (IndexId::new("c"), 0.8),
        ]
        .into_iter()
        .collect();

        let (active, map) = RoutingMode::Threshold(0.4).apply(&ids, &weights);
        let active_set: std::collections::HashSet<&str> =
            active.iter().map(|id| id.0.as_str()).collect();
        assert!(!active_set.contains("a"), "0.1 < 0.4 must drop");
        assert!(active_set.contains("b"), "0.5 >= 0.4 must keep");
        assert!(active_set.contains("c"));
        assert!(!map.contains_key(&IndexId::new("a")));
    }

    /// Indexes missing a weight entry default to neutral 1.0, so threshold
    /// routing must not silently drop them — otherwise "no metadata"
    /// becomes "no relevance" by accident.
    #[test]
    fn routing_threshold_keeps_neutral_indexes() {
        let ids = vec![IndexId::new("known"), IndexId::new("missing")];
        let weights: std::collections::HashMap<IndexId, f32> =
            [(IndexId::new("known"), 0.05_f32)].into_iter().collect();

        let (active, _map) = RoutingMode::Threshold(0.5).apply(&ids, &weights);
        let active_set: std::collections::HashSet<&str> =
            active.iter().map(|id| id.0.as_str()).collect();
        assert!(!active_set.contains("known"), "0.05 < 0.5 dropped");
        // Missing entries default to 1.0 → kept.
        assert!(
            active_set.contains("missing"),
            "indexes without a context embedding must use neutral 1.0 weight"
        );
    }

    /// Verify request → routing-mode resolution: missing or unknown values
    /// fall back to `All`; explicit values pick the right strategy and
    /// honour their `n` / `threshold` knobs.
    #[test]
    fn routing_mode_from_request_resolves_strategy() {
        let base =
            |routing: Option<&str>, n: Option<usize>, t: Option<f32>| -> GlobalSearchRequest {
                GlobalSearchRequest {
                    query: "x".into(),
                    top_k: 1,
                    full_content: false,
                    indexes: None,
                    routing: routing.map(|s| s.to_string()),
                    routing_n: n,
                    routing_threshold: t,
                }
            };
        assert!(matches!(
            RoutingMode::from_request(&base(None, None, None)),
            RoutingMode::All
        ));
        assert!(matches!(
            RoutingMode::from_request(&base(Some("garbage"), None, None)),
            RoutingMode::All
        ));
        match RoutingMode::from_request(&base(Some("top_n"), Some(5), None)) {
            RoutingMode::TopN(n) => assert_eq!(n, 5),
            _ => panic!("expected TopN"),
        }
        match RoutingMode::from_request(&base(Some("top_n"), None, None)) {
            RoutingMode::TopN(n) => assert_eq!(n, RoutingMode::DEFAULT_TOP_N),
            _ => panic!("expected TopN default"),
        }
        match RoutingMode::from_request(&base(Some("threshold"), None, Some(0.7))) {
            RoutingMode::Threshold(t) => assert!((t - 0.7).abs() < 1e-6),
            _ => panic!("expected Threshold"),
        }
    }

    /// Issue #121: after `install_embedder_error` records a hang/timeout,
    /// `/health` must report `embedder: "error"` plus a human-readable
    /// `embedder_error` field so operators don't waste hours debugging a
    /// daemon stuck in `"initializing"`.
    #[tokio::test]
    async fn install_embedder_error_surfaces_in_health() {
        use crate::core::registry::IndexRegistry;

        let state = SearchAppState::new(IndexRegistry::new());
        state
            .install_embedder_error("init timed out after 60s")
            .await;
        let state_arc = Arc::new(state);
        let Json(resp) = health_handler(State(state_arc)).await;
        assert_eq!(resp.embedder, "error");
        assert_eq!(
            resp.embedder_error.as_deref(),
            Some("init timed out after 60s"),
        );
    }

    /// Issue #121: when the embedder init task recorded a permanent error,
    /// `POST /indexes` must return a 503 carrying the error message rather
    /// than the generic "initializing" reason. Callers (CLI, MCP) rely on
    /// the message to surface the underlying cause to operators.
    #[tokio::test]
    async fn create_index_returns_503_with_error_when_embedder_failed() {
        use crate::core::registry::IndexRegistry;
        use axum::body::to_bytes;

        let state = SearchAppState::new(IndexRegistry::new());
        state
            .install_embedder_error("init timed out after 60s")
            .await;
        let state_arc = Arc::new(state);
        // Use a real tempdir so the issue #63 root_path validator (which now
        // runs ahead of the embedder check) accepts the path and the
        // handler proceeds to the embedder-error branch we're asserting on.
        let tmp = tempfile::tempdir().expect("tempdir");
        let resp = create_index_handler(
            State(state_arc),
            Json(CreateIndexRequest {
                id: "demo".to_string(),
                root_path: tmp.path().to_path_buf(),
                include_paths: None,
                exclude_globs: None,
                extensions: None,
                domain_terms: None,
                path_filter: None,
                include_docs: None,
                respect_gitignore: None,
                lexical_only: None,
                skip_kg: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("read body");
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid json");
        let err_str = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            err_str.contains("embedder init failed"),
            "expected error message to mention init failure, got: {err_str}",
        );
        assert!(
            err_str.contains("init timed out after 60s"),
            "expected recorded timeout message to be surfaced, got: {err_str}",
        );
    }

    /// Issue #121: after the embedder is installed successfully, a previously
    /// recorded error must be cleared so `/health` reports `"ready"` and not
    /// `"error"` (e.g. if a retry succeeded after a transient failure).
    #[tokio::test]
    async fn install_embedder_clears_previous_error() {
        use crate::core::embed::MockEmbedder;
        use crate::core::registry::IndexRegistry;

        let state = SearchAppState::new(IndexRegistry::new());
        state.install_embedder_error("transient hang").await;
        // Verify the error is recorded.
        assert!(state.current_embedder_error().is_some());

        // Install a healthy embedder — the error must clear.
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
        state.install_embedder(embedder).await;
        assert!(state.current_embedder_error().is_none());
        assert!(state.is_embedder_ready());

        let state_arc = Arc::new(state);
        let Json(resp) = health_handler(State(state_arc)).await;
        assert_eq!(resp.embedder, "ready");
        assert!(resp.embedder_error.is_none());
    }

    /// Issue #120: when the previous reindex for an index aborted at the
    /// memory limit, a follow-up `POST /indexes/:id/reindex` request must be
    /// refused with `429 Too Many Requests` for the duration of the cooldown.
    ///
    /// Why: without the guard, an external caller (CLI watchdog, open-mpm)
    /// that retries on abort would loop: each retry re-processes files that
    /// had no content-hash entry yet, pushes RSS over the limit again, and
    /// aborts again.
    /// What: stages an index, records a memory-abort timestamp, calls
    /// `reindex_handler` and asserts the 429 + JSON body shape. Then resets
    /// the cooldown env to 0 s, removes the entry, and verifies the next
    /// call queues successfully.
    /// Test: this test.
    #[tokio::test]
    async fn reindex_handler_rejects_within_cooldown() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        let id = IndexId::new("cooldown-test");
        let tmp = tempfile::tempdir().expect("tempdir");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(CodeIndexer::new("cooldown-test", tmp.path()))),
            tmp.path().to_path_buf(),
        ));
        let state = Arc::new(SearchAppState::new(registry));

        // Simulate a prior memory abort by writing a fresh timestamp.
        state
            .last_reindex_aborted_at
            .insert(id.clone(), std::time::Instant::now());

        // Default cooldown is 300 s — handler must refuse with 429.
        let result = reindex_handler(
            State(Arc::clone(&state)),
            axum::extract::Path("cooldown-test".to_string()),
            None,
        )
        .await;
        let err = result.expect_err("expected 429 inside cooldown window");
        assert_eq!(err.0, StatusCode::TOO_MANY_REQUESTS);
        let body = err.1 .0;
        assert!(body.get("retry_after_secs").is_some());
        assert!(body.get("hint").is_some());
        assert_eq!(body["index_id"], "cooldown-test");

        // Drop the abort entry and verify the next call queues successfully.
        state.last_reindex_aborted_at.remove(&id);
        let ok = reindex_handler(
            State(Arc::clone(&state)),
            axum::extract::Path("cooldown-test".to_string()),
            None,
        )
        .await
        .expect("queued");
        assert_eq!(ok.0["queued"], serde_json::Value::Bool(true));
    }

    /// Issue #120: the `AbortedMemory` variant must serialize to the
    /// kebab-case-but-lowercase form (`"abortedmemory"`) consistent with the
    /// existing `Complete`/`Failed`/`Running` variants. External callers
    /// parse the status string off the SSE stream, so the wire format is
    /// load-bearing.
    /// Test: this test.
    #[tokio::test]
    async fn reindex_status_aborted_memory_serializes_lowercase() {
        let status = crate::service::reindex::ReindexStatus::AbortedMemory;
        let json = serde_json::to_string(&status).expect("serialize");
        assert_eq!(json, "\"abortedmemory\"");
    }

    /// Issue #80 — `GET /indexes/:id/status` reports `"indexing"` while a
    /// reindex is `Running` and `"ready"` once it reaches a terminal state.
    ///
    /// Why: the admin UI / MCP `index_status` consumers relied on a `status`
    /// field that previously did not exist, so a long-running reindex looked
    /// identical to an idle index. Mapping the live `ReindexStatus` lets
    /// callers show an "indexing…" spinner and avoids reporting `"ready"`
    /// mid-reindex.
    /// What: stages a bare index, drives the per-index `ReindexProgress`
    /// through `Running` then `Complete`, and asserts the handler's `status`
    /// field flips from `"indexing"` to `"ready"`.
    /// Test: this test.
    #[tokio::test]
    async fn index_status_reports_indexing_then_ready() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use crate::service::reindex::{ReindexProgress, ReindexStatus};
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();
        let id = IndexId::new("status-test");
        let tmp = tempfile::tempdir().expect("tempdir");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(CodeIndexer::new("status-test", tmp.path()))),
            tmp.path().to_path_buf(),
        ));
        let state = Arc::new(SearchAppState::new(registry));

        // No progress entry yet → idle index reports "ready".
        let Json(idle) = index_status_handler(
            State(Arc::clone(&state)),
            axum::extract::Path("status-test".to_string()),
        )
        .await
        .expect("status 200");
        assert_eq!(idle["status"], "ready", "idle index must report ready");

        // A Running reindex must surface "indexing".
        let progress = Arc::new(ReindexProgress::new()); // defaults to Running
        state.reindex_progress.insert(id.clone(), progress.clone());
        let Json(running) = index_status_handler(
            State(Arc::clone(&state)),
            axum::extract::Path("status-test".to_string()),
        )
        .await
        .expect("status 200");
        assert_eq!(
            running["status"], "indexing",
            "running reindex must report indexing"
        );

        // A terminal state maps back to "ready".
        progress.status.store(ReindexStatus::Complete);
        let Json(done) = index_status_handler(
            State(Arc::clone(&state)),
            axum::extract::Path("status-test".to_string()),
        )
        .await
        .expect("status 200");
        assert_eq!(
            done["status"], "ready",
            "completed reindex must report ready"
        );
    }

    /// Issue #35 — `GET /health` carries the enriched resource fields
    /// (`rss_mb`, `rss_limit_mb`, `disk_bytes`, `cpu_pct`).
    ///
    /// Why: external probes and the admin UI render these; the JSON contract
    /// must remain stable. `rss_mb` is sampled live so it is asserted only
    /// for presence, not an exact value.
    /// What: builds a bare `SearchAppState`, calls `health_handler`, and
    /// asserts every new field deserialises with a plausible value.
    /// Test: this test.
    #[tokio::test]
    async fn health_includes_resource_fields() {
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let Json(resp) = health_handler(State(state)).await;
        // rss_mb is sampled from the live test process; tolerate 0 only in
        // sandboxes where /proc is restricted, but it must be a sane unit.
        assert!(resp.rss_mb < 1024 * 1024, "rss_mb unit must be MB");
        // cpu_pct is a non-negative percentage (first sample may be 0.0).
        assert!(resp.cpu_pct >= 0.0, "cpu_pct must be non-negative");
        // disk_bytes / rss_limit_mb are u64 — presence is the contract here;
        // the disk ticker has not run yet so disk_bytes is 0.
        assert_eq!(resp.disk_bytes, 0, "disk ticker has not ticked yet");
        let _ = resp.rss_limit_mb;
    }

    /// Issue #38 — `index_disk_and_mtime` returns `(None, None)` for an index
    /// whose per-index data directory does not exist.
    ///
    /// Why: a freshly-registered but never-reindexed index has no on-disk
    /// directory yet; the helper must degrade gracefully so `index_status`
    /// still responds 200 with JSON `null` for both fields rather than
    /// panicking or erroring.
    /// What: calls the helper with a random id that cannot have a data dir
    /// and asserts both halves are `None`.
    /// Test: this test.
    #[test]
    fn index_disk_and_mtime_handles_missing_dir() {
        let id = format!("nonexistent-index-{}", std::process::id());
        let (disk, mtime) = index_disk_and_mtime(&id);
        assert!(disk.is_none(), "missing dir yields no disk_bytes");
        assert!(mtime.is_none(), "missing dir yields no last_indexed");
    }

    /// Issue #80 — `first_existing_mtime_rfc3339` prefers `index.redb` over the
    /// legacy `chunks.json`, and falls back to `chunks.json` when only it
    /// exists.
    ///
    /// Why: the redb cutover left `last_indexed` permanently `null` because the
    /// selector read `chunks.json` (no longer rewritten) instead of the live
    /// `index.redb`. This pins the precedence so a regression re-introducing
    /// the JSON-only read is caught without standing up a daemon.
    /// What: writes both files into a tempdir, asserts the returned mtime
    /// matches `index.redb` (made strictly newer than `chunks.json`); then a
    /// chunks.json-only dir returns that file's mtime.
    /// Test: this test.
    #[test]
    fn last_indexed_prefers_redb_then_chunks_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();

        // Legacy snapshot first (older), then the authoritative redb (newer).
        std::fs::write(dir.join("chunks.json"), b"[]").expect("write chunks.json");
        // Ensure a strictly later mtime for index.redb so the assertion that we
        // picked redb (not chunks.json) is unambiguous.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(dir.join("index.redb"), b"redb").expect("write index.redb");

        let redb_mtime = std::fs::metadata(dir.join("index.redb"))
            .and_then(|m| m.modified())
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
            .expect("redb mtime");

        let got = first_existing_mtime_rfc3339(dir, &["index.redb", "chunks.json"]);
        assert_eq!(
            got.as_deref(),
            Some(redb_mtime.as_str()),
            "selector must prefer index.redb mtime over chunks.json"
        );

        // chunks.json-only fallback (un-migrated index).
        let tmp2 = tempfile::tempdir().expect("tempdir2");
        std::fs::write(tmp2.path().join("chunks.json"), b"[]").expect("write chunks.json");
        let fallback = first_existing_mtime_rfc3339(tmp2.path(), &["index.redb", "chunks.json"]);
        assert!(
            fallback.is_some(),
            "selector must fall back to chunks.json when index.redb is absent"
        );
    }

    /// Issue #80 — `first_existing_mtime_rfc3339` returns `None` when none of
    /// the candidate files exist.
    ///
    /// Why: a freshly-registered index has neither file; the selector must
    /// degrade to `None` so the handler reports `last_indexed: null` rather
    /// than panicking.
    /// What: calls the selector against an empty tempdir and asserts `None`.
    /// Test: this test.
    #[test]
    fn last_indexed_none_when_no_candidates_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let got = first_existing_mtime_rfc3339(tmp.path(), &["index.redb", "chunks.json"]);
        assert!(got.is_none(), "no candidate files → None");
    }

    /// Issue #38 — `/health` includes the `embedder_info` block once an
    /// embedder is wired, and omits it otherwise.
    ///
    /// Why: the admin UI's Health view renders the model dimension + provider
    /// from this block; a BM25-only daemon (no embedder) must omit it so the
    /// UI can show an honest "not available" state.
    /// What: builds a BM25-only state, asserts `embedder_info` is `None`.
    /// Test: this test.
    #[tokio::test]
    async fn health_omits_embedder_info_when_bm25_only() {
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let Json(resp) = health_handler(State(state)).await;
        assert!(
            resp.embedder_info.is_none(),
            "BM25-only daemon must omit embedder_info"
        );
    }

    /// Review #727 finding 3 — `/health` surfaces the leaked probe thread count.
    ///
    /// Why: confirms `warmboot_leaked_probe_threads` is present and reflects the
    /// process-global counter. `serial` + pre-call snapshot fixes the TOCTOU
    /// where reading the counter AFTER the handler call could see a higher value
    /// from a concurrently-running serial test, making the assertion flaky.
    ///
    /// What: capture counter before handler call; assert response field matches.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn health_includes_warmboot_leaked_probe_threads() {
        use crate::service::warm_boot::leaked_probe_thread_count;
        // Snapshot BEFORE the call: serial prevents concurrent increments.
        let expected = leaked_probe_thread_count();
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let Json(resp) = health_handler(State(state)).await;
        assert_eq!(
            resp.warmboot_leaked_probe_threads, expected,
            "warmboot_leaked_probe_threads must equal counter snapshot from before handler call"
        );
    }

    /// `GET /health` reports `chat_available` with correct semantics (issue #781).
    ///
    /// Why: The Svelte dashboard uses this flag to show/hide the Chat panel.
    /// Before #781 the flag was `openrouter_enabled || local_model.enabled`, but
    /// `LocalModelConfig::default()` has `enabled: true`, so it was always `true`
    /// even without a working provider. The fix: `true` only when an OpenRouter
    /// key is present OR when the lazy chat_provider OnceCell has resolved to Some.
    /// What: Three assertions: (1) key present → true, (2) no key + OnceCell not
    /// populated → false, (3) no key + OnceCell pre-populated with Some → true.
    /// Test: this test.
    #[tokio::test]
    async fn health_chat_available_semantics() {
        use tokio::sync::OnceCell;

        // Case 1: non-empty OpenRouter key → chat_available == true regardless
        // of the OnceCell state.
        let state_with_key = Arc::new(
            SearchAppState::new(IndexRegistry::new())
                .with_openrouter_api_key("sk-or-test-key-not-real"),
        );
        let Json(resp_with) = health_handler(State(state_with_key)).await;
        assert!(
            resp_with.chat_available,
            "chat_available must be true when OPENROUTER_API_KEY is non-empty"
        );

        // Case 2: no key, local_model.enabled=true (the DEFAULT), OnceCell not
        // populated yet → chat_available must be FALSE (the bug fixed by #781).
        let state_default_local = Arc::new(
            SearchAppState::new(IndexRegistry::new())
                .with_openrouter_api_key("")
                // Explicitly use the default local model config (enabled=true)
                // to prove the flag is NOT set just because enabled=true.
                .with_local_model(trusty_common::LocalModelConfig::default()),
        );
        let Json(resp_default) = health_handler(State(state_default_local)).await;
        assert!(
            !resp_default.chat_available,
            "chat_available must be false when no key and local model not yet probed \
             (OnceCell empty) — the #781 regression guard"
        );

        // Case 3: no key, OnceCell pre-populated with Some(provider) →
        // chat_available must be TRUE (a local model was probed and confirmed).
        // We simulate this by constructing a state and pre-initialising the
        // chat_provider OnceCell with a mock provider.
        let state_local_probed =
            SearchAppState::new(IndexRegistry::new()).with_openrouter_api_key("");
        let mock_provider: Arc<dyn trusty_common::ChatProvider> =
            Arc::new(trusty_common::OpenRouterProvider::new(
                "fake-key-for-test".to_string(),
                "test-model".to_string(),
            ));
        // Pre-populate the OnceCell to simulate a successful local-model probe.
        let cell: &OnceCell<Option<Arc<dyn trusty_common::ChatProvider>>> =
            &state_local_probed.chat_provider;
        cell.get_or_init(|| async { Some(mock_provider) }).await;
        let state_arc = Arc::new(state_local_probed);
        let Json(resp_probed) = health_handler(State(state_arc)).await;
        assert!(
            resp_probed.chat_available,
            "chat_available must be true when chat_provider OnceCell resolved to Some"
        );
    }

    /// Issue #35 — `GET /logs/tail` returns the most recent buffered lines.
    ///
    /// Why: operators inspect a running daemon via this endpoint; it must
    /// surface exactly what the shared `LogBuffer` holds and report `total`.
    /// What: attaches a `LogBuffer`, pushes three lines, calls the handler
    /// with `n=2`, and asserts the tail + `total` count.
    /// Test: this test.
    #[tokio::test]
    async fn logs_tail_returns_recent_lines() {
        let buffer = trusty_common::log_buffer::LogBuffer::new(100);
        buffer.push("line one".to_string());
        buffer.push("line two".to_string());
        buffer.push("line three".to_string());
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_log_buffer(buffer));
        let Json(body) = logs_tail_handler(State(state), Query(LogsTailParams { n: 2 })).await;
        let lines = body["lines"].as_array().expect("lines array");
        assert_eq!(lines.len(), 2, "n=2 must return two lines");
        assert_eq!(lines[0].as_str(), Some("line two"));
        assert_eq!(lines[1].as_str(), Some("line three"));
        assert_eq!(body["total"].as_u64(), Some(3), "total counts all buffered");
    }

    /// Issue #35 — `GET /logs/tail?n=` is clamped to `[1, MAX_LOGS_TAIL_N]`.
    ///
    /// Why: a misconfigured client must not be able to request more lines
    /// than the buffer holds, and `n=0` must still return at least one line.
    /// What: pushes one line, requests `n=0` and an oversized `n`, asserting
    /// both clamp to a valid result.
    /// Test: this test.
    #[tokio::test]
    async fn logs_tail_clamps_n() {
        let buffer = trusty_common::log_buffer::LogBuffer::new(100);
        for i in 0..5 {
            buffer.push(format!("l{i}"));
        }
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_log_buffer(buffer));
        // n=0 clamps up to 1.
        let Json(zero) =
            logs_tail_handler(State(Arc::clone(&state)), Query(LogsTailParams { n: 0 })).await;
        assert_eq!(zero["lines"].as_array().expect("lines").len(), 1);
        // n past MAX clamps down to the buffer length (5 here).
        let Json(big) = logs_tail_handler(
            State(state),
            Query(LogsTailParams {
                n: MAX_LOGS_TAIL_N * 10,
            }),
        )
        .await;
        assert_eq!(big["lines"].as_array().expect("lines").len(), 5);
    }

    /// Issue #35 — `POST /admin/stop` acknowledges the shutdown request.
    ///
    /// Why: the response shape `{ ok, message }` is the documented contract
    /// for the admin UI's stop button.
    /// What: calls `admin_stop_handler` and asserts the JSON body. It does
    /// NOT await the spawned exit task — that would terminate the test
    /// process — but the 200 ms delay before `process::exit` guarantees the
    /// test returns first.
    /// Test: this test.
    #[tokio::test]
    async fn admin_stop_returns_ok() {
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let Json(body) = admin_stop_handler(State(state)).await;
        assert_eq!(body["ok"], serde_json::Value::Bool(true));
        assert_eq!(body["message"].as_str(), Some("shutting down"));
    }

    // ── Issue #63 / #64: root_path validation + cross-index bleed guards ──

    /// Issue #63: a relative `root_path` must be rejected with `400` and a
    /// helpful message — silently resolving it against the daemon's CWD is
    /// the exact bug we are fixing.
    #[tokio::test]
    async fn create_index_rejects_relative_root_path() {
        use crate::core::registry::IndexRegistry;
        use axum::body::to_bytes;

        let state = SearchAppState::new(IndexRegistry::new());
        // Install a working embedder so we get past the readiness gate and
        // actually exercise the path validator.
        let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
        state.install_embedder(embedder).await;
        let state_arc = Arc::new(state);
        let resp = create_index_handler(
            State(state_arc),
            Json(CreateIndexRequest {
                id: "rel-bad".into(),
                root_path: std::path::PathBuf::from("claude-mpm"),
                include_paths: None,
                exclude_globs: None,
                extensions: None,
                domain_terms: None,
                path_filter: None,
                include_docs: None,
                respect_gitignore: None,
                lexical_only: None,
                skip_kg: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 4096).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
        assert!(err.contains("absolute"), "got: {err}");
    }

    /// Issue #63: an absolute-but-nonexistent `root_path` must also be
    /// rejected. Prevents creating an index that points at a directory that
    /// has not been created yet (the reindex walker would see no files,
    /// silently producing an empty index named after a real project).
    #[tokio::test]
    async fn create_index_rejects_nonexistent_root_path() {
        use crate::core::registry::IndexRegistry;
        use axum::body::to_bytes;

        let state = SearchAppState::new(IndexRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
        state.install_embedder(embedder).await;
        let state_arc = Arc::new(state);
        let resp = create_index_handler(
            State(state_arc),
            Json(CreateIndexRequest {
                id: "ghost".into(),
                root_path: std::path::PathBuf::from(
                    "/this/path/should/never/exist/trusty-search-test-xyz",
                ),
                include_paths: None,
                exclude_globs: None,
                extensions: None,
                domain_terms: None,
                path_filter: None,
                include_docs: None,
                respect_gitignore: None,
                lexical_only: None,
                skip_kg: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 4096).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
        assert!(err.contains("does not exist"), "got: {err}");
    }

    /// Issue (indexed-paths-mismatch): when the caller supplies a `root_path`
    /// that is a symlink to a real directory, the handler must canonicalise
    /// it before storing on the `IndexHandle`. Otherwise the registry holds
    /// the symlink alias, the walker emits file paths under the alias, and
    /// search queries from the canonical mount point return zero hits because
    /// `file_is_within_root` won't match.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_index_canonicalizes_symlinked_root_path() {
        use crate::core::registry::IndexId;
        use crate::core::registry::IndexRegistry;
        use std::os::unix::fs::symlink;

        let state = SearchAppState::new(IndexRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
        state.install_embedder(embedder).await;
        let state_arc = Arc::new(state);

        let tmp = tempfile::tempdir().expect("tempdir");
        let real_root = std::fs::canonicalize(tmp.path()).expect("canonicalize real root");
        let parent = real_root.parent().expect("tempdir has parent");
        let link_path = parent.join(format!(
            "trusty-search-server-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&link_path);
        symlink(&real_root, &link_path).expect("create symlink");

        let resp = create_index_handler(
            State(Arc::clone(&state_arc)),
            Json(CreateIndexRequest {
                id: "symlinked".into(),
                // Register via the SYMLINK path — the registry should still
                // store the CANONICAL path so search queries from either
                // alias resolve identically.
                root_path: link_path.clone(),
                include_paths: None,
                exclude_globs: None,
                extensions: None,
                domain_terms: None,
                path_filter: None,
                include_docs: None,
                respect_gitignore: None,
                lexical_only: None,
                skip_kg: None,
            }),
        )
        .await;
        let _ = std::fs::remove_file(&link_path); // best-effort cleanup
        assert_eq!(resp.status(), StatusCode::OK);

        let handle = state_arc
            .registry
            .get(&IndexId::new("symlinked"))
            .expect("registered handle");
        assert_eq!(
            handle.root_path, real_root,
            "registry stored the symlink alias instead of the canonical path",
        );
        assert_ne!(
            handle.root_path, link_path,
            "registry retained the symlink alias — downstream walkers will mismatch",
        );
    }

    /// Issue #63: an absolute, existing directory must be accepted.
    #[tokio::test]
    async fn create_index_accepts_valid_absolute_root_path() {
        use crate::core::registry::IndexRegistry;

        let state = SearchAppState::new(IndexRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
        state.install_embedder(embedder).await;
        let state_arc = Arc::new(state);
        let tmp = tempfile::tempdir().expect("tempdir");
        let resp = create_index_handler(
            State(Arc::clone(&state_arc)),
            Json(CreateIndexRequest {
                id: "valid-abs".into(),
                root_path: tmp.path().to_path_buf(),
                include_paths: None,
                exclude_globs: None,
                extensions: None,
                domain_terms: None,
                path_filter: None,
                include_docs: None,
                respect_gitignore: None,
                lexical_only: None,
                skip_kg: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Issue #64: `file_is_within_root` accepts clean relative paths (the
    /// normal stored shape) and rejects path-traversal attempts.
    #[test]
    fn file_is_within_root_relative_ok() {
        let root = std::path::Path::new("/Users/me/proj");
        assert!(file_is_within_root("src/auth.rs", root));
        assert!(file_is_within_root("./src/auth.rs", root));
        assert!(file_is_within_root("Cargo.toml", root));
    }

    /// Issue #64: relative paths that climb out via `..` must be rejected,
    /// even though they may resolve inside `root` for some `root` values.
    #[test]
    fn file_is_within_root_rejects_dotdot() {
        let root = std::path::Path::new("/Users/me/proj");
        assert!(!file_is_within_root("../other/file.rs", root));
        assert!(!file_is_within_root("src/../../leak.rs", root));
    }

    /// Issue #64: absolute paths must literally start with the index root.
    /// This is the load-bearing guard against cross-index bleed when the
    /// daemon ever stores absolute file paths (e.g. legacy chunks from a
    /// misregistered index — see #63).
    #[test]
    fn file_is_within_root_absolute_must_start_with_root() {
        let root = std::path::Path::new("/Users/me/proj");
        assert!(file_is_within_root("/Users/me/proj/src/auth.rs", root));
        assert!(!file_is_within_root(
            "/Users/me/other-proj/src/auth.rs",
            root
        ));
        assert!(!file_is_within_root("/etc/passwd", root));
    }

    /// Issue #64: empty file strings are defensively rejected — they should
    /// never occur in a valid chunk and we don't want them sneaking past
    /// the filter as a benign-looking relative path.
    #[test]
    fn file_is_within_root_rejects_empty() {
        let root = std::path::Path::new("/Users/me/proj");
        assert!(!file_is_within_root("", root));
    }

    /// Issue #541: when the index root is a symlink alias pointing at a real
    /// directory, an absolute file path stored under the real (canonical) root
    /// must NOT be dropped — `file_is_within_root` must fall back to
    /// canonicalized comparison and return `true`.
    ///
    /// This exercises the slow-path fallback added for #541: the lexical check
    /// `/real/dir/src/auth.rs`.starts_with(`/link`) fails, so the predicate
    /// canonicalizes both sides and retries.
    #[cfg(unix)]
    #[test]
    fn file_is_within_root_symlinked_root_does_not_drop_valid_result() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        // Create a real directory that will be the "canonical" root.
        let real_dir = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(real_dir.path()).unwrap();

        // Symlink → real_dir (the handle holds the symlink path as its root_path).
        let link = canonical_root
            .parent()
            .unwrap()
            .join(format!("trusty-541-root-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        symlink(&canonical_root, &link).expect("create symlink");

        // A file stored with its canonical (non-symlink) absolute path — this
        // is exactly what the indexer produces after walking the real directory.
        let file_path = canonical_root.join("src/auth.rs");
        let file_str = file_path.to_str().unwrap();

        // With the link as `root`, the lexical check fails but the canonical
        // fallback must pass — the file IS within the root.
        let result = file_is_within_root(file_str, &link);
        let _ = std::fs::remove_file(&link);

        assert!(
            result,
            "file under canonical root must pass even when index root is a symlink alias; \
             file={file_str}, root={link}",
            link = link.display(),
        );
    }

    /// Issue #541: a file genuinely outside the root must still be rejected
    /// even after the canonicalize fallback runs.
    #[test]
    fn file_is_within_root_outside_root_still_rejected_after_canonicalize() {
        use tempfile::tempdir;

        let root_dir = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(root_dir.path()).unwrap();

        // A path that is definitely outside the root.
        let outside = "/etc/passwd";
        assert!(
            !file_is_within_root(outside, &canonical_root),
            "path genuinely outside root must still be rejected"
        );
    }

    /// Issue #541: `search_handler` must always include `stale_index_root` in
    /// the response `meta` block (as a boolean). When no results are dropped by
    /// the out-of-root filter the field is `false`; we verify its presence and
    /// type because the BM25 / MockEmbedder may return 0 results on a minimal
    /// test index, making it hard to guarantee `true` without complex setup.
    /// What: builds a minimal bare index, calls `search_handler`, and asserts the
    /// `stale_index_root` field is present and boolean in the `meta` block.
    /// Test: this test.
    #[tokio::test]
    async fn search_handler_meta_includes_stale_index_root_field() {
        use crate::core::embed::{Embedder, MockEmbedder};
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
        use crate::core::store::{UsearchStore, VectorStore};
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let dim = 16;
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
        let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
        let indexer = CodeIndexer::new("stale-meta-test", tmp.path())
            .with_components(Arc::clone(&embedder), Arc::clone(&store));

        let registry = IndexRegistry::new();
        let handle = IndexHandle::bare(
            IndexId::new("stale-meta-idx"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            tmp.path().to_path_buf(),
        );
        registry.register(handle);

        let state = Arc::new(SearchAppState::new(registry));
        state.install_embedder(embedder).await;

        let resp = search_handler(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Path("stale-meta-idx".to_string()),
            axum::extract::Json(crate::core::indexer::SearchQuery {
                text: "hello".to_string(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                branch_files: None,
                branch_boost: 1.5,
                branch: None,
                stage: Some(crate::core::indexer::SearchStage::Lexical),
                mode: crate::core::indexer::SearchMode::Code,
                exclude_archived: false,
                refine_query: None,
            }),
        )
        .await;

        let Json(body) = resp.expect("handler must succeed");
        let meta = body.get("meta").expect("meta block present");

        assert!(
            meta.get("stale_index_root").is_some(),
            "meta block must contain stale_index_root field; meta={meta:?}"
        );
        assert!(
            meta["stale_index_root"].is_boolean(),
            "stale_index_root must be a boolean; got={:?}",
            meta["stale_index_root"]
        );
        // For an empty index (no chunks were added), no results can be dropped,
        // so stale_index_root must be false.
        assert_eq!(
            meta["stale_index_root"], false,
            "stale_index_root must be false when no results were dropped"
        );
    }

    // ── /grep endpoint ──────────────────────────────────────────────────────

    /// Stage a single-index registry whose chunks point at real files written
    /// under a tempdir root, so `grep_one_index` has something to read.
    ///
    /// Why: the grep handlers walk the index's distinct file set and read each
    /// file fresh from disk; an integration test therefore needs both indexed
    /// chunks *and* matching on-disk files. This helper wires both up with a
    /// `MockEmbedder` (no ONNX) and returns the held `TempDir` so callers keep
    /// the files alive for the duration of the test.
    /// What: writes each `(rel_path, content)` under the tempdir, registers a
    /// `CodeIndexer` with components, `add_chunk`s a `RawChunk` per file, and
    /// returns `(state, index_id, tempdir)`.
    /// Test: consumed by the `grep_*` tests below.
    async fn stage_grep_index(
        files: &[(&str, &str)],
    ) -> (Arc<SearchAppState>, IndexId, tempfile::TempDir) {
        use crate::core::chunker::{ChunkType, RawChunk};
        use crate::core::embed::{Embedder, MockEmbedder};
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexRegistry};
        use crate::core::store::{UsearchStore, VectorStore};

        let tmp = tempfile::tempdir().expect("tempdir");
        let dim = 16;
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
        let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
        let indexer = CodeIndexer::new("grep-test", tmp.path()).with_components(embedder, store);

        for (i, (rel, content)) in files.iter().enumerate() {
            let abs = tmp.path().join(rel);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).expect("mkdirs");
            }
            std::fs::write(&abs, content).expect("write file");
            let chunk = RawChunk {
                id: format!("c{i}"),
                file: rel.to_string(),
                start_line: 1,
                end_line: 1 + content.lines().count(),
                content: content.to_string(),
                function_name: None,
                language: Some("rust".to_string()),
                chunk_type: ChunkType::Code,
                calls: Vec::new(),
                inherits_from: Vec::new(),
                chunk_depth: 0,
                parent_chunk_id: None,
                child_chunk_ids: Vec::new(),
                nlp_keywords: Vec::new(),
                nlp_code_refs: Vec::new(),
                virtual_terms: Vec::new(),
            };
            indexer.add_chunk(chunk).await.expect("add_chunk");
        }

        let registry = IndexRegistry::new();
        let id = IndexId::new("grep-test");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(indexer)),
            tmp.path().to_path_buf(),
        ));
        (Arc::new(SearchAppState::new(registry)), id, tmp)
    }

    fn grep_req(pattern: &str) -> crate::service::grep::GrepRequest {
        serde_json::from_value(serde_json::json!({ "pattern": pattern }))
            .expect("default grep request")
    }

    /// `POST /indexes/:id/grep` returns line-accurate matches read fresh from
    /// the on-disk files the index knows about.
    #[tokio::test]
    async fn grep_endpoint_returns_matches() {
        let (state, _id, _tmp) = stage_grep_index(&[
            ("src/auth.rs", "// header\nfn authenticate() {}\n"),
            ("src/util.rs", "fn helper() {}\n"),
        ])
        .await;

        let Json(resp) = grep_handler(
            State(state),
            Path("grep-test".to_string()),
            Json(grep_req("authenticate")),
        )
        .await
        .expect("200");

        assert_eq!(resp.total, 1);
        assert!(!resp.truncated);
        assert_eq!(resp.matches[0].file, "src/auth.rs");
        assert_eq!(resp.matches[0].line, 2);
        assert_eq!(resp.matches[0].text, "fn authenticate() {}");
    }

    /// The glob filter restricts which indexed files are read.
    #[tokio::test]
    async fn grep_endpoint_honours_glob() {
        let (state, _id, _tmp) = stage_grep_index(&[
            ("src/auth.rs", "fn target() {}\n"),
            ("docs/readme.md", "target appears here too\n"),
        ])
        .await;

        let mut req = grep_req("target");
        req.glob = Some("**/*.rs".to_string());
        let Json(resp) = grep_handler(State(state), Path("grep-test".to_string()), Json(req))
            .await
            .expect("200");
        assert_eq!(resp.total, 1);
        assert_eq!(resp.matches[0].file, "src/auth.rs");
    }

    /// A malformed regex yields `400 Bad Request` with a JSON error body.
    #[tokio::test]
    async fn grep_endpoint_bad_regex_is_400() {
        let (state, _id, _tmp) = stage_grep_index(&[("a.rs", "fn x() {}\n")]).await;
        let err = grep_handler(
            State(state),
            Path("grep-test".to_string()),
            Json(grep_req("(unclosed")),
        )
        .await
        .expect_err("400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1 .0.get("error").is_some());
    }

    /// An unknown index id yields `404 Not Found`.
    #[tokio::test]
    async fn grep_endpoint_unknown_index_is_404() {
        let (state, _id, _tmp) = stage_grep_index(&[("a.rs", "fn x() {}\n")]).await;
        let err = grep_handler(
            State(state),
            Path("does-not-exist".to_string()),
            Json(grep_req("x")),
        )
        .await
        .expect_err("404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    /// `POST /grep` (global) fans out across every registered index.
    #[tokio::test]
    async fn grep_global_fans_out() {
        let (state, _id, _tmp) =
            stage_grep_index(&[("src/auth.rs", "fn authenticate() {}\n")]).await;
        let Json(resp) = global_grep_handler(State(state), Json(grep_req("authenticate")))
            .await
            .expect("200");
        assert_eq!(resp.total, 1);
        assert_eq!(resp.matches[0].file, "src/auth.rs");
    }

    /// Global grep with an `index_id` that doesn't exist returns an empty set
    /// (tolerant fan-out), not a 404.
    #[tokio::test]
    async fn grep_global_respects_index_filter() {
        let (state, _id, _tmp) = stage_grep_index(&[("a.rs", "fn x() {}\n")]).await;
        let mut req = grep_req("x");
        req.index_id = Some("nope".to_string());
        let Json(resp) = global_grep_handler(State(state), Json(req))
            .await
            .expect("200");
        assert_eq!(resp.total, 0);
        assert!(!resp.truncated);
    }

    // ── Issue #404: nested-index fan-out tests ────────────────────────────

    /// `GET /indexes` default (no query param) returns the flat string array
    /// unchanged — byte-compatible with the pre-#404 response shape.
    #[tokio::test]
    async fn list_indexes_flat_default_unchanged() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };

        let registry = IndexRegistry::new();
        for name in ["alpha", "beta"] {
            let id = IndexId::new(name);
            let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
            registry.register(IndexHandle::bare(
                id.clone(),
                std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
                format!("/tmp/{name}").into(),
            ));
        }
        let state = std::sync::Arc::new(SearchAppState::new(registry));

        // No format param → flat list
        let resp = list_indexes_handler(
            State(state),
            Query(ListIndexesParams {
                format: None,
                details: false,
            }),
        )
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = value["indexes"].as_array().expect("indexes array");
        // Must be strings (flat format)
        for item in arr {
            assert!(
                item.is_string(),
                "flat default must return string IDs: {item:?}"
            );
        }
        assert_eq!(arr.len(), 2);
    }

    /// `GET /indexes?format=tree` returns an object-array with hierarchy
    /// fields (`parent_id`, `children`, `priority_boost`, `is_sub_index`).
    #[tokio::test]
    async fn list_indexes_tree_format_shape() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let registry = IndexRegistry::new();

        // Register a parent and child whose root_paths have a strict prefix
        // relationship.  We use non-existent paths so canonicalize_best_effort
        // falls back to the raw strings for both, giving a deterministic
        // comparison on all platforms without requiring real directories.
        let parent_id = IndexId::new("tree-parent");
        let child_id = IndexId::new("tree-child");

        let parent_root: std::path::PathBuf = "/nonexistent_test_root_abc".into();
        let child_root: std::path::PathBuf = "/nonexistent_test_root_abc/services/billing".into();

        registry.register(IndexHandle::bare(
            parent_id.clone(),
            Arc::new(RwLock::new(CodeIndexer::new(
                "tree-parent",
                "/nonexistent_test_root_abc",
            ))),
            parent_root,
        ));
        registry.register(IndexHandle::bare(
            child_id.clone(),
            Arc::new(RwLock::new(CodeIndexer::new(
                "tree-child",
                "/nonexistent_test_root_abc/services/billing",
            ))),
            child_root,
        ));

        let state = Arc::new(SearchAppState::new(registry));

        let resp = list_indexes_handler(
            State(state),
            Query(ListIndexesParams {
                format: Some("tree".to_string()),
                details: false,
            }),
        )
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = value["indexes"].as_array().expect("indexes array");
        assert_eq!(arr.len(), 2);
        // Each entry must be an object with required fields.
        for entry in arr {
            assert!(entry["id"].is_string(), "id must be string");
            assert!(entry["root_path"].is_string(), "root_path must be present");
            assert!(
                entry["priority_boost"].is_number(),
                "priority_boost must be a number"
            );
            assert!(
                entry["is_sub_index"].is_boolean(),
                "is_sub_index must be bool"
            );
            assert!(entry["children"].is_array(), "children must be an array");
        }

        // tree-child (/tmp/tree_child_sub_test) is a sub-path of /tmp →
        // it should be identified as a sub-index.
        let child_entry = arr
            .iter()
            .find(|e| e["id"].as_str() == Some("tree-child"))
            .expect("tree-child entry");
        assert_eq!(
            child_entry["is_sub_index"].as_bool(),
            Some(true),
            "tree-child must be a sub-index"
        );
        let parent_entry = arr
            .iter()
            .find(|e| e["id"].as_str() == Some("tree-parent"))
            .expect("tree-parent entry");
        assert_eq!(
            parent_entry["is_sub_index"].as_bool(),
            Some(false),
            "tree-parent must not be a sub-index"
        );
    }

    /// `GET /indexes?details=true` returns objects with `id` and `size_bytes`
    /// fields (issue #312).  When the index data dir has never been created
    /// `size_bytes` must be `null` rather than missing or erroring.
    ///
    /// Why: MCP `list_indexes` and the admin UI need per-index disk usage in a
    /// single call; the `?details=true` variant is the additive/backward-compat
    /// way to expose it without breaking the bare flat format.
    /// Test: this function.
    #[tokio::test]
    async fn list_indexes_details_includes_size_bytes() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };

        let registry = IndexRegistry::new();
        for name in ["detail-alpha", "detail-beta"] {
            let id = IndexId::new(name);
            let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
            registry.register(IndexHandle::bare(
                id.clone(),
                std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
                format!("/tmp/{name}").into(),
            ));
        }
        let state = std::sync::Arc::new(SearchAppState::new(registry));

        let resp = list_indexes_handler(
            State(state),
            Query(ListIndexesParams {
                format: None,
                details: true,
            }),
        )
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = value["indexes"].as_array().expect("indexes array");
        assert_eq!(arr.len(), 2);
        for entry in arr {
            assert!(
                entry["id"].is_string(),
                "each detail entry must have a string id: {entry:?}"
            );
            // size_bytes must be present: either a number or null (dir not
            // created yet), never missing entirely.
            assert!(
                entry.get("size_bytes").is_some(),
                "each detail entry must have a size_bytes field: {entry:?}"
            );
            // root_path must be present (issue #661 — auto-derive support).
            assert!(
                entry.get("root_path").is_some(),
                "each detail entry must have a root_path field (issue #661): {entry:?}"
            );
        }
    }

    /// `GET /indexes?details=true` exposes the registered `root_path` per index
    /// so trusty-review can auto-derive the correct index from the project cwd.
    ///
    /// Why: issue #661 — user-level MCP wiring omits TRUSTY_SEARCH_INDEX;
    /// trusty-review must match the current repo root to a registered index
    /// without issuing N individual status requests.
    /// What: registers one index with a known root path, issues the details
    /// request, and asserts the returned `root_path` matches what was registered.
    /// Test: this function.
    #[tokio::test]
    async fn list_indexes_details_includes_root_path() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };

        let registry = IndexRegistry::new();
        let id = IndexId::new("rp-test");
        let indexer = CodeIndexer::new("rp-test", "/tmp/rp-test");
        registry.register(IndexHandle::bare(
            id.clone(),
            std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/rp-test"),
        ));
        let state = std::sync::Arc::new(SearchAppState::new(registry));

        let resp = list_indexes_handler(
            State(state),
            Query(ListIndexesParams {
                format: None,
                details: true,
            }),
        )
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = value["indexes"].as_array().expect("indexes array");
        assert_eq!(arr.len(), 1, "expected exactly one index entry");
        let entry = &arr[0];
        assert_eq!(
            entry["id"].as_str(),
            Some("rp-test"),
            "id must match registered index id"
        );
        let rp = entry["root_path"]
            .as_str()
            .expect("root_path must be a non-null string");
        assert_eq!(
            rp, "/tmp/rp-test",
            "root_path must match what was registered"
        );
    }

    /// `POST /search` with nested indexes: the response must include
    /// `hierarchy_dedup_count` and the sub-index result should be preferred
    /// when both parent and child contain the same file region.
    #[tokio::test]
    async fn global_search_nested_hierarchy_dedup_count_present() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        // Two flat peer indexes (no nesting) — dedup_count should be 0.
        let registry = IndexRegistry::new();
        for name in ["flat-a", "flat-b"] {
            let id = IndexId::new(name);
            let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
            indexer
                .index_file(
                    &format!("{name}/lib.rs"),
                    "fn beta_function() { println!(\"beta\"); }",
                )
                .await
                .expect("index_file");
            registry.register(IndexHandle::bare(
                id.clone(),
                Arc::new(RwLock::new(indexer)),
                format!("/tmp/{name}").into(),
            ));
        }
        let state = Arc::new(SearchAppState::new(registry));

        let Json(value) = global_search_handler(
            State(state),
            Json(GlobalSearchRequest {
                query: "beta_function".into(),
                top_k: 10,
                full_content: false,
                indexes: None,
                routing: None,
                routing_n: None,
                routing_threshold: None,
            }),
        )
        .await
        .expect("handler ok");

        // Must include the new field regardless of whether dedup fired.
        assert!(
            value["hierarchy_dedup_count"].is_number(),
            "hierarchy_dedup_count must be present: {value:?}"
        );
        // Flat peers → no nesting → count must be 0.
        assert_eq!(
            value["hierarchy_dedup_count"].as_u64(),
            Some(0),
            "flat peers must not trigger dedup"
        );
    }

    /// `POST /search` with a sub-index: the effective lane weight for the
    /// sub-index must be boosted, and `hierarchy_dedup_count` reflects any
    /// dropped parent copies.
    #[tokio::test]
    async fn global_search_sub_index_boost_applied() {
        use crate::core::{
            indexer::CodeIndexer,
            registry::{IndexHandle, IndexId, IndexRegistry},
        };
        use std::sync::Arc;
        use tokio::sync::RwLock;

        // Use non-existent paths so canonicalize_best_effort falls back to raw
        // string comparison on all platforms (avoids /tmp → /private/tmp macOS
        // symlink mismatch).
        let registry = IndexRegistry::new();

        let parent_root: std::path::PathBuf = "/nonexistent_boost_root".into();
        let child_root: std::path::PathBuf = "/nonexistent_boost_root/sub".into();

        let parent_id = IndexId::new("boost-parent");
        let child_id = IndexId::new("boost-child");

        let parent_indexer = CodeIndexer::new("boost-parent", "/nonexistent_boost_root");
        parent_indexer
            .index_file("src/lib.rs", "fn gamma_function() { println!(\"gamma\"); }")
            .await
            .expect("parent index_file");
        registry.register(IndexHandle::bare(
            parent_id.clone(),
            Arc::new(RwLock::new(parent_indexer)),
            parent_root,
        ));

        let child_indexer = CodeIndexer::new("boost-child", "/nonexistent_boost_root/sub");
        child_indexer
            .index_file(
                "sub/lib.rs",
                "fn gamma_function() { println!(\"gamma sub\"); }",
            )
            .await
            .expect("child index_file");
        registry.register(IndexHandle::bare(
            child_id.clone(),
            Arc::new(RwLock::new(child_indexer)),
            child_root,
        ));

        let state = Arc::new(SearchAppState::new(registry));

        let Json(value) = global_search_handler(
            State(state),
            Json(GlobalSearchRequest {
                query: "gamma_function".into(),
                top_k: 10,
                full_content: false,
                indexes: None,
                routing: None,
                routing_n: None,
                routing_threshold: None,
            }),
        )
        .await
        .expect("handler ok");

        // Must include the dedup count field.
        assert!(
            value["hierarchy_dedup_count"].is_number(),
            "hierarchy_dedup_count must be present: {value:?}",
        );

        // Both indexes should be searched.
        let searched = value["indexes_searched"].as_array().unwrap();
        assert_eq!(
            searched.len(),
            2,
            "both parent and child should be searched"
        );

        // Results should exist (BM25 finds the keyword).
        let results = value["results"].as_array().unwrap();
        assert!(!results.is_empty(), "expected at least one result");

        // The sub-index has a boost of 1.5 applied to its lane weight, so its
        // results should rank first when querying for a term both indexes have.
        // Verify that at least one result comes from "boost-child".
        let has_child_result = results
            .iter()
            .any(|r| r["index_id"].as_str() == Some("boost-child"));
        assert!(
            has_child_result,
            "sub-index (boost-child) must contribute results to the fan-out"
        );
    }

    // ── Issue #750: unknown index id returns 404 with JSON body ──────────

    /// `POST /indexes/{id}/search` with an unknown id must return HTTP 404
    /// with a well-formed JSON body `{"error":"unknown index","index_id":"…"}`.
    ///
    /// Why (issue #750): previously `.ok_or(StatusCode::NOT_FOUND)` produced a
    /// bare 404 with no body, causing clients to fail with
    /// `error decoding response body` instead of a clear "index not found".
    #[tokio::test]
    async fn search_unknown_index_returns_404_json() {
        use crate::core::registry::IndexRegistry;
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let err = search_handler(
            State(state),
            Path("no-such-index".to_string()),
            Json(crate::core::indexer::SearchQuery {
                text: "fn main".to_string(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                branch_files: None,
                branch_boost: 1.5,
                branch: None,
                stage: None,
                mode: crate::core::indexer::SearchMode::Code,
                exclude_archived: false,
                refine_query: None,
            }),
        )
        .await
        .expect_err("unknown index must 404");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(err.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unknown index");
        assert_eq!(json["index_id"], "no-such-index");
    }

    /// `GET /indexes/{id}/status` with an unknown id must return HTTP 404
    /// with a well-formed JSON body `{"error":"unknown index","index_id":"…"}`.
    ///
    /// Why (issue #750): same root cause as search — bare 404, no JSON body.
    #[tokio::test]
    async fn status_unknown_index_returns_404_json() {
        use crate::core::registry::IndexRegistry;
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let err = index_status_handler(State(state), Path("no-such-index".to_string()))
            .await
            .expect_err("unknown index must 404");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(err.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unknown index");
        assert_eq!(json["index_id"], "no-such-index");
    }

    /// `POST /indexes/{id}/reindex/stream` with an unknown id must return
    /// HTTP 404 with a well-formed JSON body.
    ///
    /// Why (issue #750): reindex stream had the same bare-404 gap.
    #[tokio::test]
    async fn reindex_stream_unknown_index_returns_404_json() {
        use crate::core::registry::IndexRegistry;
        let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
        let err = reindex_stream_handler(State(state), Path("ghost-index".to_string()))
            .await
            .expect_err("unknown index must 404");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(err.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unknown index");
        assert_eq!(json["index_id"], "ghost-index");
    }
}
