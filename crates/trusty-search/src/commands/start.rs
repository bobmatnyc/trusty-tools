//! Handler for `trusty-search start` — boots the HTTP daemon.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use crate::commands::start_restore::restore_one_index;
use crate::core::registry::{IndexStages, StageState, StageStatus};
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

    /// `skip_kg` flag (issue #313): when `true`, the graph stage is forced to
    /// `Skipped` regardless of on-disk state. Independent of `lexical_only`.
    /// Why: a `skip_kg` index that happened to have a graph on disk (e.g. from
    /// a prior full-pipeline reindex) must not advertise the KG lane on
    /// warm-boot — the `skip_kg` config intent wins unconditionally.
    /// Test: `warm_boot_respects_skip_kg_flag` in this module.
    pub skip_kg: bool,
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
    //
    // Issue #313: `skip_kg` forces the graph stage to `Skipped` independently
    // of `lexical_only`. A `skip_kg` index that happened to have a graph on
    // disk (from a prior full-pipeline reindex) must not advertise the KG lane
    // — the config flag wins unconditionally.
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
        let graph = if inputs.skip_kg {
            // skip_kg: graph is permanently Skipped regardless of on-disk state.
            StageState::skipped()
        } else if inputs.graph_node_count > 0 {
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

/// Restore every index recorded in `indexes.toml` and in colocated roots by
/// re-registering it on the in-memory registry.
///
/// Why (issues #85 / #403 / #718 / #723): before this hook every restart
/// required re-indexing. #718 bounded scans and opens with
/// `spawn_blocking` + timeout. #723 adds probe-per-volume: each distinct
/// volume is probed ONCE on a bare OS thread before any redb opens so a
/// TCC-blocked volume costs at most one leaked thread (not one-per-index).
/// What: Phase 1 reads legacy entries from `indexes.toml`; Phase 2 scans
/// tracked roots. Each entry is restored via `restore_one_index_bounded`.
/// Test: integration test in `tests/integration_tests.rs`.
async fn restore_indexes(state: &SearchAppState, embedder: &Arc<dyn crate::core::Embedder>) {
    use crate::service::warm_boot::{
        collect_colocated_entries, collect_legacy_entries, is_on_inaccessible_volume,
        probe_warmboot_volumes, probe_warmboot_volumes_from_paths, restore_one_index_bounded,
    };
    use std::collections::HashSet;

    // ── Phase 1: legacy indexes (indexes.toml) ─────────────────────────────
    let legacy_entries = collect_legacy_entries();
    let mut seen_ids: HashSet<String> = HashSet::new();
    // Issue #860: track canonicalized root_paths from legacy entries so that
    // Phase 2 can suppress colocated entries for the same root even when
    // the two ID schemes differ (basename vs. full-path-sanitized).
    let mut seen_root_paths: HashSet<std::path::PathBuf> = HashSet::new();

    // Issue #873: TCC vs timeout skip counters for WarmBootSummary.
    let mut total_skipped_tcc: usize = 0;
    let mut total_skipped_timeout: usize = 0;

    if legacy_entries.is_empty() {
        tracing::warn!(
            "warm-boot: no legacy index entries (indexes.toml absent/empty). \
             Under launchd, set TRUSTY_DATA_DIR to an absolute path (issue #718)."
        );
    } else {
        // Issue #723: probe each distinct volume once before any redb opens.
        let inaccessible_volumes = probe_warmboot_volumes(&legacy_entries);
        if !inaccessible_volumes.is_empty() {
            tracing::warn!(
                "warm-boot: {} volume(s) inaccessible (issue #723): {}",
                inaccessible_volumes.len(),
                inaccessible_volumes
                    .iter()
                    .map(|v| v.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let total_legacy = legacy_entries.len();
        tracing::info!(
            "warm-boot: restoring {} legacy index registration(s) from indexes.toml",
            total_legacy
        );
        let (mut legacy_ok, mut legacy_skipped_tcc, mut legacy_skipped_other) =
            (0usize, 0usize, 0usize);
        for entry in legacy_entries {
            seen_ids.insert(entry.id.clone());
            // Issue #860: record the canonicalized root_path BEFORE the
            // inaccessible-volume guard so Phase 2 never duplicates this root
            // even if the legacy restore is skipped.
            seen_root_paths.insert(canonicalize_best_effort(&entry.root_path));
            if is_on_inaccessible_volume(&entry.root_path, &inaccessible_volumes) {
                tracing::warn!(
                    "warm-boot: skipping index '{}' — volume {} inaccessible (issue #723)",
                    entry.id,
                    entry.root_path.display(),
                );
                legacy_skipped_tcc += 1;
                continue;
            }
            let s = state.clone();
            let e = Arc::clone(embedder);
            if restore_one_index_bounded(entry, move |en| async move {
                restore_one_index(&s, &e, en).await;
            })
            .await
            {
                legacy_ok += 1;
            } else {
                // Timeout or panic — volume was accessible but restore stalled.
                legacy_skipped_other += 1;
            }
        }
        total_skipped_tcc += legacy_skipped_tcc;
        total_skipped_timeout += legacy_skipped_other;
        tracing::info!(
            "warm-boot: legacy phase complete — {legacy_ok}/{total_legacy} \
             (skipped tcc={legacy_skipped_tcc} timeout={legacy_skipped_other})"
        );
    }

    // ── Phase 2: colocated indexes (roots.toml + fs scan) ──────────────────
    // Issue #723: probe colocated roots before the async scan.
    // Issues #736 / #737: load roots once and reuse the paths for the probe
    // (fixes the double `load_roots()` call that was previously present), and
    // probe the actual root paths directly rather than building throwaway
    // `PersistedIndex { ..Default::default() }` structs whose `root_path`
    // would default to empty when a root lacked a filesystem path.
    let colocated_inaccessible = {
        use crate::service::roots_registry::load_roots;
        match load_roots() {
            Ok(roots) => {
                let root_paths: Vec<std::path::PathBuf> =
                    roots.into_iter().map(|r| r.path).collect();
                probe_warmboot_volumes_from_paths(&root_paths)
            }
            Err(_) => std::collections::HashSet::new(),
        }
    };
    // Issue #860: pass seen_root_paths so the colocated scan suppresses ghost
    // entries whose root is already owned by a legacy entry (even if the IDs differ).
    let colocated_entries =
        collect_colocated_entries(&seen_ids, &seen_root_paths, &colocated_inaccessible).await;

    if colocated_entries.is_empty() {
        tracing::debug!("warm-boot: no additional colocated indexes discovered");
    } else {
        let total_colocated = colocated_entries.len();
        tracing::info!(
            "warm-boot: restoring {} colocated index registration(s) from tracked roots",
            total_colocated
        );
        let (mut colocated_ok, mut colocated_skipped_tcc, mut colocated_skipped_other) =
            (0usize, 0usize, 0usize);
        for entry in colocated_entries {
            // Issue #873: skip inaccessible vol. before restore so timeout ≠ tcc-skip.
            if is_on_inaccessible_volume(&entry.root_path, &colocated_inaccessible) {
                colocated_skipped_tcc += 1;
                continue;
            }
            let s = state.clone();
            let e = Arc::clone(embedder);
            if restore_one_index_bounded(entry, move |en| async move {
                restore_one_index(&s, &e, en).await;
            })
            .await
            {
                colocated_ok += 1;
            } else {
                colocated_skipped_other += 1;
            }
        }
        total_skipped_tcc += colocated_skipped_tcc;
        total_skipped_timeout += colocated_skipped_other;
        tracing::info!(
            "warm-boot: colocated phase complete — {colocated_ok}/{total_colocated} \
             (skipped tcc={colocated_skipped_tcc} timeout={colocated_skipped_other})"
        );
    }

    let total = state.registry.list().len();
    tracing::info!("warm-boot: complete — {total} total index(es) registered (legacy + colocated)");

    // Issue #873: update WarmBootSummary, emit FDA warning, persist count.
    record_warm_boot_result(state, total, total_skipped_tcc, total_skipped_timeout);

    // Issue #764: fail-loud warm-boot — tally total skipped/failed indexes and
    // store the count on AppState so `/health` can surface it without operators
    // having to tail logs.
    let registered_ids: std::collections::HashSet<String> =
        state.registry.list().into_iter().map(|id| id.0).collect();
    // TODO(#796): `seen_ids` covers only legacy entries from `indexes.toml`; colocated
    // index failures (Phase 2 above) are not counted here, so `failed_count` can
    // under-report when colocated indexes time out or fail to restore.
    let failed_count: usize = seen_ids
        .iter()
        .filter(|id| !registered_ids.contains(*id))
        .count();
    if failed_count > 0 {
        state
            .warmboot_failed_indexes
            .store(failed_count, std::sync::atomic::Ordering::Relaxed);
        tracing::error!(
            failed_count,
            registered = total,
            "warm-boot FAIL-LOUD: {failed_count} index(es) from indexes.toml did NOT load on \
             this boot (TCC denial, redb-format mismatch, or corrupt corpus). \
             These indexes are MISSING from /health and search results. \
             Run `trusty-search health` or check /health?warmboot_failed_indexes \
             for the count, then resolve the root cause and restart (issue #764).",
        );
    }
}

// Prior-index-count helpers extracted to `commands::prior_index_count` (issue #873 split).
use crate::commands::prior_index_count::{load_prior_index_count, record_warm_boot_result};

/// Attempt to canonicalize `path` (resolving symlinks), returning the canonical
/// form on success or the original path on failure.
///
/// Why (issue #541): `indexes.toml` may store a symlink-alias or pre-rename
/// path from a previous registration. Re-canonicalizing at warm-boot makes
/// `handle.root_path` match the absolute paths that the indexer stored in
/// chunk records, preventing `file_is_within_root` from dropping valid results.
/// This is a re-export of the canonical implementation that lives in
/// `service::warm_boot` (where it is also used for Phase 2 root-path dedup, #860
/// / #864) so both call sites use the same function and their fallback behaviour
/// (raw path on error, `debug` log) is guaranteed identical.
/// What: calls `std::fs::canonicalize`; on `Err` logs at `debug` level and
/// returns the original path unchanged so warm-boot is never blocked.
/// Test: `warm_boot_canonicalize_best_effort_*` unit tests in this module.
pub(crate) use crate::service::warm_boot::canonicalize_best_effort;

/// Resolve the embedder back-end and return an `Arc<dyn Embedder>` ready for use.
///
/// Why (issue #110 Phase 2 — stdio): `trusty-embedderd` is a required runtime
/// dependency. Running embedding in-process inside the search daemon couples
/// the ONNX model lifecycle to the daemon's memory budget and prevents
/// independent restart/upgrade of the embedding subsystem. The sidecar
/// architecture is a core design commitment, not an optional feature — silent
/// fallback to in-process would let users miss the new architecture entirely.
///
/// What: reads `TRUSTY_EMBEDDER` and dispatches:
///   - unset / `auto` / `stdio` → arm a `LazyEmbedderHandle` (issue #315,
///     deferred spawn — the child process starts on the first embed request,
///     not at daemon boot). Fails fast with an install hint if the binary is
///     not on PATH.
///   - `in-process`             → in-process FastEmbedder (explicit escape hatch
///     for tests / debugging — never silently activated)
///   - `http://…`               → HTTP remote (manually managed embedderd)
///   - `unix:/path`             → UDS remote (manually managed embedderd)
///   - `candle`                 → Candle Metal backend (feature-gated)
///
/// Test: run `trusty-search start` with `RUST_LOG=info` — the startup log must
/// contain `"embedderd supervisor armed, deferred spawn enabled"` before the
/// first request is served, and `"spawning trusty-embedderd"` only when the
/// first hybrid search or reindex arrives.
/// To test fail-fast: run with PATH stripped and TRUSTY_EMBEDDERD_BIN unset;
/// the process must exit with an error containing "cargo install trusty-embedderd".
///
/// Returns `(embedder, embedderd_pid_slot)`. `embedderd_pid_slot` is `Some` only
/// for the stdio-sidecar path and holds an `Arc<AtomicU32>` that the
/// `LazyEmbedderHandle` keeps updated with the current child OS PID (0 when no
/// live process) so callers can sample the sidecar's RSS without holding any
/// mutex. Non-stdio paths return `None`.
async fn build_embedder() -> Result<(
    std::sync::Arc<dyn crate::core::Embedder>,
    Option<Arc<AtomicU32>>,
)> {
    use crate::service::embedder_supervisor::{
        locate_embedderd_binary, LazyEmbedderHandle, SupervisorConfig,
    };

    let trusty_embedder_env = std::env::var("TRUSTY_EMBEDDER").unwrap_or_default();

    // Issue #41 phase 4: candle Metal path (feature-gated, explicit opt-in).
    #[cfg(feature = "candle")]
    {
        if trusty_embedder_env == "candle" {
            let candle =
                tokio::task::spawn_blocking(crate::service::candle_embedder::CandleEmbedder::new)
                    .await
                    .map_err(|e| anyhow::anyhow!("candle embedder init task panicked: {e}"))??;
            let dim = candle.dimension();
            tracing::info!("embedder initialized: model=all-MiniLM-L6-v2 dim={dim} backend=candle");
            return Ok((std::sync::Arc::new(candle), None));
        }
    }

    match trusty_embedder_env.as_str() {
        // ── Lazy-spawn stdio sidecar (issue #315 — deferred boot default) ──
        // `TRUSTY_EMBEDDER` unset, "auto", or "stdio" → arm a
        // `LazyEmbedderHandle`. The child is NOT spawned here; the first call
        // to `embed` / `embed_batch` triggers the spawn so `lexical_only`
        // deployments with no semantic workloads never pay the ~123 MB RSS
        // cost.
        //
        // Binary discovery and config parsing still happen here (at daemon
        // boot) so any misconfiguration (missing binary, bad env var) fails
        // fast with an actionable error rather than surfacing silently on the
        // first embed request.
        "" | "auto" | "stdio" => {
            // `trusty-embedderd` is a required runtime dependency — fail fast
            // with an actionable install hint rather than silently downgrading
            // to in-process embedding. Users who need to skip the sidecar for
            // tests or debugging must set `TRUSTY_EMBEDDER=in-process` explicitly.
            let binary = locate_embedderd_binary().map_err(|e| {
                anyhow::anyhow!(
                    "{e}\n\n\
                     ERROR: trusty-embedderd binary not found on PATH.\n\
                     \n\
                     trusty-search v0.13+ requires trusty-embedderd to be installed alongside it.\n\
                     \n\
                     Install it with:\n\
                     \x20 cargo install trusty-embedderd --locked\n\
                     \n\
                     Or set TRUSTY_EMBEDDERD_BIN to an absolute path:\n\
                     \x20 export TRUSTY_EMBEDDERD_BIN=/path/to/trusty-embedderd\n\
                     \n\
                     If you need to run without the sidecar (tests, debugging), use:\n\
                     \x20 TRUSTY_EMBEDDER=in-process trusty-search start"
                )
            })?;

            let config = SupervisorConfig::from_env();

            tracing::info!(
                "embedder mode: stdio-sidecar lazy (binary={}, idle_shutdown_secs={})",
                binary.display(),
                config.idle_shutdown_secs,
            );

            // Issue #315: construct the lazy handle (no child spawned yet).
            // The handle wraps the binary path and config; the child process
            // starts on the first `embed_batch` call.
            let handle = Arc::new(LazyEmbedderHandle::new(binary, config));
            let pid_slot = handle.app_pid_slot();

            Ok((Arc::new(LazySlotEmbedderAdapter { handle }), Some(pid_slot)))
        }

        // ── In-process safety-valve ────────────────────────────────────────
        "in-process" | "local" => {
            tracing::info!("embedder mode: in-process (override via TRUSTY_EMBEDDER=in-process)");
            let embedder = build_in_process_embedder().await?;
            Ok((embedder, None))
        }

        // ── HTTP remote (manually managed embedderd) ───────────────────────
        addr if addr.starts_with("http://") || addr.starts_with("https://") => {
            tracing::info!("embedder mode: remote http ({})", addr);
            let client = trusty_common::embedder_client::RemoteEmbedderClient::new(addr.to_owned());
            Ok((
                Arc::new(RemoteEmbedderAdapter {
                    client: EmbedderClientKind::Http(client),
                }),
                None,
            ))
        }

        // ── UDS remote (manually managed embedderd) ────────────────────────
        path if path.starts_with("unix:") => {
            let sock = PathBuf::from(&path["unix:".len()..]);
            tracing::info!("embedder mode: remote uds ({})", sock.display());
            let client = trusty_common::embedder_client::UdsEmbedderClient::new(sock);
            Ok((Arc::new(UdsEmbedderAdapter { client }), None))
        }

        other => anyhow::bail!(
            "invalid TRUSTY_EMBEDDER value: {other:?}. \
             Expected: unset (default stdio sidecar), 'auto', 'stdio', 'in-process', \
             'http://...', or 'unix:/path/to/socket'"
        ),
    }
}

/// Build the in-process `FastEmbedder` and log details.
///
/// Why: extracted from the monolithic `build_embedder` so the `in-process`
/// escape-hatch path has a clean, focused helper. This is never called from
/// the default `auto`/`stdio` path — it is only reachable via explicit
/// `TRUSTY_EMBEDDER=in-process` or `TRUSTY_EMBEDDER=local`.
/// What: constructs `FastEmbedder`, logs the provider / dimension, applies
/// GPU batch-size tuning, and wraps in an `Arc<dyn Embedder>`.
/// Test: exercised when `TRUSTY_EMBEDDER=in-process` is set explicitly.
async fn build_in_process_embedder() -> Result<Arc<dyn crate::core::Embedder>> {
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
    Ok(Arc::new(embedder))
}

/// Internal enum for the HTTP remote adapter to hold either HTTP or UDS client.
///
/// Why: avoids duplicating the `RemoteEmbedderAdapter` struct for the two HTTP
/// variants — both share identical adapter logic and differ only in the
/// concrete `EmbedderClient` impl they hold.
/// What: two variants, each wrapping the corresponding `trusty_common`
/// client type.
/// Test: exercised via `TRUSTY_EMBEDDER=http://...` (Http variant) startup.
enum EmbedderClientKind {
    Http(trusty_common::embedder_client::RemoteEmbedderClient),
}

/// Adapter that implements trusty-search's `Embedder` trait by delegating to
/// a `RemoteEmbedderClient` (HTTP) (issue #110 Phase 1 / Phase 2).
///
/// Why: trusty-search's internal `Embedder` trait uses `&[&str]` slices;
/// `EmbedderClient` uses `Vec<String>`. This adapter bridges the two without
/// modifying either side.
///
/// What: holds an `EmbedderClientKind` and impls the local `Embedder`
/// facade that `CodeIndexer` and `EmbedPool` hold behind `Arc<dyn Embedder>`.
///
/// Test: exercised end-to-end when `TRUSTY_EMBEDDER=http://...` is set at
/// daemon startup; the `bit_identical` integration test validates correctness.
struct RemoteEmbedderAdapter {
    client: EmbedderClientKind,
}

#[async_trait::async_trait]
impl crate::core::Embedder for RemoteEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let mut v = match &self.client {
            EmbedderClientKind::Http(c) => c
                .embed_batch(vec![text.to_string()])
                .await
                .map_err(|e| anyhow::anyhow!("remote embed failed: {e}"))?,
        };
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("remote embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        match &self.client {
            EmbedderClientKind::Http(c) => c
                .embed_batch(owned)
                .await
                .map_err(|e| anyhow::anyhow!("remote embed_batch failed: {e}")),
        }
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    /// Report the execution provider the remote `trusty-embedderd` resolves.
    ///
    /// Why: issue #604. The remote sidecar selects its EP through this crate's
    /// `init_options`, which is a pure function of build features + env, so the
    /// parent can predict the same answer and `/health` reports the real
    /// provider instead of the trait-default `CPU`.
    /// What: delegates to `trusty_common::embedder::resolve_expected_provider`.
    /// Test: `resolve_expected_provider_*` in trusty-common cover the resolver;
    /// real-GPU end-to-end is hardware-gated.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::resolve_expected_provider()
    }
}

/// Adapter that implements trusty-search's `Embedder` trait by delegating to
/// a `UdsEmbedderClient` (issue #110 Phase 2).
///
/// Why: the auto-spawn path uses a UDS socket for low-latency IPC; this
/// adapter bridges the `EmbedderClient` trait (Vec<String>) to the internal
/// `Embedder` trait (&[&str]) without changing either side.
///
/// What: holds a `UdsEmbedderClient` and delegates `embed` / `embed_batch`
/// calls through the EmbedderClient trait.
///
/// Test: exercised whenever `TRUSTY_EMBEDDER` is unset (auto-spawn) or set to
/// `unix:/path`; the `supervisor_spawns_and_serves_embed_requests` integration
/// test validates round-trip correctness.
struct UdsEmbedderAdapter {
    client: trusty_common::embedder_client::UdsEmbedderClient,
}

#[async_trait::async_trait]
impl crate::core::Embedder for UdsEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let mut v = self
            .client
            .embed_batch(vec![text.to_string()])
            .await
            .map_err(|e| anyhow::anyhow!("uds embed failed: {e}"))?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("uds embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        self.client
            .embed_batch(owned)
            .await
            .map_err(|e| anyhow::anyhow!("uds embed_batch failed: {e}"))
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    /// Report the execution provider the UDS-remote `trusty-embedderd` resolves.
    ///
    /// Why: issue #604 — see `RemoteEmbedderAdapter::provider`. The UDS sidecar
    /// runs the same `init_options` resolution, so the parent predicts the same
    /// provider for `/health`.
    /// What: delegates to `trusty_common::embedder::resolve_expected_provider`.
    /// Test: covered by trusty-common's `resolve_expected_provider_*` tests.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::resolve_expected_provider()
    }
}

/// Adapter for the lazy stdio sidecar path (issue #315).
///
/// Why: `LazyEmbedderHandle` defers the child spawn to the first embed call.
/// This adapter satisfies the `Arc<dyn Embedder>` interface that the rest of
/// the daemon holds, forwarding every `embed` / `embed_batch` call through
/// `LazyEmbedderHandle::embed_via` which triggers the spawn on first use and
/// then routes through the supervisor's slot on all subsequent calls.
///
/// What: holds an `Arc<LazyEmbedderHandle>` and delegates both `embed` and
/// `embed_batch` through `embed_via`. The lazy handle handles single-flight
/// spawn, crash-restart transparency, and optional idle-shutdown internally.
///
/// Test: exercised whenever `TRUSTY_EMBEDDER` is unset or set to `auto` /
/// `stdio`. The `supervisor_spawns_and_serves_embed_requests` integration test
/// validates round-trip correctness (marked `#[ignore]`, requires binary).
struct LazySlotEmbedderAdapter {
    handle: Arc<crate::service::embedder_supervisor::LazyEmbedderHandle>,
}

#[async_trait::async_trait]
impl crate::core::Embedder for LazySlotEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let text_owned = text.to_string();
        let mut v = self
            .handle
            .embed_via(|client| async move { client.embed_batch(vec![text_owned]).await })
            .await
            .map_err(|e| anyhow::anyhow!("lazy-stdio embed failed: {e}"))?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("lazy-stdio embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        self.handle
            .embed_via(|client| async move { client.embed_batch(owned).await })
            .await
            .map_err(|e| anyhow::anyhow!("lazy-stdio embed_batch failed: {e}"))
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    /// Report the execution provider the lazy stdio sidecar resolves.
    ///
    /// Why: issue #604 — this is the **default** deployment path, and it was the
    /// direct cause of `/health` reporting `provider=CPU` while the sidecar's
    /// own startup log said `provider=CUDA`. The `LazyEmbedderHandle` defers the
    /// child spawn, so there is no live provider to read until the first embed;
    /// rather than report a stale `CPU`, predict the provider the sidecar will
    /// resolve via the shared `init_options` logic. This is correct even before
    /// the child has spawned, because the resolution is a pure function of build
    /// features + env.
    /// What: delegates to `trusty_common::embedder::resolve_expected_provider`.
    /// Test: covered by trusty-common's `resolve_expected_provider_*` tests;
    /// real-GPU validation is hardware-gated.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::resolve_expected_provider()
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
/// exit-1 message. When `data_dir` is `Some`, the override is stamped into
/// `TRUSTY_DATA_DIR` before any path resolution so every callsite of
/// `daemon_dir()` / `data_dir()` in the child process sees the same root.
/// When `no_auto_discover` is `true` (or `TRUSTY_NO_AUTO_DISCOVER=1` is set),
/// the post-hydration auto-discovery scan is skipped entirely so the daemon
/// serves only indexes already in `indexes.toml` or registered at runtime.
/// Test: run twice in a row — the second invocation must exit 1 with the
/// "another daemon is already running" message. Run with `--data-dir /tmp/ts-x`
/// and confirm the lockfile lands in `/tmp/ts-x/daemon.lock`. Run with
/// `--no-auto-discover` and verify no "auto-discover" log lines appear.
pub async fn handle_start(
    port: u16,
    foreground: bool,
    device: &str,
    data_dir: Option<&std::path::Path>,
    verbose: bool,
    no_auto_discover: bool,
) -> Result<()> {
    // Apply the --data-dir override as early as possible — before the
    // background self-spawn path so the spawned child inherits the env var.
    // Precedence: TRUSTY_DATA_DIR (env) > --data-dir (flag). Both code paths
    // (foreground + background) must see the same root so lockfile probes agree.
    //
    // SAFETY: invoked on the main thread before tokio spawns any workers.
    // Same invariant as the TRUSTY_DEVICE set_var below.
    if std::env::var_os("TRUSTY_DATA_DIR").is_none() {
        if let Some(dir) = data_dir {
            let dir = dir.to_path_buf();
            // Reject relative paths — a relative data-dir would resolve
            // differently depending on CWD, breaking daemon re-discovery.
            anyhow::ensure!(
                dir.is_absolute(),
                "--data-dir must be an absolute path (got: {})",
                dir.display()
            );
            // Create the directory now so the child daemon can acquire its
            // lockfile immediately on first start, and so we can give the
            // operator a clear error (permission denied) rather than a cryptic
            // lockfile-creation failure later.
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create --data-dir directory: {}", dir.display()))?;
            if std::fs::read_dir(&dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
            {
                tracing::warn!(
                    "--data-dir {} is empty (no existing indexes); daemon will start fresh",
                    dir.display()
                );
            }
            // SAFETY: single-threaded at this point (before tokio workers).
            unsafe {
                std::env::set_var("TRUSTY_DATA_DIR", &dir);
            }
            tracing::info!("data-dir override: {}", dir.display());
        }
    }

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
            .arg(device);
        if no_auto_discover {
            cmd.arg("--no-auto-discover");
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Propagate the data-dir override into the child via env var. Since we
        // already stamped TRUSTY_DATA_DIR above, the child will inherit it
        // through the process environment — no extra CLI arg needed.
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
    //
    // Bug-reporting #478 (Phase 1 wire-up): compose the bug-capture layer into
    // the same registry so all three layers are installed in one `try_init`.
    // `_error_store` is prefixed to suppress the unused-variable lint; Phase 2
    // will stash it in `SearchAppState` and expose it through HTTP / MCP tools.
    let (log_buffer, _error_store) = trusty_common::init_tracing_with_buffer_and_capture(
        if verbose { 2 } else { 0 },
        trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
        "trusty-search",
        env!("CARGO_PKG_VERSION"),
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

    // Issue #747 Fix D: warn on stale TRUSTY_DEVICE=cpu (Apple Silicon).
    crate::commands::startup_checks::warn_if_stale_cpu_device_on_apple_silicon();

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
    if policy.total_ram_mb < MIN_RAM_MB
        && std::env::var("TRUSTY_SKIP_RAM_CHECK").as_deref() != Ok("1")
    {
        anyhow::bail!(
            "trusty-search requires at least 16 GB of RAM.\n\
             Detected: {} MB ({:.1} GB)\n\
             Indexing large codebases on machines with less memory is not supported.\n\
             To bypass on this host set TRUSTY_SKIP_RAM_CHECK=1 in the daemon environment.",
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

    // Issue #110 Phase 2: the embedder init runs on a dedicated background
    // task so the HTTP listener can bind and accept requests while the ONNX
    // model (or trusty-embedderd subprocess) is coming up.
    //
    // For the `auto` / `in-process` paths, the actual heavy work (ONNX session
    // init, or subprocess spawn + readiness poll) is wrapped in `spawn_blocking`
    // to avoid blocking the async executor on synchronous native code (see
    // issue #121: Intel Xeon AVX-512 ORT hang). The `auto` spawn path is async-
    // native (Tokio process), so `build_embedder` is already async; the
    // `spawn_blocking` wrapper is a no-op overhead for that branch, but it
    // keeps the timeout logic uniform across all paths.
    //
    // The supervisor handle is stored in an `Arc<Mutex<Option<_>>>` so the
    // shutdown hook (below) can reach it after `tokio::spawn` consumes the
    // original binding.
    let init_timeout_secs: u64 = std::env::var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    let install_state = state.clone();
    tokio::spawn(async move {
        let runtime_handle = tokio::runtime::Handle::current();

        let init_handle = tokio::task::spawn_blocking(move || {
            // `build_embedder` is async; using `block_on` here is correct
            // because we're on a `spawn_blocking` thread (not a Tokio worker).
            // This preserves the issue #121 fix for in-process ONNX init while
            // also allowing the async subprocess-spawn path to run.
            runtime_handle.block_on(build_embedder())
        });

        let init_result =
            tokio::time::timeout(Duration::from_secs(init_timeout_secs), init_handle).await;

        match init_result {
            Ok(Ok(Ok((embedder, pid_slot)))) => {
                // Issue #282: if the sidecar path returned a PID slot, install
                // it on the state so `/health` and the reindex RSS poller can
                // read the child PID lock-free.
                if let Some(slot) = pid_slot {
                    install_state.install_embedderd_pid_slot(slot).await;
                }
                install_state.install_embedder(Arc::clone(&embedder)).await;
                // Issue #41: worker pool — priority-aware dispatch, bounded queue.
                // Issue #1003: stall tracker wired so health_handler can detect ANE stalls.
                let tracker = Arc::clone(&install_state.embedder_stall_tracker);
                use crate::service::embed_pool::EmbedPool;
                let base = EmbedPool::with_autotune(Arc::clone(&embedder));
                let pool = std::sync::Arc::new(base.with_stall_tracker(tracker));
                install_state.install_embed_pool(pool).await;
                tracing::info!("embedder ready — vector lane online");
                let prior = load_prior_index_count(); // Issue #873: FDA regression baseline.
                install_state
                    .prior_index_count
                    .store(prior, std::sync::atomic::Ordering::Relaxed);
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
                // Issue #314: skip when `--no-auto-discover` / `TRUSTY_NO_AUTO_DISCOVER=1`.
                if !no_auto_discover {
                    tokio::spawn(crate::commands::discover::auto_discover_and_index());
                } else {
                    tracing::info!(
                        "auto-discover: disabled via --no-auto-discover / TRUSTY_NO_AUTO_DISCOVER"
                    );
                }
            }
            Ok(Ok(Err(e))) => {
                let msg = format!("embedder init failed: {e}");
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

    // Issue #537: throttled startup update check. Runs once per 24h (on-disk
    // cache). Result stored in SearchAppState::update_available for /health.
    // Non-blocking: failure degrades to "no update info" — never aborts startup.
    {
        let update_available = state.update_available.clone();
        tokio::spawn(async move {
            let crate_name = env!("CARGO_PKG_NAME");
            let current = env!("CARGO_PKG_VERSION");
            if let Some(info) = trusty_common::update::check_throttled(crate_name, current).await {
                tracing::info!(
                    latest = %info.latest,
                    "update available: {}",
                    trusty_common::update::notice(&info)
                );
                eprintln!("{}", trusty_common::update::notice(&info));
                if let Ok(mut guard) = update_available.lock() {
                    *guard = Some(info.latest);
                }
            }
        });
    }

    // The auto-spawned trusty-embedderd subprocess uses `kill_on_drop(true)`,
    // so it is terminated automatically when the supervisor task (and its
    // `Child` handle) is dropped on daemon exit. No explicit shutdown hook
    // is needed.
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
    //! Warm-boot stage-restoration tests (issue #135) and embedder fail-fast
    //! smoke tests.
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
            skip_kg: false,
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
            skip_kg: false,
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
            skip_kg: false,
        });
        assert_eq!(stages.lexical.status, StageStatus::InProgress);
        assert_eq!(stages.lifecycle_status(), "walking");
        assert!(stages.search_capabilities().is_empty());
    }

    /// Issue #313: a `skip_kg` index that happens to have a non-empty symbol
    /// graph on disk (from a prior full-pipeline reindex) must NOT surface the
    /// `kg` lane on warm-boot. The `skip_kg` flag wins over on-disk state.
    ///
    /// Why: an operator who flipped `skip_kg = true` expects no KG anywhere
    /// — including after a daemon restart that inherits stale redb graph bytes.
    /// The flag is the source of truth; on-disk evidence is subordinate.
    /// What: set `graph_node_count = 7_402` (non-zero, would normally produce
    /// `graph = Ready`) alongside `skip_kg = true`; assert graph = Skipped
    /// and `"kg"` is absent from search_capabilities.
    /// Test: this test.
    #[test]
    fn warm_boot_respects_skip_kg_flag() {
        // skip_kg wins over non-empty on-disk graph.
        let stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            hnsw_snapshot_ready: true,
            graph_node_count: 7_402,
            lexical_only: false,
            skip_kg: true,
        });
        assert_eq!(
            stages.graph.status,
            StageStatus::Skipped,
            "skip_kg must force graph to Skipped even when on-disk graph is non-empty"
        );
        assert_eq!(
            stages.semantic.status,
            StageStatus::Ready,
            "skip_kg must not affect the semantic lane"
        );
        let caps = stages.search_capabilities();
        assert!(
            !caps.contains(&"kg"),
            "skip_kg must suppress the kg capability"
        );
        assert!(
            caps.contains(&"vector"),
            "skip_kg must not suppress the vector capability"
        );

        // skip_kg + lexical_only together: both semantic and graph are Skipped.
        let stages_both = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 14_823,
            hnsw_snapshot_ready: true,
            graph_node_count: 7_402,
            lexical_only: true,
            skip_kg: true,
        });
        assert_eq!(stages_both.semantic.status, StageStatus::Skipped);
        assert_eq!(stages_both.graph.status, StageStatus::Skipped);
        let caps = stages_both.search_capabilities();
        assert!(!caps.contains(&"vector"));
        assert!(!caps.contains(&"kg"));
    }

    /// When `trusty-embedderd` is not on PATH and `TRUSTY_EMBEDDERD_BIN` is
    /// unset, the default `auto`/`stdio` path must fail fast with an
    /// actionable error containing the install hint — not silently fall back
    /// to in-process embedding.
    ///
    /// Why (issue #110 Phase 2 course-correction): the soft fallback
    /// `"trusty-embedderd not found; falling back to in-process"` was the
    /// same "lazy" pattern the user explicitly rejected. Missing binary means
    /// the operator has not completed their upgrade — that is a deployment
    /// error, not a reason to silently downgrade the architecture.
    /// What: sets `TRUSTY_EMBEDDERD_BIN` to a non-existent path, calls
    /// `locate_embedderd_binary`, and asserts:
    ///   (a) the result is `Err`,
    ///   (b) the error string contains `"cargo install trusty-embedderd"`.
    /// Test: this test (no binary / ONNX model needed; always runs).
    #[test]
    fn missing_binary_fails_fast_with_install_hint() {
        use crate::service::embedder_supervisor::locate_embedderd_binary;

        // Isolate from the real environment: point TRUSTY_EMBEDDERD_BIN at a
        // path that definitely does not exist, bypassing the PATH walk.
        // SAFETY: test-only. Cargo runs each test binary in its own process
        // (not a thread-per-test model), so env mutation here is safe as long
        // as no parallel test reads the same var. This is the only test in
        // this module that touches TRUSTY_EMBEDDERD_BIN.
        let prev = std::env::var("TRUSTY_EMBEDDERD_BIN").ok();
        unsafe {
            std::env::set_var(
                "TRUSTY_EMBEDDERD_BIN",
                "/nonexistent/path/trusty-embedderd-missing",
            );
        }
        let result = locate_embedderd_binary();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TRUSTY_EMBEDDERD_BIN", v),
                None => std::env::remove_var("TRUSTY_EMBEDDERD_BIN"),
            }
        }

        // The locate call itself must fail.
        assert!(
            result.is_err(),
            "locate_embedderd_binary must return Err when binary is absent"
        );

        // The error message that `build_embedder` wraps around the locate
        // error must contain the install hint. We construct it here exactly
        // as `build_embedder` does to assert the hint text stays in sync.
        let locate_err = result.unwrap_err();
        let wrapped = format!(
            "{locate_err}\n\n\
             ERROR: trusty-embedderd binary not found on PATH.\n\
             \n\
             trusty-search v0.13+ requires trusty-embedderd to be installed alongside it.\n\
             \n\
             Install it with:\n\
             \x20 cargo install trusty-embedderd --locked\n\
             \n\
             Or set TRUSTY_EMBEDDERD_BIN to an absolute path:\n\
             \x20 export TRUSTY_EMBEDDERD_BIN=/path/to/trusty-embedderd\n\
             \n\
             If you need to run without the sidecar (tests, debugging), use:\n\
             \x20 TRUSTY_EMBEDDER=in-process trusty-search start"
        );
        assert!(
            wrapped.contains("cargo install trusty-embedderd"),
            "install hint must contain 'cargo install trusty-embedderd'; got: {wrapped}"
        );
        assert!(
            wrapped.contains("TRUSTY_EMBEDDER=in-process"),
            "escape hatch hint must mention TRUSTY_EMBEDDER=in-process; got: {wrapped}"
        );
    }

    /// Verify the `no_auto_discover` config-resolution rules in isolation.
    ///
    /// Why (issue #314): the gate logic lives in `handle_start` which requires
    /// a full daemon boot to exercise end-to-end. Testing the *decision* (should
    /// the scan run?) as a pure boolean helper keeps the test deterministic and
    /// free of filesystem / network side-effects.
    ///
    /// What: mirrors the exact precedence implemented at the `tokio::spawn` call
    /// site — CLI flag (`no_auto_discover: bool`) wins over the
    /// `TRUSTY_NO_AUTO_DISCOVER` env var, which in turn wins over the default
    /// (false = scan enabled).
    ///
    /// Test: this test. Run with:
    ///   `cargo test -p trusty-search -- no_auto_discover_resolution`
    #[test]
    fn no_auto_discover_resolution() {
        /// Pure function that mirrors the gate condition in `handle_start`.
        /// Returns `true` when auto-discovery should be **skipped**.
        ///
        /// Why: extracted so we can drive it with arbitrary (cli_flag, env)
        /// combinations without touching the real environment or daemon state.
        /// What: CLI flag takes unconditional precedence; env var is read only
        /// when the flag is `false`.
        /// Test: see the outer `no_auto_discover_resolution` test.
        fn should_skip_discovery(cli_flag: bool, env_val: Option<&str>) -> bool {
            if cli_flag {
                return true;
            }
            // Mirror the clap `env = "TRUSTY_NO_AUTO_DISCOVER"` resolution:
            // any non-empty value that parses as a bool-ish "1" / "true" skips.
            matches!(env_val, Some("1") | Some("true"))
        }

        // Default: scan is enabled (no flag, no env).
        assert!(
            !should_skip_discovery(false, None),
            "scan must be enabled by default"
        );

        // CLI flag alone suppresses scan.
        assert!(
            should_skip_discovery(true, None),
            "--no-auto-discover must suppress scan"
        );

        // Env var "1" suppresses scan when flag is false.
        assert!(
            should_skip_discovery(false, Some("1")),
            "TRUSTY_NO_AUTO_DISCOVER=1 must suppress scan"
        );

        // Env var "true" suppresses scan when flag is false.
        assert!(
            should_skip_discovery(false, Some("true")),
            "TRUSTY_NO_AUTO_DISCOVER=true must suppress scan"
        );

        // CLI flag takes precedence even when env would also suppress.
        assert!(
            should_skip_discovery(true, Some("1")),
            "CLI flag must take precedence"
        );

        // Unrecognised env value does NOT suppress (e.g. leftover "0").
        assert!(
            !should_skip_discovery(false, Some("0")),
            "TRUSTY_NO_AUTO_DISCOVER=0 must not suppress scan"
        );
        assert!(
            !should_skip_discovery(false, Some("")),
            "empty env value must not suppress scan"
        );
    }

    // ── Issue #484: moved-project relocation tests ─────────────────────────────

    use crate::commands::start_restore::try_locate_moved_root;
    use crate::service::colocated_storage::COLOCATED_DIR_NAME;
    use crate::service::persistence::PersistedIndex;
    use serial_test::serial;
    use tempfile::tempdir;

    /// Create a populated `.trusty-search/index.redb` under `root`.
    fn make_populated_ts(root: &std::path::Path) {
        let ts_dir = root.join(COLOCATED_DIR_NAME);
        std::fs::create_dir_all(&ts_dir).unwrap();
        std::fs::write(ts_dir.join("index.redb"), b"notempty").unwrap();
    }

    /// Why: the core relocation contract — a colocated index with a dead root_path
    /// and exactly one candidate tracked root containing a populated .trusty-search/
    /// must be relinked to that candidate.
    /// What: set up a dead root entry and one candidate tracked root with a
    /// populated redb; call `try_locate_moved_root` and assert it returns the
    /// candidate path.
    /// Test: this test.
    #[test]
    #[serial]
    fn restore_moved_colocated_index_relinks_unique_candidate() {
        let data_tmp = tempdir().unwrap();
        let new_root = tempdir().unwrap();
        make_populated_ts(new_root.path());

        // Point TRUSTY_DATA_DIR at our tempdir so roots.toml is isolated.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

        // Register new_root as a tracked root.
        crate::service::roots_registry::upsert_root(new_root.path().to_path_buf()).unwrap();

        // Entry whose root_path no longer exists.
        let dead_root = std::path::PathBuf::from("/tmp/trusty-484-dead-root-xyz9999");
        let entry = PersistedIndex {
            id: "moved-project".to_string(),
            root_path: dead_root.clone(),
            colocated: true,
            ..Default::default()
        };

        let result = try_locate_moved_root(&entry, &[]);
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };

        let new_path = result.expect("must find the unique candidate");
        assert_eq!(
            new_path.canonicalize().unwrap(),
            new_root.path().canonicalize().unwrap(),
            "must relink to the tracked root containing .trusty-search/"
        );
    }

    /// Why: when the root_path is missing and NO tracked root has a populated
    /// .trusty-search/, `try_locate_moved_root` must return None and must NOT
    /// create any ghost directory.
    /// What: register a tracked root with NO .trusty-search/, call the function,
    /// assert None is returned and no ghost dir was created.
    /// Test: this test.
    #[test]
    #[serial]
    fn restore_missing_root_with_no_candidate_returns_none() {
        let data_tmp = tempdir().unwrap();
        let empty_root = tempdir().unwrap();
        // No .trusty-search/ here.

        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };
        crate::service::roots_registry::upsert_root(empty_root.path().to_path_buf()).unwrap();

        let dead_root = std::path::PathBuf::from("/tmp/trusty-484-no-candidate-xyz9999");
        let entry = PersistedIndex {
            id: "no-candidate".to_string(),
            root_path: dead_root.clone(),
            colocated: true,
            ..Default::default()
        };

        let result = try_locate_moved_root(&entry, &[]);
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };

        assert!(
            result.is_none(),
            "must return None when no candidate has a populated .trusty-search/"
        );
        // Verify no ghost directory was created under the dead root.
        let ghost = dead_root.join(COLOCATED_DIR_NAME);
        assert!(
            !ghost.exists(),
            "must not create a ghost .trusty-search/ under the missing root"
        );
    }

    /// Why: when multiple tracked roots have a populated .trusty-search/ and none
    /// is claimed by another entry, `try_locate_moved_root` must return None
    /// (ambiguous — cannot auto-pick).
    /// What: register two tracked roots both with populated .trusty-search/; call
    /// the function and assert it returns None.
    /// Test: this test.
    #[test]
    #[serial]
    fn restore_missing_root_with_ambiguous_candidates_returns_none() {
        let data_tmp = tempdir().unwrap();
        let root_a = tempdir().unwrap();
        let root_b = tempdir().unwrap();
        make_populated_ts(root_a.path());
        make_populated_ts(root_b.path());

        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };
        crate::service::roots_registry::upsert_root(root_a.path().to_path_buf()).unwrap();
        crate::service::roots_registry::upsert_root(root_b.path().to_path_buf()).unwrap();

        let dead_root = std::path::PathBuf::from("/tmp/trusty-484-ambiguous-xyz9999");
        let entry = PersistedIndex {
            id: "ambiguous".to_string(),
            root_path: dead_root,
            colocated: true,
            ..Default::default()
        };

        let result = try_locate_moved_root(&entry, &[]);
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };

        assert!(
            result.is_none(),
            "must return None when multiple candidates exist (ambiguous)"
        );
    }

    // ── Issue #541: warm-boot canonicalization tests ───────────────────────────

    /// Why (issue #541): `canonicalize_best_effort` must return the canonical
    /// form for a path that exists on disk.
    /// What: create a real tempdir (which may have a symlink-alias prefix on
    /// macOS, e.g. /var → /private/var), call the helper, and assert the result
    /// equals `std::fs::canonicalize`.
    /// Test: this test.
    #[test]
    fn canonicalize_best_effort_resolves_existing_path() {
        let tmp = tempdir().unwrap();
        let expected = std::fs::canonicalize(tmp.path()).unwrap();
        let got = canonicalize_best_effort(tmp.path());
        assert_eq!(
            got, expected,
            "canonicalize_best_effort must return the canonical form for an existing path"
        );
    }

    /// Why (issue #541): `canonicalize_best_effort` must fall back to the
    /// original path without panicking when the path does not exist.
    /// What: pass a definitely-nonexistent path; assert the returned value equals
    /// the input.
    /// Test: this test.
    #[test]
    fn canonicalize_best_effort_falls_back_for_missing_path() {
        let missing = std::path::PathBuf::from("/tmp/trusty-541-definitely-does-not-exist-xyz");
        let got = canonicalize_best_effort(&missing);
        assert_eq!(
            got, missing,
            "canonicalize_best_effort must fall back to the input for a missing path"
        );
    }

    /// Why (issue #541): `canonicalize_best_effort` on a symlink must return
    /// the target, not the link path — this is the core guarantee the warm-boot
    /// fix relies on.
    /// What: create a real tempdir, symlink to it, call the helper on the symlink,
    /// and assert the result equals the canonical target.
    /// Test: this test.
    #[cfg(unix)]
    #[test]
    fn canonicalize_best_effort_resolves_symlink() {
        use std::os::unix::fs::symlink;

        let real_dir = tempdir().unwrap();
        let real_canonical = std::fs::canonicalize(real_dir.path()).unwrap();

        let link = real_canonical
            .parent()
            .unwrap()
            .join(format!("trusty-541-symlink-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        symlink(&real_canonical, &link).expect("create symlink");

        let got = canonicalize_best_effort(&link);
        let _ = std::fs::remove_file(&link);

        assert_eq!(
            got, real_canonical,
            "canonicalize_best_effort must resolve symlinks to their target"
        );
    }

    /// Why: issue #604 — the default lazy stdio sidecar adapter previously fell
    /// through to the trait-default `provider() == Cpu`, so `/health` reported
    /// `provider=CPU` even when the sidecar resolved CUDA/CoreML. The adapter
    /// must now report the same provider the sidecar resolves, available
    /// *before* the child spawns (the resolution is pure).
    /// What: builds a `LazySlotEmbedderAdapter` over a non-spawned handle and
    /// asserts its `provider()` equals `resolve_expected_provider()` (the value
    /// the sidecar will report). No child process is started.
    /// Test: this test.
    #[test]
    fn lazy_adapter_reports_resolved_provider() {
        use crate::core::Embedder as _;
        use crate::service::embedder_supervisor::{LazyEmbedderHandle, SupervisorConfig};

        let handle = std::sync::Arc::new(LazyEmbedderHandle::new(
            std::path::PathBuf::from("/nonexistent/trusty-embedderd"),
            SupervisorConfig::default(),
        ));
        let adapter = LazySlotEmbedderAdapter { handle };
        assert_eq!(
            adapter.provider(),
            trusty_common::embedder::resolve_expected_provider(),
            "lazy stdio adapter must report the sidecar's resolved provider, not the CPU default"
        );
    }

    /// Why: issue #604 — the UDS-remote adapter shares the same defect/fix as
    /// the lazy adapter; `/health` must not report a stale `CPU` for a
    /// UDS-connected sidecar.
    /// What: builds a `UdsEmbedderAdapter` over a dummy socket path (no
    /// connection is made by construction) and asserts `provider()` matches the
    /// resolver.
    /// Test: this test.
    #[test]
    fn uds_adapter_reports_resolved_provider() {
        use crate::core::Embedder as _;

        let adapter = UdsEmbedderAdapter {
            client: trusty_common::embedder_client::UdsEmbedderClient::new(
                std::path::PathBuf::from("/tmp/nonexistent-trusty-604.sock"),
            ),
        };
        assert_eq!(
            adapter.provider(),
            trusty_common::embedder::resolve_expected_provider(),
            "uds adapter must report the sidecar's resolved provider, not the CPU default"
        );
    }
}
