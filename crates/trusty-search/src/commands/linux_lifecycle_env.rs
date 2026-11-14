//! The caller variables a Linux-spawned trusty-search daemon keeps (#11085).
//!
//! Why: `trusty_common::linux_lifecycle::start` clears the spawned daemon's
//! environment down to `DaemonDescriptor::BASE_ENV` plus the descriptor's
//! `env_passthrough` (#11098). The self-spawn it replaces, and every bridge
//! auto-start, inherit the caller's whole environment, so any variable the
//! daemon reads that is missing here makes a daemon started by `start` or
//! `restart` behave differently from one a bridge starts.
//! What: [`ENV_PASSTHROUGH`], derived from every `env::var` / `var_os` read in
//! the daemon (this crate's `src/`, the trusty-common code it calls at runtime,
//! and the embedder sidecars it spawns, which inherit its environment), minus
//! the names in [`EXCLUDED`]. Credential-shaped names are never listed.
//! Test: `env_passthrough_carries_every_daemon_setting_and_no_credential`,
//! `env_passthrough_covers_every_variable_this_crate_reads`.

/// Names copied from the caller's environment, when set, into the daemon's.
pub(crate) const ENV_PASSTHROUGH: &[&str] = &[
    // The data-dir key; also pinned in `instance_env`.
    super::DATA_DIR_ENV,
    // Logging, proxies and CA bundles for model downloads, the session bus for
    // the Secret Service credential store, the install-dir lookup, and the
    // dynamic-loader paths a CUDA / load-dynamic ORT build needs.
    "RUST_LOG",
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
    trusty_common::shutdown::TERMINATION_GRACE_ENV,
];

/// Names the daemon code reads that are deliberately NOT passed, with why.
///
/// `env_passthrough_covers_every_variable_this_crate_reads` requires every
/// name this crate's `src/` reads to be in [`ENV_PASSTHROUGH`], in
/// `BASE_ENV`, or here.
#[cfg(test)]
pub(crate) const EXCLUDED: &[(&str, &str)] = &[
    ("OPENROUTER_API_KEY", "credential; owner decision (#11085)"),
    (
        "TRUSTY_EXIT_WITH_PARENT",
        "parent-death stamp; #11098 drops it",
    ),
    ("TRUSTY_INDEX", "CLI index selector, read by the client"),
    (
        "TRUSTY_SEARCH_SOCKET",
        "client socket override; the daemon binds --socket",
    ),
    ("TRUSTY_SEARCH_DATA_DIR", "client launchd probe only"),
    ("TRUSTY_SEARCH_NO_HTTP", "retired by #9536; not read"),
    ("TRUSTY_SEARCH_ISOLATED_CHILD", "test re-exec marker"),
    ("TRUSTY_SEARCH_TEST_ASSUME_LAUNCHD_SUPERVISED", "test seam"),
    ("TRUSTY_RSS_PROBE_CHILD", "test re-exec marker"),
    ("TRUSTY_MAX_INDEX_FILES_ABSENT_4356", "test marker"),
    ("PRINT_SNAPSHOT_10498", "test snapshot printer"),
];
