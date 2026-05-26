//! Handler for `trusty-search start` — boots the HTTP daemon.

use anyhow::Result;
use colored::Colorize;
use std::sync::Arc;
use std::time::Duration;

use crate::core::registry::{IndexHandle, IndexId, IndexStages, StageState, StageStatus};
use crate::service::persistence::load_index_registry;
use crate::service::persistence_loader::build_indexer_with_persisted_state;
use crate::service::SearchAppState;

/// Inputs to [`derive_warm_boot_stages`] — every signal the pure stage
/// classifier needs in one struct so the call site can build it once and the
/// tests can drive it without filesystem fixtures.
///
/// Why (issue #135): the warm-boot path was instantiating every restored
/// handle with `stages = Pending` and never consulting the on-disk artifacts.
/// Searches against existing indexes silently dropped the semantic + graph
/// lanes because `search_capabilities` is derived from `stages`. Extracting
/// the classification into a pure function lets the call site keep its single
/// disk-inspection sweep and lets the unit tests pin every transition.
/// What: a small data carrier — no `Default` impl, no methods. Construction
/// is intentionally explicit so a future caller cannot forget to wire one of
/// the four signals.
/// Test: every `warm_boot_*` test in the colocated `mod tests` constructs one
/// of these inputs directly and asserts the resulting [`IndexStages`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct WarmBootInputs {
    /// Number of chunks the durable corpus reports (`CorpusStore::chunk_count`).
    /// Zero means the corpus is genuinely empty OR a previous reindex died
    /// mid-flight before the first batch committed.
    pub chunk_count: usize,
    /// `true` when `<data_dir>/indexes/<id>/hnsw.usearch` exists on disk AND
    /// was successfully restored into the indexer's vector store with a
    /// dimension matching the embedder. We deliberately do not re-open the
    /// file here: `build_indexer_with_persisted_state` already validates the
    /// snapshot and falls back to a fresh store on any mismatch, so this flag
    /// captures "we have a usable HNSW lane after warm-boot".
    pub hnsw_snapshot_ready: bool,
    /// Number of nodes in the symbol graph that
    /// `build_indexer_with_persisted_state` rehydrated (via redb +
    /// `load_or_rebuild_symbol_graph`). Zero means the graph is empty either
    /// because the corpus is empty or because the build never produced any
    /// callers/callees relationships (rare for non-trivial code).
    pub graph_node_count: usize,
    /// `lexical_only` flag persisted on the registry entry — when `true`,
    /// semantic + graph stages are forced to `Skipped` regardless of what is
    /// on disk. Defensive: a flapping operator could have an old HNSW snapshot
    /// from a prior non-lexical-only life lying around; the staged pipeline
    /// must not surface it.
    pub lexical_only: bool,
}

/// Pure classifier: given the four warm-boot signals, decide where each of
/// the three stages should land on the restored handle's [`IndexStages`].
///
/// Why (issue #135): see [`WarmBootInputs`]. This is the function the unit
/// tests exercise — separating it from the disk-walking caller keeps the
/// tests deterministic and the production path a thin wrapper.
/// What: applies the rules from the ticket spec, in this order:
///   1. `lexical_only == true` → semantic + graph are `Skipped`.
///   2. `chunk_count > 0` → lexical is `Ready` (BM25 + redb both restored
///      cleanly by the loader).
///   3. `chunk_count == 0` → lexical is `InProgress` (mid-reindex recovery
///      — the next reindex will resume via the hash-skip path).
///   4. `hnsw_snapshot_ready` (and not lexical-only) → semantic is `Ready`.
///   5. `graph_node_count > 0` (and not lexical-only) → graph is `Ready`.
///
/// Test: `warm_boot_*` tests in this module's `tests` submodule.
pub(crate) fn derive_warm_boot_stages(inputs: WarmBootInputs) -> IndexStages {
    // Lexical stage.
    //
    // Why: `chunk_count > 0` means the redb corpus restored cleanly AND
    // `build_indexer_with_persisted_state::restore_corpus` rebuilt BM25 as a
    // side effect of `load_chunks_from_redb`'s four-phase publish — so the
    // lexical lane is queryable as soon as we register the handle. The
    // `last_indexed` proxy lives outside this struct (the registry handle
    // does not carry a timestamp; freshness is computed from
    // `index.redb` mtime by `index_disk_and_mtime`), so we leave
    // `completed_at` as `None` and rely on the existing freshness reporting
    // for the wall-clock signal. A future ticket can plumb mtime through if
    // operators need stage-level timestamps after warm-boot.
    let lexical = if inputs.chunk_count > 0 {
        StageState {
            status: StageStatus::Ready,
            ..Default::default()
        }
    } else {
        // Mid-reindex recovery: redb is empty but the index is registered.
        // Mark lexical `InProgress` so the search handler does not advertise
        // BM25 as ready, and the next reindex (triggered manually or by the
        // file watcher) will resume via the hash-skip path in
        // `commit_parsed_batch`.
        StageState {
            status: StageStatus::InProgress,
            ..Default::default()
        }
    };

    // Semantic + graph: forced to `Skipped` for `lexical_only` indexes
    // regardless of on-disk state. Otherwise read directly from the
    // inspection signals.
    let (semantic, graph) = if inputs.lexical_only {
        (StageState::skipped(), StageState::skipped())
    } else {
        let semantic = if inputs.hnsw_snapshot_ready {
            StageState {
                status: StageStatus::Ready,
                ..Default::default()
            }
        } else {
            StageState::pending()
        };
        let graph = if inputs.graph_node_count > 0 {
            StageState {
                status: StageStatus::Ready,
                ..Default::default()
            }
        } else {
            StageState::pending()
        };
        (semantic, graph)
    };

    IndexStages {
        lexical,
        semantic,
        graph,
    }
}

/// Restore every index recorded in `indexes.toml` by re-registering it on the
/// in-memory registry. For each entry we attempt to load the persisted HNSW
/// snapshot and chunk corpus from disk so the index comes back warm (no
/// re-indexing required).
///
/// Why (issue #85): before this hook, the daemon had no way to remember
/// which projects were registered — every restart required the user to run
/// `trusty-search index <path>` again. Now the registry is durable and
/// HNSW + chunks are restored automatically.
/// What: iterates registry entries, skips any that the in-memory registry
/// already has (idempotent — `create_index` may have raced ahead), then
/// constructs an `IndexHandle` via the shared `build_indexer_with_persisted_state`
/// helper.
/// Test: integration test in `tests/integration_tests.rs` that writes a
/// registry file, calls this hook, and asserts the registry list matches.
async fn restore_indexes(state: &SearchAppState, embedder: &Arc<dyn crate::core::Embedder>) {
    let entries = match load_index_registry() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("could not read indexes.toml at startup: {e}");
            return;
        }
    };
    if entries.is_empty() {
        return;
    }
    tracing::info!(
        "warm-boot: restoring {} index registration(s) from indexes.toml",
        entries.len()
    );
    for entry in entries {
        let id = IndexId::new(entry.id.clone());
        if state.registry.get(&id).is_some() {
            // A live create_index handler beat us to it — skip.
            continue;
        }
        let mut indexer =
            build_indexer_with_persisted_state(&entry.id, entry.root_path.clone(), embedder).await;
        // Restore per-index filters and domain vocabulary from indexes.toml.
        // Resolve `include_paths` to absolute under `root_path` so the reindex
        // walker can prune without per-call path arithmetic. `.` and empty
        // entries collapse to "walk the whole root".
        let include_paths: Vec<std::path::PathBuf> = entry
            .include_paths
            .iter()
            .filter(|p| !p.trim().is_empty() && p.trim() != ".")
            .map(|p| entry.root_path.join(p.trim()))
            .collect();
        let extensions: Vec<String> = entry
            .extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_string())
            .filter(|e| !e.is_empty())
            .collect();
        indexer.set_domain_terms(entry.domain_terms.clone());
        // Issue #75: capture the current git HEAD SHA at registration so the
        // search response can flag staleness when the working tree advances
        // past the indexed commit. Best-effort: `None` outside a git repo.
        let indexed_head_sha = crate::core::git::head_sha(&entry.root_path);
        let lexical_only = entry.lexical_only;
        // Issue #135: inspect the on-disk artifacts that
        // `build_indexer_with_persisted_state` just restored and derive the
        // staged-pipeline state from them. Before this, every warm-booted
        // index landed with `stages = Pending` and `search_capabilities`
        // computed from that — so the search handler silently disabled the
        // vector + KG lanes on every existing index until the user ran a
        // force reindex. That broke the v0.9.0 upgrade path for everyone with
        // a populated `indexes.toml`.
        //
        // The inspection is cheap: `chunk_count` is one redb metadata read,
        // `hnsw.usearch` is a `path.exists()` filesystem call (the dim /
        // deserialise check already happened inside the loader), and the
        // symbol-graph node count is an `Arc::clone` + in-memory read.
        let chunk_count = indexer
            .corpus_store()
            .and_then(|c| c.chunk_count().ok())
            .unwrap_or(0);
        let hnsw_snapshot_ready = crate::service::persistence::hnsw_path(&entry.id)
            .map(|p| crate::service::persistence::has_persisted_hnsw(&p))
            .unwrap_or(false);
        let graph_node_count = indexer.snapshot_symbol_graph().await.node_count();
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count,
            hnsw_snapshot_ready,
            graph_node_count,
            lexical_only,
        });
        tracing::info!(
            "warm-boot: index '{}' restored — chunks={} hnsw_snapshot={} graph_nodes={} \
             lexical_only={} → stages(lexical={:?}, semantic={:?}, graph={:?})",
            entry.id,
            chunk_count,
            hnsw_snapshot_ready,
            graph_node_count,
            lexical_only,
            stages.lexical.status,
            stages.semantic.status,
            stages.graph.status,
        );
        let handle = IndexHandle {
            id: id.clone(),
            indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
            root_path: entry.root_path,
            include_paths,
            exclude_globs: entry.exclude_globs,
            extensions,
            domain_terms: entry.domain_terms,
            include_docs: entry.include_docs,
            respect_gitignore: entry.respect_gitignore,
            path_filter: entry.path_filter,
            context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
            context_summary: Arc::new(tokio::sync::RwLock::new(None)),
            indexed_head_sha: Arc::new(tokio::sync::RwLock::new(indexed_head_sha)),
            lexical_only,
            stages: Arc::new(tokio::sync::RwLock::new(stages)),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
        };
        state.registry.register(handle);
    }
}

/// Build a shared `FastEmbedder` for every index registered during the
/// daemon's lifetime.
///
/// Why (Bug A fix): without this, `create_index_handler` constructs a BM25-only
/// `CodeIndexer` and the HNSW lane silently contributes nothing — the symptom
/// seen in the 115k-chunk benchmark where every result returned
/// `match_reason: "bm25"`.
///
/// Why (blocking init): previously a failure here returned `None` and the
/// daemon continued in BM25-only mode without any visible signal — operators
/// only noticed when every search returned `match_reason: "bm25"` and an
/// entire 17k-file repo "indexed" in 12 seconds (no ONNX work happened).
/// Now: success is logged at INFO with the embedding dimension so operators
/// can confirm the model loaded; failure is logged at ERROR and propagated
/// so the daemon exits non-zero rather than silently degrading.
///
/// Why (issue #110 Phase 1): when `TRUSTY_EMBEDDER` is set to an HTTP address
/// (`http://...`) the daemon switches to `RemoteEmbedderClient` and sends all
/// embed calls to a running `trusty-embedderd` instance. Default behaviour
/// (`local`, `in-process`, or unset) is unchanged — `InProcessEmbedderClient`
/// wraps the existing `FastEmbedder`.
///
/// Test: run `trusty-search start` with `RUST_LOG=info` — the log MUST
/// contain `embedder: in-process` or `embedder: remote http://...` before any
/// HTTP request is accepted. Force a failure (e.g. delete the model cache while
/// offline) and the daemon must exit non-zero, not start in BM25 mode.
async fn build_embedder() -> Result<std::sync::Arc<dyn crate::core::Embedder>> {
    // Issue #110 Phase 1: check TRUSTY_EMBEDDER for a remote HTTP address.
    // A value starting with "http://" or "https://" routes to the remote path.
    // Unset, "local", or "in-process" all fall through to the default path.
    let trusty_embedder_env = std::env::var("TRUSTY_EMBEDDER").unwrap_or_default();
    if trusty_embedder_env.starts_with("http://") || trusty_embedder_env.starts_with("https://") {
        tracing::info!("embedder: remote {}", trusty_embedder_env);
        let client =
            trusty_common::embedder_client::RemoteEmbedderClient::new(trusty_embedder_env.clone());
        return Ok(std::sync::Arc::new(RemoteEmbedderAdapter { client }));
    }

    // Issue #41 phase 4: when the candle feature is compiled in AND the
    // operator opts in via `TRUSTY_EMBEDDER=candle`, build a `CandleEmbedder`
    // instead of the ONNX/fastembed path. This bypasses the CoreML jetsam
    // risk on Apple Silicon while keeping the embedding contract identical
    // (384-dim L2-normalised f32).
    #[cfg(feature = "candle")]
    {
        if trusty_embedder_env == "candle" {
            let candle =
                tokio::task::spawn_blocking(crate::service::candle_embedder::CandleEmbedder::new)
                    .await
                    .map_err(|e| anyhow::anyhow!("candle embedder init task panicked: {e}"))??;
            let dim = candle.dimension();
            tracing::info!("embedder initialized: model=all-MiniLM-L6-v2 dim={dim} backend=candle");
            return Ok(std::sync::Arc::new(candle));
        }
    }

    tracing::info!("embedder: in-process");
    let embedder = crate::core::FastEmbedder::new().await.map_err(|e| {
        tracing::error!("FastEmbedder init failed: {e:#}");
        anyhow::anyhow!("FastEmbedder init failed: {e}")
    })?;
    let dim = <crate::core::FastEmbedder as crate::core::Embedder>::dimension(&embedder);
    let provider = embedder.provider();
    let metal_hint = match provider {
        trusty_common::embedder::ExecutionProvider::CoreML => " (Metal GPU + ANE + CPU)",
        trusty_common::embedder::ExecutionProvider::CoreMLAne => " (Neural Engine + CPU)",
        trusty_common::embedder::ExecutionProvider::Cuda => " (CUDA GPU)",
        trusty_common::embedder::ExecutionProvider::Cpu => "",
    };
    tracing::info!(
        "embedder initialized: model=AllMiniLML6V2(Q) dim={dim} provider={provider}{metal_hint}"
    );
    tune_batch_size_for_provider(provider);
    Ok(std::sync::Arc::new(embedder))
}

/// Adapter that implements trusty-search's `Embedder` trait by delegating to
/// a `RemoteEmbedderClient` (issue #110 Phase 1).
///
/// Why: trusty-search's internal `Embedder` trait uses `&[&str]` slices;
/// `EmbedderClient` uses `Vec<String>`. This adapter bridges the two without
/// modifying either side.
///
/// What: holds an `Arc<RemoteEmbedderClient>` and impls the local `Embedder`
/// facade that `CodeIndexer` and `EmbedPool` hold behind `Arc<dyn Embedder>`.
///
/// Test: exercised end-to-end when `TRUSTY_EMBEDDER=http://...` is set at
/// daemon startup; the `bit_identical` integration test validates correctness.
struct RemoteEmbedderAdapter {
    client: trusty_common::embedder_client::RemoteEmbedderClient,
}

#[async_trait::async_trait]
impl crate::core::Embedder for RemoteEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let mut v = self
            .client
            .embed_batch(vec![text.to_string()])
            .await
            .map_err(|e| anyhow::anyhow!("remote embed failed: {e}"))?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("remote embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        self.client
            .embed_batch(owned)
            .await
            .map_err(|e| anyhow::anyhow!("remote embed_batch failed: {e}"))
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// When the resolved execution provider is a GPU, retune `TRUSTY_MAX_BATCH_SIZE`
/// upward so ONNX dispatches use the GPU efficiently.
///
/// Why (issue #113): the CPU batch-size formula (≈55 MB transient ORT arena
/// per batch slot, clamped to `[32, 512]`) is sized to keep the *CPU* ORT
/// path under the soft RSS cap. On a CUDA GPU the arena lives in device
/// memory and the per-slot transient is much smaller, so the CPU-tuned
/// default (e.g. 128 on Medium tier) starves the GPU — most wall-clock is
/// spent on host↔device round-trips instead of compute. Bumping the default
/// to 512 cuts host↔device transitions ~4× and is what turns a 5-hour
/// reindex into a ~30-minute one on a Tesla T4 (40 k files, INT8 model).
/// What: if a GPU EP is active AND the operator did NOT opt out by setting
/// `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1`, retune `TRUSTY_MAX_BATCH_SIZE` to 512.
/// Test: on a CUDA-enabled binary the startup log shows
/// `gpu_batch_tuning: TRUSTY_MAX_BATCH_SIZE=512 (was N)`; running with
/// `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 TRUSTY_MAX_BATCH_SIZE=256 trusty-search start`
/// keeps 256.
fn tune_batch_size_for_provider(provider: trusty_common::embedder::ExecutionProvider) {
    const GPU_BATCH_DEFAULT: usize = 512;

    // CoreML is intentionally excluded from the GPU batch-size bump.
    //
    // Why: unlike CUDA (whose ORT arena lives in device memory), CoreML on
    // Apple Silicon pre-allocates GPU/ANE buffers in the *unified* memory
    // pool and those buffers stack between calls. Bumping TRUSTY_MAX_BATCH_SIZE
    // to 512 on CoreML reliably inflates process RSS by ~70 GB in seconds
    // and triggers macOS jetsam SIGKILL. The reindex pipeline now uses
    // `TRUSTY_COREML_BATCH_SIZE` (default 32) when CoreML is active —
    // see `core::indexer::ingest::embed_chunks_in_batches`. Leaving
    // `TRUSTY_MAX_BATCH_SIZE` at its tier default is the safe answer.
    if matches!(
        provider,
        trusty_common::embedder::ExecutionProvider::CoreML
            | trusty_common::embedder::ExecutionProvider::CoreMLAne
    ) {
        let coreml_bs = crate::core::resolve_coreml_batch_size();
        tracing::info!(
            "gpu_batch_tuning: provider={provider} → using TRUSTY_COREML_BATCH_SIZE={coreml_bs} for \
             indexing batches (CoreML EP allocates per-batch buffers in the unified-memory pool)"
        );
        return;
    }

    let is_gpu = matches!(provider, trusty_common::embedder::ExecutionProvider::Cuda);
    if !is_gpu {
        return;
    }

    if std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        tracing::info!(
            "gpu_batch_tuning: TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 set, leaving batch size unchanged"
        );
        return;
    }

    let current = std::env::var("TRUSTY_MAX_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(128);
    if current >= GPU_BATCH_DEFAULT {
        return;
    }

    // SAFETY: invoked on the main thread before any indexing worker has
    // started. Same invariant `MemoryPolicy::apply_to_env` relies on.
    unsafe {
        std::env::set_var("TRUSTY_MAX_BATCH_SIZE", GPU_BATCH_DEFAULT.to_string());
    }
    tracing::info!(
        "gpu_batch_tuning: provider={provider} → TRUSTY_MAX_BATCH_SIZE={GPU_BATCH_DEFAULT} (was {current}); \
         set TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 to keep your value"
    );
}

/// Why: extracted from `main()`. The boot sequence is intricate (lockfile probe,
/// embedder, app state) and benefits from being its own unit. Facts storage
/// moved to trusty-analyzer (issue #40).
/// What: probes the lockfile fast-path, then constructs `SearchAppState` and
/// hands off to `run_daemon`. Maps `DaemonError::AlreadyRunning` to a friendly
/// exit-1 message.
/// Test: run twice in a row — the second invocation must exit 1 with the
/// "another daemon is already running" message.
pub async fn handle_start(port: u16, foreground: bool, device: &str, verbose: bool) -> Result<()> {
    // Background self-spawn path: when invoked without `--foreground`, fork a
    // detached copy of ourselves with `--foreground` and return immediately.
    //
    // Why: prior to this, `run_daemon().await` blocked the current process
    // forever and held the controlling terminal. Running `trusty-search start`
    // from inside a tmux pane (e.g. via `make patch`) meant a SIGHUP on pane
    // close would kill the daemon and tear down the whole tmux session. By
    // detaching stdio to `/dev/null` and returning, we let the caller (shell,
    // make, tmux) continue without owning the daemon's lifetime.
    //
    // launchd/systemd/Docker users (and `trusty-search service`) always pass
    // `--foreground` and stay on the inline path so the supervisor can manage
    // the process lifecycle.
    if !foreground {
        // Validate `--device` here too so a typo doesn't silently spawn a
        // child that will then immediately exit non-zero.
        match device {
            "auto" | "cpu" | "gpu" => {}
            other => {
                anyhow::bail!("invalid --device value '{other}'; expected one of: auto, cpu, gpu")
            }
        }
        let exe = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("could not resolve current_exe: {e}"))?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("start")
            .arg("--foreground")
            .arg("--port")
            .arg(port.to_string())
            .arg("--device")
            .arg(device)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not spawn detached daemon: {e}"))?;
        let pid = child.id();
        eprintln!(
            "{} Daemon starting in background (pid {pid}). Run `trusty-search status` to verify readiness.",
            "✓".green()
        );
        return Ok(());
    }

    // Issue #35: the foreground daemon owns tracing init so it can wire the
    // in-memory `LogBuffer` that backs `GET /logs/tail`. `main.rs` skips its
    // usual `init_tracing` call for the `start` command precisely so this
    // `try_init` wins the race and the buffer captures every log line.
    let log_buffer = trusty_common::init_tracing_with_buffer(
        if verbose { 2 } else { 0 },
        trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
    );

    // Translate the `--device` CLI flag into the `TRUSTY_DEVICE` env var that
    // `trusty-embedder` reads at session-init time. An explicit shell-level
    // `TRUSTY_DEVICE` always wins so launchd/systemd unit files can set the
    // policy in one place; the CLI flag only fills in when no env value is
    // already present. `auto` is a no-op (existing behaviour: try GPU EPs in
    // priority order, fall back to CPU on failure).
    //
    // SAFETY: invoked on the main thread before tokio spawns any workers.
    // Same invariant `MemoryPolicy::apply_to_env` relies on.
    if std::env::var_os("TRUSTY_DEVICE").is_none() {
        match device {
            "cpu" | "gpu" => unsafe {
                std::env::set_var("TRUSTY_DEVICE", device);
            },
            "auto" => {}
            other => {
                anyhow::bail!("invalid --device value '{other}'; expected one of: auto, cpu, gpu")
            }
        }
    }

    // Persist any memory-limit env vars set in the current shell so that
    // launchd restarts (which run without the user's shell environment) pick
    // them up via `daemon.env`. This runs before the fast-path check so the
    // file is always refreshed when `start` is explicitly invoked.
    crate::service::save_daemon_env();

    // Source daemon.env into the current environment (env var > file > default).
    // This is primarily for the first `start` after upgrading, when the file
    // may already exist from a previous run.
    crate::service::load_daemon_env();

    // Auto-tune memory caps based on detected system RAM (issue: memory-tier
    // autosizing). Precedence: explicit env var > daemon.env (just loaded)
    // > tier default. `MemoryPolicy::detect()` writes the resolved values
    // back into the process env so existing readers (indexer, bm25,
    // symbol_graph, memguard, store) pick them up automatically.
    //
    // SAFETY: invoked before tokio spawns any indexing workers — env mutation
    // here is on the runtime's main worker thread but no other thread is
    // reading these vars yet. Same invariant `load_daemon_env` relies on.
    let policy = crate::core::MemoryPolicy::detect();

    // Hard 16 GB minimum: trusty-search is designed for developer workstations.
    // Machines with less than 16 GB cannot safely index large codebases —
    // ONNX model load, HNSW resident memory, and indexing batches will OOM
    // under realistic workloads. Exit before binding any port or loading
    // the embedder so the operator gets a clear, actionable error.
    const MIN_RAM_MB: u64 = 16 * 1024;
    if policy.total_ram_mb < MIN_RAM_MB {
        anyhow::bail!(
            "trusty-search requires at least 16 GB of RAM.\n\
             Detected: {} MB ({:.1} GB)\n\
             Indexing large codebases on machines with less memory is not supported.",
            policy.total_ram_mb,
            policy.total_ram_mb as f64 / 1024.0
        );
    }

    policy.log_summary();
    // We arrive here only when `foreground == true` (the background path
    // returned earlier via self-spawn). The flag is consumed by the spawn
    // branch above, so it is unused from this point onward.
    let _ = foreground;
    // Fast-path: bail before loading the 86 MB embedding model when
    // another daemon is already running.  The lock check is ~1 ms;
    // FastEmbedder::new() can take several seconds on first run.
    //
    // Issue #87 (Option A): if the lockfile exists and the recorded PID is
    // *alive*, print an actionable warning and exit 1 so the caller knows
    // they must stop the daemon first before installing a new binary.
    // Replacing the binary on-disk while the daemon is running causes macOS
    // 26.3+ to SIGKILL the process with "Code Signature Invalid".
    //
    // NOTE: this is intentionally exit(1), NOT exit(0).  The previous exit(0)
    // was kept for launchd's crash-loop guard; that guard is now handled by
    // the `DaemonError::AlreadyRunning` branch below (which still exits 0)
    // because launchd only calls `start` without a prior `stop`.  A direct
    // user invocation of `trusty-search start` when the daemon is already up
    // should be a visible, actionable error.
    //
    // If the PID is dead (stale lock), fall through to `run_daemon`, whose
    // `acquire_lock` removes the stale file and retries on our behalf.
    if let Some(pid) = crate::service::running_daemon_pid() {
        tracing::warn!("daemon already running (pid {pid}); refusing to start a second instance");
        anyhow::bail!(
            "Daemon already running (pid {pid}).\n\
             Stop it first with `trusty-search stop`, then re-run `trusty-search start`.\n\
             Replacing the binary while the daemon is running causes macOS to SIGKILL \
             the process (Code Signature Invalid)."
        );
    }
    // Rare race: the lock is held but the PID-aliveness check returned None
    // (lockfile may contain garbage or be mid-write by a sibling launch).
    // Fall through to `run_daemon` — its `acquire_lock` will either succeed
    // (lock now free) or return AlreadyRunning, handled below.

    // Issue #81: detect orphan daemons whose PIDs are NOT recorded in the
    // lockfile (e.g. a previous `start` whose lockfile was deleted or
    // overwritten by a launchd respawn). These orphans keep their HNSW
    // indexes resident — we observed a 73 GB RSS orphan daemon left running
    // when `stop` only knew about the lockfile PID. Reap them now so we
    // don't end up with two daemons fighting over `bind_with_auto_port`.
    let orphans = crate::commands::stop::find_daemon_pids();
    if !orphans.is_empty() {
        tracing::warn!(
            "found {} existing trusty-search daemon process(es) not tracked by lockfile: {:?} — terminating before start",
            orphans.len(),
            orphans
        );
        eprintln!(
            "{} found {} existing trusty-search daemon process(es) not tracked by lockfile — stopping them first",
            "⚠".yellow(),
            orphans.len()
        );
        #[cfg(unix)]
        for pid in &orphans {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        // Give them 3 s to exit cleanly, then SIGKILL stragglers.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            #[cfg(unix)]
            let any_alive = orphans.iter().any(|p| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(*p as i32), None).is_ok()
            });
            #[cfg(not(unix))]
            let any_alive = false;
            if !any_alive || std::time::Instant::now() >= deadline {
                break;
            }
        }
        #[cfg(unix)]
        for pid in &orphans {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid as i32), None).is_ok() {
                tracing::warn!("orphan pid {pid} ignored SIGTERM — sending SIGKILL");
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(*pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        // Clear stale lock/port files left behind by the killed orphans.
        if let Ok(lock) = crate::service::daemon_lock_path() {
            let _ = std::fs::remove_file(&lock);
        }
        if let Some(port) = super::daemon_utils::daemon_port_path() {
            let _ = std::fs::remove_file(&port);
        }
    }

    // Why (v0.3.12 fix — deferred embedder init): previously `build_embedder()`
    // was awaited before the HTTP listener bound, so the daemon's port stayed
    // closed for 15–30 s on first run while ONNX/CoreML loaded the model.
    // That blew past the 10 s readiness budget in `daemon_guard.rs` and made
    // `trusty-search index` think the daemon had failed to start. Now: we
    // construct the `SearchAppState` immediately, kick off model loading on
    // a background task, and let `run_daemon` bind the HTTP port right away.
    // Handlers that need the embedder return `503 Service Unavailable` until
    // `state.install_embedder()` flips the watch channel.
    let cfg = crate::service::load_user_config();

    // Issue #35: hand the daemon's `LogBuffer` (created above by
    // `init_tracing_with_buffer`) to the HTTP state so `GET /logs/tail`
    // serves the same lines the tracing subscriber captures. Without this
    // the endpoint would only ever see the empty default buffer.
    // Issue #41 Phase 1: install the Prometheus recorder before constructing
    // the AppState so every emit macro fired during state setup is captured.
    // Failure to install is non-fatal — log and continue without /metrics.
    let metrics_state = match crate::service::metrics::install_recorder() {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::warn!("could not install prometheus recorder: {e} (metrics disabled)");
            None
        }
    };

    let mut state =
        crate::service::SearchAppState::new(crate::core::registry::IndexRegistry::new())
            .with_local_model(cfg.local_model)
            .with_openrouter_model(cfg.openrouter_model)
            .with_openrouter_api_key(cfg.openrouter_api_key)
            .with_log_buffer(log_buffer);
    if let Some(m) = metrics_state {
        state = state.with_metrics(m);
    }

    // Spawn embedder load on a background task; the daemon's HTTP server
    // starts serving requests in parallel. On success, `install_embedder`
    // populates the slot and flips the readiness watch so the next inbound
    // request transitions out of the "initializing" branch. On failure, we
    // log loudly but leave the daemon running in BM25-only mode — operators
    // can `/health`-check `embedder: "unavailable"` and intervene. We can't
    // exit the process here without racing the HTTP server's shutdown path.
    // Why (issue #121): on Intel Xeon AVX-512 hosts, `ort-2.0.0-rc.12`'s CPU
    // session init can block the calling thread indefinitely with no error,
    // no timeout, and no panic. Running `build_embedder().await` on a normal
    // Tokio task means the blocking native code parks a Tokio worker thread
    // forever, and the surrounding `tokio::spawn` future never makes progress.
    //
    // Fix has two parts:
    //   1. Run the ORT/fastembed init on a dedicated `spawn_blocking` thread
    //      so a hung init only consumes one blocking-pool thread instead of
    //      starving the async executor.
    //   2. Race the init against `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (default
    //      60 s). If init does not complete in time, transition the embedder
    //      to an `"error"` state (visible via `/health`) so callers stop
    //      polling and `POST /indexes` fails fast instead of dangling forever.
    //
    // The dedicated thread is intentionally NOT joined on timeout — the ORT
    // call is hung in native code we cannot abort safely, so we leak the
    // thread rather than risk an unsound abort. The daemon keeps running in
    // BM25-only mode; the operator is expected to restart with a working ORT
    // configuration after seeing the error.
    let init_timeout_secs: u64 = std::env::var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    let install_state = state.clone();
    tokio::spawn(async move {
        let runtime_handle = tokio::runtime::Handle::current();
        let init_handle = tokio::task::spawn_blocking(move || {
            // `build_embedder` is async only because `FastEmbedder::new()` is;
            // the heavy lifting (ONNX session init) happens in synchronous
            // native code inside ORT, so isolating it on a blocking thread is
            // both correct and necessary.
            runtime_handle.block_on(build_embedder())
        });

        let init_result =
            tokio::time::timeout(Duration::from_secs(init_timeout_secs), init_handle).await;

        match init_result {
            Ok(Ok(Ok(embedder))) => {
                install_state.install_embedder(Arc::clone(&embedder)).await;
                // Issue #41 Phase 1: build the embedder worker pool now that
                // the model is loaded. The pool shares the same `Arc<dyn
                // Embedder>` so it does not double the model memory; what
                // it adds is priority-aware dispatch (interactive search
                // queries jump ahead of background reindex batches) and
                // a bounded queue depth that protects the daemon from
                // unbounded fan-out.
                let pool = std::sync::Arc::new(
                    crate::service::embed_pool::EmbedPool::with_autotune(Arc::clone(&embedder)),
                );
                install_state.install_embed_pool(pool).await;
                tracing::info!("embedder ready — vector lane online");
                // Issue #85: restore every index recorded in `indexes.toml`
                // now that we have a fully-wired hybrid pipeline.
                restore_indexes(&install_state, &embedder).await;
                // Schema migration: spawn a per-index background migration task
                // for every restored index. Migrations are non-blocking — the
                // daemon keeps serving queries at the pre-migration schema
                // quality until the task completes. `TRUSTY_DISABLE_MIGRATIONS=1`
                // skips all migrations (useful for debugging or one-off restores
                // where migration side-effects are unwanted).
                if std::env::var("TRUSTY_DISABLE_MIGRATIONS").as_deref() != Ok("1") {
                    let registry =
                        std::sync::Arc::new(crate::core::migration::MigrationRegistry::new());
                    for index_id in install_state.registry.list() {
                        let Some(handle) = install_state.registry.get(&index_id) else {
                            continue;
                        };
                        let reg = std::sync::Arc::clone(&registry);
                        tokio::spawn(async move {
                            if let Err(e) =
                                crate::core::migration::run_migrations(&handle, &reg).await
                            {
                                tracing::warn!(
                                    index_id = %handle.id,
                                    "schema migration failed (index kept at current schema): {e:#}"
                                );
                            }
                        });
                    }
                }
                // Issue #41 Phase 1: prime the `trusty_index_count` gauge so
                // /metrics reports the warm-boot index count before any
                // mutating request arrives.
                crate::service::metrics::set_index_count(install_state.registry.list().len());
                // Issue #40: auto-discover Claude Code / git projects under
                // the user's configured (or default) scan paths and index any
                // not yet known to the daemon. Spawned as a separate task so
                // it cannot block the embedder-ready handoff above.
                tokio::spawn(crate::commands::discover::auto_discover_and_index());
            }
            Ok(Ok(Err(e))) => {
                let msg = format!("FastEmbedder init failed: {e}");
                install_state.install_embedder_error(msg.clone()).await;
                eprintln!(
                    "{} embedder failed to initialize: {e}\n\
                     Daemon is up but running BM25-only. Check the model cache at \
                     ~/Library/Caches/trusty-search/models/ and network access.",
                    "✗".red()
                );
            }
            Ok(Err(join_err)) => {
                let msg = format!("embedder init task panicked: {join_err}");
                install_state.install_embedder_error(msg).await;
            }
            Err(_elapsed) => {
                // Timeout fired. The blocking init thread is intentionally
                // leaked — we cannot safely cancel native ORT code that is
                // already stuck. The daemon keeps serving HTTP in
                // BM25-only mode and surfaces the error via `/health`.
                tracing::error!(
                    "embedder init timed out after {init_timeout_secs}s — \
                     ONNX session did not start (try \
                     TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS=120 or check ORT \
                     compatibility, see GitHub #121)"
                );
                let msg = format!("init timed out after {init_timeout_secs}s");
                install_state.install_embedder_error(msg).await;
                eprintln!(
                    "{} embedder init timed out after {init_timeout_secs}s — \
                     see GitHub #121. Daemon is up in BM25-only mode.",
                    "✗".red()
                );
            }
        }
    });
    match crate::service::run_daemon(state, port).await {
        Ok(()) => {}
        Err(crate::service::DaemonError::AlreadyRunning(p)) => {
            // `acquire_lock` returns AlreadyRunning only after confirming the
            // recorded PID is alive (it removes stale lockfiles automatically).
            //
            // Issue #126: launchd respawns `start` every ~30 s while the
            // daemon is up (KeepAlive + SuccessfulExit=false). The previous
            // exit(0) here meant launchd treated every respawn as a clean
            // run and immediately tried again, flooding stderr.log. Exit
            // non-zero instead — `SuccessfulExit=false` only restarts on
            // exit 0, so a non-zero exit stops the respawn loop. Suppress
            // the stderr message entirely (demote to debug) so it does not
            // accumulate in the launchd log.
            tracing::debug!(
                "daemon already running (lock at {}), exiting non-zero to stop launchd respawn",
                p.display()
            );
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("daemon failed: {e}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Warm-boot stage-restoration tests (issue #135).
    //!
    //! Why: the warm-boot path in [`restore_indexes`] is async and pulls in
    //! the full daemon-init machinery (memory policy detection, embedder
    //! load, log buffer install). Driving it end-to-end from a unit test
    //! requires a tempdir override of `dirs::data_local_dir` which the
    //! current persistence layer does not expose. Instead we test the pure
    //! classifier [`derive_warm_boot_stages`] which contains every business
    //! rule the bug is about — the call site is a thin disk-inspection
    //! adapter with no branches of its own. Each test below corresponds to
    //! one bullet in the ticket spec.
    //! Test: this module — `cargo test -p trusty-search --lib warm_boot`.
    use super::*;
    use crate::core::registry::StageStatus;

    /// Helper: construct [`WarmBootInputs`] without spelling out every field
    /// in every test. Defaults represent the "empty fresh index" baseline so
    /// each test only overrides the signals it cares about.
    fn inputs() -> WarmBootInputs {
        WarmBootInputs {
            chunk_count: 0,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
        }
    }

    /// A restored index whose redb corpus reports `chunk_count > 0` has the
    /// lexical lane fully wired (`load_chunks_from_redb` rebuilt BM25 as a
    /// side effect). The classifier must flip lexical → `Ready` so the
    /// search handler advertises BM25 / literal / exact_match in
    /// `search_capabilities`.
    #[test]
    fn warm_boot_marks_lexical_ready_when_chunks_present() {
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            ..inputs()
        });
        assert_eq!(stages.lexical.status, StageStatus::Ready);
        assert!(stages.search_capabilities().contains(&"bm25"));
        assert!(stages.search_capabilities().contains(&"literal"));
        assert!(stages.search_capabilities().contains(&"exact_match"));
    }

    /// When the HNSW sidecar exists on disk and the loader successfully
    /// rehydrated it (the call site sets `hnsw_snapshot_ready = true`), the
    /// semantic stage must come back as `Ready` and `vector` must appear in
    /// `search_capabilities`.
    #[test]
    fn warm_boot_marks_semantic_ready_when_hnsw_snapshot_exists() {
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            hnsw_snapshot_ready: true,
            ..inputs()
        });
        assert_eq!(stages.semantic.status, StageStatus::Ready);
        assert!(stages.search_capabilities().contains(&"vector"));
    }

    /// When the persisted symbol graph rehydrated with a non-zero node count
    /// (`Indexer::snapshot_symbol_graph().node_count() > 0`), the graph
    /// stage must be `Ready` and `kg` must appear in `search_capabilities`.
    #[test]
    fn warm_boot_marks_graph_ready_when_symbol_graph_nonempty() {
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            hnsw_snapshot_ready: true,
            graph_node_count: 7_402,
            ..inputs()
        });
        assert_eq!(stages.graph.status, StageStatus::Ready);
        assert!(stages.search_capabilities().contains(&"kg"));
    }

    /// Missing HNSW snapshot → semantic stays `Pending`. Lexical can still
    /// be `Ready` (BM25 does not depend on the embedder), but the search
    /// handler must not advertise `vector` until a reindex regenerates the
    /// HNSW file.
    #[test]
    fn warm_boot_marks_semantic_pending_when_no_snapshot() {
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            hnsw_snapshot_ready: false,
            ..inputs()
        });
        assert_eq!(stages.lexical.status, StageStatus::Ready);
        assert_eq!(stages.semantic.status, StageStatus::Pending);
        assert!(!stages.search_capabilities().contains(&"vector"));
    }

    /// Defensive: a `lexical_only` index that happens to have a stale HNSW
    /// file from a prior non-lexical-only life (or a graph populated by an
    /// earlier reindex) must NOT surface the vector / kg lanes. The
    /// `lexical_only` flag wins regardless of on-disk state.
    #[test]
    fn warm_boot_respects_lexical_only_flag() {
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            hnsw_snapshot_ready: true,
            graph_node_count: 7_402,
            lexical_only: true,
        });
        assert_eq!(stages.lexical.status, StageStatus::Ready);
        assert_eq!(stages.semantic.status, StageStatus::Skipped);
        assert_eq!(stages.graph.status, StageStatus::Skipped);
        let caps = stages.search_capabilities();
        assert!(caps.contains(&"bm25"));
        assert!(!caps.contains(&"vector"));
        assert!(!caps.contains(&"kg"));
    }

    /// Mid-reindex recovery: the registry has the entry but the redb corpus
    /// is empty (the previous reindex died before the first batch
    /// committed). Lexical must come back as `InProgress` — not `Pending` —
    /// so the lifecycle status surfaces "walking" and the next reindex
    /// trigger can resume cleanly via the hash-skip path.
    #[test]
    fn warm_boot_marks_mid_reindex_as_in_progress() {
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 0,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
        });
        assert_eq!(stages.lexical.status, StageStatus::InProgress);
        assert_eq!(stages.lifecycle_status(), "walking");
        assert!(stages.search_capabilities().is_empty());
    }
}
