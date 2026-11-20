//! The caller variables each Linux-spawned daemon keeps (#11085).
//!
//! Why: [`super::start`] clears a spawned daemon's environment down to
//! [`super::DaemonDescriptor::BASE_ENV`] plus the descriptor's
//! `env_passthrough` (#11098). A daemon a stdio bridge auto-starts inherits
//! the bridge's environment instead, so a trusty-console started by the verbs
//! hands its bridges only what its own passthrough carried. Keeping each
//! daemon's list here lets that daemon's descriptor and the console's read the
//! same constant, so the two cannot drift apart.
//! What: [`MEMORY_DAEMON`] and [`SEARCH_DAEMON`], each derived from every
//! NON-credential variable that daemon's path reads — the daemon crate's
//! `src/` and the trusty-common code it calls. Register a new read here, never
//! a credential (`*_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`): the daemons
//! read those from the `.env.local` tiers and the credential store.
//! Test: `daemon_lists_hold_no_credential_and_no_duplicate`; each consumer's
//! descriptor tests in trusty-memory, trusty-search and trusty-console.

/// The trusty-memory daemon's passthrough (`serve --foreground`).
pub const MEMORY_DAEMON: &[&str] = &[
    // Process and network: logging, proxies and CA bundles for model
    // downloads, the session bus for the Secret Service credential store,
    // `CARGO_HOME` for the install-dir lookup, and the ORT/CUDA loader paths.
    "RUST_LOG",
    // #11085: the log-buffer filter of `tracing_init::
    // init_tracing_with_buffer_and_capture`, which this daemon installs.
    "RUST_LOG_BUFFER",
    "RUST_BACKTRACE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "CARGO_HOME",
    "ORT_DYLIB_PATH",
    // #11085: the loader path ORT's CUDA provider libraries resolve through.
    "LD_LIBRARY_PATH",
    // #11085: trusty-common, embedder (`embedder::{fast_embedder,types,chunk}`).
    "TRUSTY_EMBEDDER_MODEL",
    "TRUSTY_DEVICE",
    "FASTEMBED_CACHE_DIR",
    "FASTEMBED_CACHE_PATH",
    "HF_HOME",
    "TRUSTY_EMBED_ONNX_BATCH",
    "TRUSTY_ORT_INTRA_THREADS",
    "TRUSTY_ORT_INTER_THREADS",
    "TRUSTY_ORT_ALLOW_SPINNING",
    "TRUSTY_GPU_MEM_LIMIT_BYTES",
    "TRUSTY_GPU_MEM_LIMIT_MB",
    // #11085: trusty-common, storage, shutdown, bug capture, update check and
    // the local model probe (`bm25`, `redb_cache`, `shutdown`, `error_capture`,
    // `update`, `local_probe`).
    "TRUSTY_BM25_CORPUS_CAP",
    "TRUSTY_MEMORY_REDB_CACHE_MB",
    "TRUSTY_TERMINATION_GRACE_SECS",
    "TRUSTY_NO_BUG_CAPTURE",
    "TRUSTY_NO_UPDATE_CHECK",
    "CI",
    "OLLAMA_HOST",
    // #11085: this crate, security and tenancy (`lib.rs`, `tools::palace_ops`).
    "TRUSTY_MEMORY_MULTI_TENANT",
    "TRUSTY_SKIP_PALACE_ENFORCEMENT",
    "TRUSTY_MEMORY_RULINGS_PALACES",
    // #11085: this crate, BM25 lane and repair.
    "TRUSTY_BM25_DAEMON",
    "TRUSTY_BM25_MAX_PALACES",
    "TRUSTY_BM25_TEXT_BUDGET_MB",
    "TRUSTY_BM25_NO_BACKFILL",
    "TRUSTY_BM25_REPAIR_INTERVAL_SECS",
    // #11085: this crate, palace and session-store residency.
    "TRUSTY_MEMORY_MAX_OPEN_PALACES",
    "TRUSTY_MEMORY_MAX_OPEN_SESSION_STORES",
    "TRUSTY_MEMORY_IDLE_EVICT_SECS",
    "TRUSTY_MEMORY_STARTUP_OPEN_LIMIT",
    // #11085: this crate, timeouts and budgets (`engine::timeouts` and the
    // HNSW op budget, worker liveness).
    "TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS",
    "TRUSTY_EMBED_BATCH_TIMEOUT_SECS",
    "TRUSTY_EMBED_CHUNKED_CAP_SECS",
    "TRUSTY_OPEN_QUEUE_TIMEOUT_SECS",
    "TRUSTY_SLOW_WRITE_WARN_SECS",
    "TRUSTY_WRITE_LOCK_TIMEOUT_SECS",
    "TRUSTY_WRITE_OP_BUDGET_SECS",
    "TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS",
    "TRUSTY_WRITE_TXN_DEADLINE_SECS",
    "TRUSTY_DREAM_PERMIT_WAIT_SECS",
    "TRUSTY_HNSW_OP_BUDGET_SECS",
    "TRUSTY_WEDGE_THRESHOLD_SECS",
    // #11085: this crate, background work (dream and chunk backfill tickers).
    "TRUSTY_DREAM_DISABLED",
    "TRUSTY_DREAM_MAX_CONCURRENT",
    "TRUSTY_CHUNK_BACKFILL_DISABLED",
    "TRUSTY_CHUNK_BACKFILL_BACKOFF_SECS",
    "TRUSTY_CHUNK_BACKFILL_BUSY_SECS",
    "TRUSTY_CHUNK_BACKFILL_EMBED_SECS",
    "TRUSTY_CHUNK_BACKFILL_ERROR_BACKOFF_SECS",
    "TRUSTY_CHUNK_BACKFILL_HOST_FAILURES",
    "TRUSTY_CHUNK_BACKFILL_IDLE_SECS",
    "TRUSTY_CHUNK_BACKFILL_LOCK_WAIT_SECS",
    "TRUSTY_CHUNK_BACKFILL_MAX_CHUNKS",
    "TRUSTY_CHUNK_BACKFILL_MAX_DRAWERS",
    "TRUSTY_CHUNK_BACKFILL_MIN_AGE_SECS",
    "TRUSTY_CHUNK_BACKFILL_PROBE_SECS",
    "TRUSTY_CHUNK_BACKFILL_SLICE_MS",
    "TRUSTY_CHUNK_BACKFILL_TICK_MS",
];

/// The trusty-search daemon's passthrough (`start --foreground`), including
/// the embedder sidecars it spawns, which inherit its environment.
pub const SEARCH_DAEMON: &[&str] = &[
    // The data-dir key (trusty-search `start::isolation::DATA_DIR_ENV`); its
    // descriptor also pins it in `instance_env`.
    "TRUSTY_DATA_DIR",
    // Logging, proxies and CA bundles for model downloads, the session bus for
    // the Secret Service credential store, the install-dir lookup, and the
    // dynamic-loader paths a CUDA / load-dynamic ORT build needs.
    "RUST_LOG",
    // #11085: the log-buffer filter of `tracing_init::
    // init_tracing_with_buffer_and_capture`, which this daemon installs.
    "RUST_LOG_BUFFER",
    "RUST_BACKTRACE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "ORT_DYLIB_PATH",
    "LD_LIBRARY_PATH",
    "CUDA_VISIBLE_DEVICES",
    "container",
    // #11085: opt-outs of the boot-time `trusty_common::update::check_throttled`.
    // `update::NO_UPDATE_CHECK_ENV`; that module sits behind `update-check`.
    "TRUSTY_NO_UPDATE_CHECK",
    "CI",
    // Embedder: model, device, caches, sidecars and the python lane.
    "FASTEMBED_CACHE_DIR",
    "FASTEMBED_CACHE_PATH",
    "HF_HOME",
    "OLLAMA_HOST",
    "TRUSTY_EMBEDDER",
    "TRUSTY_EMBEDDER_MODEL",
    "TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS",
    "TRUSTY_EMBEDDER_PYTHON_EAGER",
    "TRUSTY_DEVICE",
    "TRUSTY_DEVICE_EXPLICIT",
    "TRUSTY_INDEX_DEVICE",
    "TRUSTY_COREML_BATCH_SIZE",
    "TRUSTY_COREML_COMPUTE_UNITS",
    "TRUSTY_COREML_INIT_TIMEOUT_SECS",
    "TRUSTY_COREML_TRIPWIRE_MB",
    "TRUSTY_CUDA_SIDECAR_BATCH_CAP",
    "TRUSTY_GPU_MEM_LIMIT_BYTES",
    "TRUSTY_GPU_MEM_LIMIT_MB",
    "TRUSTY_ORT_ALLOW_SPINNING",
    "TRUSTY_EMBED_CHECKPOINT_CHUNKS",
    "TRUSTY_EMBED_CHECKPOINT_SECS",
    "TRUSTY_EMBED_INFLIGHT",
    "TRUSTY_EMBED_NO_PROGRESS_SECS",
    "TRUSTY_EMBED_ONNX_BATCH",
    "TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS",
    "TRUSTY_EMBED_WORKERS",
    "TRUSTY_EMBEDDING_CACHE",
    "TRUSTY_EMBEDDERD_BIN",
    "TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS",
    "TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS",
    "TRUSTY_EMBEDDERD_MAX_RESTARTS",
    "TRUSTY_EMBEDDERD_PY_BIN",
    "TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS",
    "TRUSTY_EMBEDDERD_RECYCLE_CHECK_SECS",
    "TRUSTY_EMBEDDERD_RECYCLE_DRAIN_SECS",
    "TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB",
    "TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB",
    "TRUSTY_EMBEDDERD_RECYCLE_MAX_AGE_SECS",
    "TRUSTY_EMBEDDERD_RECYCLE_MAX_CALLS",
    "TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS",
    "TRUSTY_EMBEDDERD_STARTUP_LOAD_SCALE_MAX",
    "TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS",
    "TRUSTY_EMBEDDERD_WEDGE_RESET_SECS",
    "TRUSTY_PY_BOOTSTRAP_RETRIES",
    "TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS",
    // Read by the python sidecar's own code, which inherits this environment.
    "TRUSTY_PY_EMBED_FP16",
    "TRUSTY_PY_EMBED_BATCH_SIZE",
    "TRUSTY_UV_BIN",
    "TRUSTY_UV_FETCH",
    // What the daemon indexes and how: discovery, KG, quantization, budgets.
    "TRUSTY_NO_AUTO_DISCOVER",
    "TRUSTY_NO_KG",
    "TRUSTY_VECTOR_QUANT",
    "TRUSTY_DISABLE_MIGRATIONS",
    "TRUSTY_DISABLE_WATCHER",
    "TRUSTY_NO_BOOT_RECONCILE",
    "TRUSTY_NO_BUG_CAPTURE",
    "TRUSTY_MPM_ROOT",
    "TRUSTY_DEFERRED_WINDOW_SPLIT",
    "TRUSTY_MAX_BATCH_SIZE",
    "TRUSTY_MAX_BATCH_SIZE_EXPLICIT",
    "TRUSTY_MAX_CHUNKS",
    "TRUSTY_MAX_CONCURRENT_REQUESTS",
    "TRUSTY_MAX_INDEX_BYTES",
    "TRUSTY_MAX_INDEX_FILES",
    "TRUSTY_MAX_KG_NODES",
    "TRUSTY_MAX_RESIDENT_INDEXES",
    "TRUSTY_BM25_CORPUS_CAP",
    "TRUSTY_REDB_CACHE_MB",
    "TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS",
    "TRUSTY_HNSW_MMAP_SERVE",
    "TRUSTY_HNSW_REVIEW_IDLE",
    // Memory limits and pressure.
    "TRUSTY_MEMORY_LIMIT_MB",
    "TRUSTY_INDEX_MEMORY_LIMIT_MB",
    "TRUSTY_MEMORY_ENFORCE_MEASURE",
    "TRUSTY_MEMORY_ENFORCE_SECS",
    "TRUSTY_MEMORY_HIGH_WATER_PCT",
    "TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS",
    "TRUSTY_MEMORY_RESTART_ON_LIMIT",
    "TRUSTY_SKIP_RAM_CHECK",
    // Timeouts, queues and background cadences.
    "TRUSTY_AMBIGUOUS_ROOT_GRACE_SECS",
    "TRUSTY_CHUNKS_IDLE_EVICT_SECS",
    "TRUSTY_CORPUS_OPEN_TIMEOUT_SECS",
    "TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS",
    "TRUSTY_ORPHAN_REAP_SECS",
    "TRUSTY_QUERY_EMBED_DEADLINE_MS",
    "TRUSTY_QUERY_TIMEOUT_SECS",
    "TRUSTY_QUEUE_DEPTH",
    "TRUSTY_QUEUE_TIMEOUT_SECS",
    "TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS",
    "TRUSTY_REHYDRATE_WAIT_MS",
    "TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS",
    "TRUSTY_REINDEX_COOLDOWN_SECS",
    "TRUSTY_REINDEX_MAX_FAILURES",
    "TRUSTY_REINDEX_QUARANTINE_MAX_SECS",
    "TRUSTY_REINDEX_RESUME",
    "TRUSTY_RESIDENCY_SWEEP_SECS",
    "TRUSTY_SEARCH_FANOUT_CONCURRENCY",
    "TRUSTY_SEARCH_FANOUT_EMBED_TIMEOUT_MS",
    "TRUSTY_SEARCH_FANOUT_INDEX_DEADLINE_MS",
    "TRUSTY_SEARCH_STANDBY_RETRY_MS",
    "TRUSTY_SEARCH_STANDBY_STALE_GRACE_MS",
    "TRUSTY_WARM_ALL_CONCURRENCY",
    "TRUSTY_WARM_ALL_INDEX_TIMEOUT_SECS",
    "TRUSTY_WARM_ALL_MAX_RSS_MB",
    "TRUSTY_WARM_ALL_WINDOW_SECS",
    "TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS",
    "TRUSTY_WARMBOOT_MAX_AGE_HOURS",
    "TRUSTY_WARMBOOT_MAX_INDEXES",
    "TRUSTY_WARMBOOT_RETRY_SECS",
    "TRUSTY_WARMBOOT_SALVAGE_SECS",
    "TRUSTY_WARMBOOT_VOLUME_PROBE_SECS",
    "TRUSTY_WATCH_IDLE_SUSPEND_SECS",
    // Shutdown: the flush the stop grace has to cover.
    "TRUSTY_SHUTDOWN_FLUSH_CONCURRENCY",
    "TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS",
    crate::shutdown::TERMINATION_GRACE_ENV,
];

#[cfg(test)]
mod tests {
    use super::{MEMORY_DAEMON, SEARCH_DAEMON};

    // #11085: a credential name here would reach every daemon a console
    // bridge starts; a duplicate means a list was merged by hand.
    #[test]
    fn daemon_lists_hold_no_credential_and_no_duplicate() {
        for list in [MEMORY_DAEMON, SEARCH_DAEMON] {
            for (i, name) in list.iter().enumerate() {
                let credential = name.to_ascii_uppercase().split('_').any(|segment| {
                    ["KEY", "APIKEY", "TOKEN", "SECRET", "PASSWORD", "PASS"].contains(&segment)
                });
                assert!(!credential, "{name} looks like a credential");
                assert!(!list[..i].contains(name), "{name} is listed twice");
            }
        }
        assert!(MEMORY_DAEMON.contains(&"TRUSTY_MEMORY_MULTI_TENANT"));
        assert!(SEARCH_DAEMON.contains(&"TRUSTY_EMBEDDER"));
        // #11085: both daemons install trusty-common's buffered tracing.
        assert!(MEMORY_DAEMON.contains(&"RUST_LOG_BUFFER"));
        assert!(SEARCH_DAEMON.contains(&"RUST_LOG_BUFFER"));
    }
}
