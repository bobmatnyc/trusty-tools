//! `impl SearchAppState` — builder and lifecycle methods.
//!
//! Why: The impl block alone is 323 lines; splitting it into a sibling file
//! keeps both files under the 500-line cap while maintaining a logical
//! boundary between data definition and behaviour.
//! What: Builder-style `with_*` methods, embedder lifecycle helpers
//! (`install_embedder`, `current_embedder`, `install_embedder_error`, …),
//! chat-provider resolution, and the embedderd pid-slot forwarder.
//! Test: covered by the handler-level tests in `super::tests`.
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, watch, OnceCell, RwLock};
use trusty_common::{ChatProvider, LocalModelConfig};

use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;

use super::state::{DaemonEvent, SearchAppState};

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
            warmboot_failed_indexes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            warmboot_summary: Arc::new(std::sync::Mutex::new(
                crate::service::server::state::WarmBootSummary::default(),
            )),
            prior_index_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
