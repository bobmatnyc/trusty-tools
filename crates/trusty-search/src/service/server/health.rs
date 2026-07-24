//! `GET /health` and `POST /upgrade` handlers.
//!
//! Why: Health probes are polled at 2s intervals by external orchestrators;
//! keeping them in a focused module makes response-shape changes easy to review.
//! What: `health_handler` returns daemon liveness + embedder status + resource
//! metrics. `upgrade_handler` drives `cargo install` and triggers a self-restart.
//! Test: `health_handler_reports_indexes_and_uptime` and related tests in
//! `super::tests`.
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::embed::Embedder as _;
use crate::service::embedder_supervisor::{BackendKind, BootstrapState};

use super::state::{ReconcileSummary, SearchAppState, WarmBootSummary};

/// Response shape for `GET /health` (issue #34 + #35 + #38 + #282 + #537 +
/// #1003).
#[derive(Serialize)]
pub(super) struct HealthResponse {
    pub(super) status: &'static str,
    pub(super) version: &'static str,
    pub(super) indexes: usize,
    pub(super) uptime_secs: u64,
    /// Embedder functional status (issue #1003).
    ///
    /// Values:
    /// - `"ready"` — embedder loaded and recent embed calls succeeded.
    /// - `"stalled"` — sidecar alive but recent embed calls timed out; daemon
    ///   falls back to BM25-only until the sidecar recovers. Distinguished from
    ///   "down/unreachable" (process missing) and "error" (init failure).
    /// - `"initializing"` — embedder model still loading.
    /// - `"error"` — embedder init task failed or timed out.
    pub(super) embedder: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedder_error: Option<String>,
    /// Seconds since the last successful embed call (issue #1003).
    /// Absent when the embedder has never produced a successful result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedder_last_ok_secs_ago: Option<u64>,
    /// Count of consecutive/recent embed timeouts since the last success
    /// (issue #1003). Non-zero indicates the sidecar is alive but stalled.
    /// Resets to 0 when the next embed call succeeds.
    pub(super) embedder_recent_timeout_count: u32,
    /// Process RSS in MB. On `try_lock` contention returns the last
    /// successfully-sampled value; `0` only before the very first sample.
    pub(super) rss_mb: u64,
    pub(super) rss_limit_mb: u64,
    pub(super) disk_bytes: u64,
    /// CPU usage percent. Same staleness semantics as `rss_mb`: returns the
    /// last good sample on contention, `0.0` only before the first sample.
    pub(super) cpu_pct: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedder_info: Option<EmbedderInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedderd_rss_mb: Option<u64>,
    pub(super) background_reindex_queue_depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) update_available: Option<String>,
    /// Warm-boot summary: how many indexes loaded vs. skipped and by what
    /// reason (issue #873). Always present after warm-boot completes; all
    /// fields are `0`/`false` on a fresh start before warm-boot runs.
    ///
    /// Why: makes the "cargo install dropped FDA" symptom (`indexes:2` from
    /// `~102`) immediately visible without tailing logs. The `warm_boot_degraded`
    /// boolean is the machine-readable flag external monitors should poll.
    pub(super) warmboot_summary: WarmBootSummary,
    /// Number of registered indexes with the KG component disabled
    /// (`skip_kg == true`) — issue #2984 Phase 1.
    ///
    /// Why: gives operators a machine-readable count of per-component
    /// suppression across the fleet without scanning every
    /// `GET /indexes/:id/status` response individually.
    /// What: computed live on every `GET /health` poll from the registry.
    /// Test: `health_surfaces_component_disabled_counts` in
    /// `tests_components`.
    pub(super) indexes_kg_disabled: usize,
    /// Number of registered indexes with the vector component disabled
    /// (`skip_vector == true`) — issue #2984 Phase 1. See `indexes_kg_disabled`.
    pub(super) indexes_vector_disabled: usize,
    /// Number of registered indexes with an enabled component (KG and/or
    /// vector) whose stage is currently `InProgress` — issue #2984 Phase 1.
    /// Covers BOTH a runtime component re-enable catch-up and an ordinary
    /// in-flight reindex (both drive the same stage transition); operators
    /// wanting to distinguish the two should consult
    /// `GET /indexes/:id/status`'s `stages` block alongside
    /// `GET /indexes/:id/reindex/stream`.
    pub(super) indexes_component_catch_up_in_progress: usize,
    /// Boot-time reconcile summary (issue #1672).
    ///
    /// Why: boot-reconcile catches stale indexes (git-delta path since #1670,
    /// mtime path since #1672) but previously only logged results. Surfacing it
    /// here lets operators and monitors see reconciliation outcome without
    /// grepping logs. `None` before the first daemon reconcile run (back-compat:
    /// daemons with reconcile disabled via `TRUSTY_NO_BOOT_RECONCILE=1` omit it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) boot_reconcile: Option<ReconcileSummary>,
    /// Count of registered indexes whose file watcher was refused because
    /// their root was detected as a network-mounted filesystem (issue
    /// #3408).
    ///
    /// Why: inotify/FSEvents cannot observe writes made by another host onto
    /// a shared EFS/NFS/SMB mount — an OS-level limitation. Starting the
    /// watcher anyway would leave the daemon reporting healthy while the
    /// watcher silently never fires for cross-host changes. This field is
    /// the machine-readable signal that lets an operator/monitor catch that
    /// condition without tailing logs. A non-zero value means those indexes
    /// must be kept in sync via `POST /indexes/:id/index-file` and
    /// `POST /indexes/:id/remove-file` instead (the supported
    /// network-mount pattern — see the operator docs).
    /// What: `WatcherManager::network_degraded_count()` at poll time.
    /// Test: `health_surfaces_watcher_network_degraded_count`.
    pub(super) indexes_watcher_network_degraded: usize,
    /// Backend hot-swap bootstrap status (epic #3524 slice 6 — PR 2/5,
    /// closes #3530 / #3493 P1).
    ///
    /// Why: PR-3's hot-swap orchestrator will drive a Python/MPS sidecar
    /// through a multi-second venv-bootstrap window before it can serve a
    /// request; operators need a machine-readable signal for "still
    /// bootstrapping" vs "bootstrap failed and fell back to ort" instead of
    /// grepping logs.
    /// What: one of `"n/a"` (no bootstrap applies — every backend in PR-1/
    /// PR-2, and the common case whenever the currently-installed backend
    /// needed no bootstrap step), `"bootstrapping"`, `"ready"`, `"failed"`,
    /// or `"fell_back_to_ort"` — mirrors
    /// [`crate::service::embedder_supervisor::BootstrapState`]. `"n/a"` when
    /// no [`SwitchableEmbedder`] handle is installed yet (the same
    /// deferred-init gap `embedder_info` falls back for — see
    /// `current_switchable_embedder`'s ordering note).
    /// Test: `health_reports_embedder_bootstrap_state`.
    pub(super) embedder_bootstrap: &'static str,
}

/// Embedding-model metadata surfaced by `GET /health` (issue #38; reworked
/// epic #3524 slice 6 — PR 2/5, closes #3530 / #3493 P1).
///
/// Why: the redesigned web UI's Health view shows which model is loaded, its
/// output dimension, and which execution provider (CPU / CoreML / CUDA / MPS)
/// is active. Before this PR every field except `dimension` was a PREDICTION
/// (`resolve_expected_provider()`, `dimension == 384`) rather than the truth —
/// e.g. the Python/MPS sidecar reported `provider=CoreML` (it never touches
/// ONNX Runtime) and `quantized` was unconditionally `true` (`dimension ==
/// 384` holds for every backend this crate ever builds, quantized or not).
/// What: sourced from `SwitchableEmbedder::active()` (the REAL installed
/// backend, epic #3524 PR-1) when available; falls back to the old
/// prediction-based path only in the brief window before a
/// `SwitchableEmbedder` handle is installed (see
/// `SearchAppState::current_switchable_embedder`'s ordering note).
/// Test: `health_includes_embedder_info_when_ready` (fallback path, no
/// switchable installed); `health_reports_switchable_python_backend_info` and
/// `health_reports_switchable_ort_backend_info` (ort and python
/// `ActiveBackend`s via the switchable path).
#[derive(Serialize)]
pub(super) struct EmbedderInfo {
    /// Vector dimensionality reported by the embedder (384 for all-MiniLM-L6).
    dimension: usize,
    /// Active execution provider: `"CPU"`, `"CoreML"`, `"CoreML(ANE)"`,
    /// `"CUDA"`, or `"MPS"`.
    provider: String,
    /// Whether the loaded model is the INT8-quantized variant.
    quantized: bool,
    /// Human-readable model identifier (e.g. `"all-MiniLM-L6-v2"`).
    model: String,
    /// Which backend implementation is serving embeddings: `"ort"`,
    /// `"python"`, `"remote"` (HTTP or UDS — see the `BackendKind::Remote`
    /// forward-note on `backend_kind_str`), `"in_process"`, `"candle"`, or
    /// `"unknown"` (the pre-switchable fallback path, which cannot
    /// distinguish backends).
    backend: String,
}

/// `BackendKind` → the `backend` string `/health` reports.
///
/// Why: kept as a standalone pure function so the mapping is unit-testable
/// without constructing a full `HealthResponse`.
/// What: one lowercase tag per variant.
///
/// Forward-note (PR-1 review, low priority): `BackendKind::Remote` collapses
/// HTTP and UDS remotes into one variant, so this always reports `"remote"`
/// for both transports. Splitting into `RemoteHttp`/`RemoteUnix` (or adding a
/// sub-label) would let `/health` distinguish them — deferred as low
/// priority since neither has resolution-order or safety implications.
/// Test: `backend_kind_str_maps_every_variant`.
fn backend_kind_str(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Ort => "ort",
        BackendKind::Python => "python",
        BackendKind::Remote => "remote",
        BackendKind::InProcess => "in_process",
        BackendKind::Candle => "candle",
    }
}

/// `BootstrapState` → the `embedder_bootstrap` string `/health` reports.
///
/// Why: standalone pure function, same rationale as [`backend_kind_str`].
/// Test: `bootstrap_state_str_maps_every_variant`.
fn bootstrap_state_str(state: BootstrapState) -> &'static str {
    match state {
        BootstrapState::NotApplicable => "n/a",
        BootstrapState::Bootstrapping => "bootstrapping",
        BootstrapState::Ready => "ready",
        BootstrapState::Failed => "failed",
        BootstrapState::FellBackToOrt => "fell_back_to_ort",
    }
}

pub(super) async fn health_handler(
    State(state): State<Arc<SearchAppState>>,
) -> Json<HealthResponse> {
    // Why: open-mpm (and other external integrators) probe `/health` to detect
    // a running trusty-search daemon before spawning their own. Including
    // `indexes` count lets the caller verify the daemon is not only alive but
    // also has the expected registry populated (issue #34).
    // What: returns `{ status, version, indexes, uptime_secs }` where
    // `indexes` is the number of registered IndexHandles in the registry
    // and `uptime_secs` is wall-clock seconds since AppState construction.
    // Test: register N indexes, GET /health, assert `indexes == N` and
    // `uptime_secs >= 0`.
    //
    // Issue #1006 — Option B: this handler MUST NOT block on any contended
    // lock. An embed stall (CoreML/CUDA) can hold `embedder_slot` in a write
    // lock for up to 30 s; `.await`-ing it here would block the health handler
    // for the same duration, causing external probes (trusty-review, open-mpm)
    // to see a false "daemon down". All lock accesses below use either the
    // watch-based `is_embedder_ready()` (no lock) or `try_read()` / `try_lock()`
    // (returns immediately rather than parking the handler).
    let embedder_error = state.current_embedder_error();
    // Issue #1003: read stall state for the functional health check.
    // These reads are lock-free (AtomicU64/U32) — safe to call from the
    // health handler without risking the 30 s stall that blocked #1006.
    let embedder_last_ok_secs_ago = state.embedder_stall_tracker.last_ok_secs_ago();
    let embedder_recent_timeout_count = state.embedder_stall_tracker.recent_timeout_count();
    let embedder_status = if state.is_embedder_ready() {
        // The sidecar is alive and the slot is populated — but is it actually
        // responding? Issue #1003: if recent timeouts > 0 and the last ok was
        // more than 30 s ago, the sidecar is stalled (alive but unresponsive).
        // Threshold: > 0 timeouts with no success yet (last_ok_secs_ago = None)
        // OR timeout count >= 1 and no recovery. We use >= 1 to be sensitive —
        // a single 30 s timeout on an interactive query is already disruptive.
        let stalled = embedder_recent_timeout_count > 0;
        if stalled {
            "stalled"
        } else {
            "ready"
        }
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
    //
    // Issue #1006 — Option B: use `try_lock()` instead of `.lock().await` so
    // the health handler never parks waiting for the sys-metrics lock.
    //
    // Issue #1016 review: on contention return the last successfully-sampled
    // values from the atomic cache instead of zeros — zeroed metrics can
    // false-alarm monitors that alert on rss_mb == 0 or cpu_pct == 0.
    let (rss_mb, cpu_pct) = if let Ok(mut metrics) = state.sys_metrics.try_lock() {
        let (rss, cpu) = metrics.sample();
        // Update the atomic cache so contended future polls have a real fallback.
        state
            .last_rss_mb
            .store(rss, std::sync::atomic::Ordering::Relaxed);
        state
            .last_cpu_pct_bits
            .store(cpu.to_bits(), std::sync::atomic::Ordering::Relaxed);
        (rss, cpu)
    } else {
        // Lock contended: return the last successfully-sampled values.
        // Falls back to (0, 0.0) only before the very first sample lands,
        // which is preferable to blocking the handler.
        let rss = state.last_rss_mb.load(std::sync::atomic::Ordering::Relaxed);
        let cpu = f32::from_bits(
            state
                .last_cpu_pct_bits
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        (rss, cpu)
    };
    // `rss_limit_mb` mirrors the resolved TRUSTY_MEMORY_LIMIT_MB soft cap.
    // `memory_limit_mb()` returns `None` when no limit is configured.
    let rss_limit_mb = crate::core::memguard::memory_limit_mb().unwrap_or(0);
    let disk_bytes = state.disk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    // Issue #38 / epic #3524 slice 6 (PR 2/5, closes #3530 / #3493 P1):
    // surface model detail (dimension, provider, quantized, model, backend)
    // once the embedder is wired so the admin UI's Health view doesn't need a
    // separate request.
    //
    // Primary path: read the REAL installed backend off the
    // `SwitchableEmbedder` (epic #3524 PR-1) via `current_switchable_embedder`
    // — `ArcSwapOption::load_full` is wait-free, so this never risks the
    // issue #1006 stall a blocking read could cause.
    //
    // Fallback path: `current_switchable_embedder()` briefly returns `None`
    // between `install_switchable_embedder` and `install_embedder` in the
    // daemon's init task (PR-2 reordered those two calls so the gap is now on
    // the OTHER side — see that method's ordering note — but a defensive
    // fallback costs nothing and covers any other future ordering). When
    // `None`, fall back to the pre-PR-2 provider-inference path via
    // `try_current_embedder()` (issue #1006's non-blocking `try_read()`) so
    // `/health` never panics or 500s and the client can simply re-poll once
    // the switchable handle lands.
    let switchable = state.current_switchable_embedder();
    let embedder_info = if let Some(sw) = switchable.as_ref() {
        let active = sw.active();
        Some(EmbedderInfo {
            dimension: sw.dimension(),
            // Issue #3493 P1 (epic #3524 slice 5 + slice 6): prefer a REAL
            // wire readback from the live embedder
            // (`Embedder::resolved_provider_label`, forwarded by
            // `SwitchableEmbedder` to whichever backend is currently
            // installed) over `ActiveBackend::provider` — a snapshot
            // PREDICTION taken once at construction/swap time (slice 6's
            // `resolve_expected_python_provider`, itself already an
            // improvement over the old ORT-oriented guess, but still not a
            // live readback). Only the Python/MPS sidecar currently reports
            // a real value (echoed in every response frame); every other
            // backend, and the brief window before the first successful
            // embed, falls back to the (now more accurate) prediction
            // unchanged.
            provider: sw
                .resolved_provider_label()
                .unwrap_or_else(|| active.provider.as_str().to_string()),
            quantized: active.quantized,
            model: active.model.clone(),
            backend: backend_kind_str(active.kind).to_string(),
        })
    } else {
        state.try_current_embedder().map(|e| {
            let dimension = e.dimension();
            EmbedderInfo {
                dimension,
                provider: e
                    .resolved_provider_label()
                    .unwrap_or_else(|| e.provider().as_str().to_string()),
                // Best-effort fallback ONLY (no ActiveBackend to read yet):
                // every backend this crate builds embeds at 384-dim
                // regardless of quantization, so this predicate does not
                // actually distinguish fp32 from INT8 — it is retained only
                // because some dimension is better than none while genuinely
                // no better source exists. The primary (switchable) path
                // above reports the real `ActiveBackend::quantized` and does
                // not have this limitation.
                quantized: dimension == trusty_common::embedder::EMBED_DIM,
                model: "all-MiniLM-L6-v2".to_string(),
                backend: "unknown".to_string(),
            }
        })
    };
    let embedder_bootstrap = switchable
        .as_ref()
        .map(|sw| bootstrap_state_str(sw.active().bootstrap))
        .unwrap_or("n/a");
    // Issue #282: sample the sidecar's current RSS (None when not running).
    let embedderd_rss_mb = state
        .current_embedderd_pid()
        .and_then(crate::core::memguard::current_rss_mb_for_pid);
    let update_available = state.update_available.lock().ok().and_then(|g| g.clone());
    // Issue #3748 slice A: `warmboot_summary.warm_boot_degraded` used to be
    // written once by `record_warm_boot_result` at boot completion and never
    // touched again — sticky until a daemon restart even after the deferred-
    // embed catch-up queue (the thing most likely to still be dragging
    // indexes toward Ready) fully drains. Detect that drain here (edge-
    // triggered on the queue's completion epoch, polled by whoever is
    // already calling `/health` — no new background task) and recompute.
    // See `recompute_warm_boot_degraded` for exactly what does/doesn't heal.
    let current_defer_embed_epoch = crate::service::reindex::deferred_embed_completion_epoch();
    let last_seen_defer_embed_epoch = state.last_seen_defer_embed_epoch.swap(
        current_defer_embed_epoch,
        std::sync::atomic::Ordering::AcqRel,
    );
    if current_defer_embed_epoch != last_seen_defer_embed_epoch {
        recompute_warm_boot_degraded(&state);
    }
    // Issue #873: surface the warm-boot summary so a post-`cargo install` FDA
    // regression (`indexes:2` instead of `~102`) is visible without tailing logs.
    // Issue #993: populate `indexes_lazy` live from the cold store so it reflects
    // the current number of not-yet-loaded indexes (decreases as lazy loads occur).
    // Issue #1106: populate `indexes_failed` from the cold store's failed-entries
    // set so operators can distinguish "pending lazy load" from "restore failed".
    let mut warmboot_summary = state
        .warmboot_summary
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    warmboot_summary.indexes_lazy = state.cold_store.len();
    warmboot_summary.indexes_failed = state.cold_store.failed_len();

    // Issue #1870: a registered index whose durable redb corpus failed to open
    // on warm-boot (`DatabaseAlreadyOpen` from an in-process double-open, or an
    // incompatible/corrupt corpus format) warm-boots into the registry with a
    // healthy-looking `chunk_count` but every search stage marked `Failed` (see
    // the `corpus_open_failed` guard in `derive_warm_boot_stages`). Free-text /
    // BM25 / hybrid queries against it silently return `[]` while `/health` used
    // to report `"ok"` — a silent, total search outage the daemon self-certified
    // as healthy. Scan the live registry for any such index and fold the count
    // into the warm-boot degraded signal so a `status != "ok"` (or
    // `warm_boot_degraded`) probe detects the outage without tailing logs.
    //
    // Issue #1006 (non-blocking health handler): use `try_read()` on each
    // handle's `stages` lock rather than `.read().await`. The stages lock is
    // read-mostly and only written by the reindex pipeline, so contention is
    // rare; on the exceedingly-rare miss we skip that handle (undercount by one
    // for this poll) — the failure is stable across restarts and every
    // subsequent 2 s poll re-scans, so it is never lost.
    //
    // Issue #1870 review follow-up: failing open on a miss is the correct
    // trade-off (never block the health handler), but a *persistently*
    // contended handle would otherwise be silently skipped forever — the one
    // remaining path that could stay invisibly degraded. Count every miss into
    // `indexes_health_scan_skipped` and name the skipped index at `debug!` so a
    // persistent miss is detectable (a healthy poll has this at 0; a handle
    // stuck under write-lock contention shows up as a repeated non-zero count
    // for the same id across consecutive polls) without changing the
    // fail-open behavior itself.
    let mut indexes_health_scan_skipped = 0usize;
    // Issue #2984 Phase 1: fold the per-component aggregate counts into the
    // SAME registry scan (rather than a second full iteration) — one pass
    // computes corpus-failure, component-disabled, and catch-up-in-progress
    // counts together.
    let mut indexes_kg_disabled = 0usize;
    let mut indexes_vector_disabled = 0usize;
    let mut indexes_component_catch_up_in_progress = 0usize;
    let indexes_corpus_failed = state
        .registry
        .list_handles()
        .iter()
        .filter(|handle| {
            if handle.skip_kg {
                indexes_kg_disabled += 1;
            }
            if handle.skip_vector {
                indexes_vector_disabled += 1;
            }
            match handle.stages.try_read() {
                Ok(stages) => {
                    if (!handle.skip_kg
                        && stages.graph.status == crate::core::registry::StageStatus::InProgress)
                        || (!handle.skip_vector
                            && stages.semantic.status
                                == crate::core::registry::StageStatus::InProgress)
                    {
                        indexes_component_catch_up_in_progress += 1;
                    }
                    stages.any_failed()
                }
                Err(_) => {
                    indexes_health_scan_skipped += 1;
                    tracing::debug!(
                        index_id = %handle.id,
                        "health: stages lock contended — skipping this handle for this poll (#1870)"
                    );
                    false
                }
            }
        })
        .count();
    warmboot_summary.indexes_corpus_failed = indexes_corpus_failed;
    warmboot_summary.indexes_health_scan_skipped = indexes_health_scan_skipped;
    if indexes_corpus_failed > 0 {
        // Fold into the single machine-readable warm-boot health flag external
        // monitors already poll, so #1870 rides the existing degraded channel.
        warmboot_summary.warm_boot_degraded = true;
    }

    // Issue #1672: read the reconcile summary. Returns None when reconcile has
    // not started yet (disabled via env gate or daemon still in boot). The
    // `skip_serializing_if = "Option::is_none"` on the response field keeps
    // the wire payload backward-compatible.
    let boot_reconcile = state
        .reconcile_summary
        .lock()
        .map(|g| {
            // Only surface a summary that has actually started (in_progress
            // or completed). Before the first reconcile run all counters are
            // 0 and in_progress is false — treat that as "not yet started"
            // so the field is absent rather than a zeroed-out struct.
            let s = g.clone();
            let started = s.in_progress
                || s.up_to_date > 0
                || s.delta_reindexed > 0
                || s.fell_back_to_full > 0
                || s.skipped_no_data > 0;
            if started {
                Some(s)
            } else {
                None
            }
        })
        .unwrap_or(None);

    // Issue #3408: count indexes whose watcher was refused because their root
    // is network-mounted (EFS/NFS/SMB/CIFS) — inotify/FSEvents cannot observe
    // another host's writes there, so starting the watcher would silently
    // never fire. Cheap: just the degraded map's length under its own lock.
    let indexes_watcher_network_degraded = state.watcher_manager.network_degraded_count().await;

    // Issue #1870: honest top-level status. `"ok"` used to be unconditional,
    // so a total silent search outage (all-lanes-Failed corpus) still read as
    // healthy. Downgrade to `"degraded"` when at least one registered index has
    // a failed lane so a simple `status != "ok"` gate catches it. The healthy
    // path is unchanged (`indexes_corpus_failed == 0` → `"ok"`).
    // Issue #3408: also downgrade when a watcher was refused for a
    // network-mounted root — the daemon is not silently broken, but it is
    // running with a real capability gap that the same `status != "ok"`
    // monitors should catch.
    let overall_status =
        if warmboot_summary.indexes_corpus_failed > 0 || indexes_watcher_network_degraded > 0 {
            "degraded"
        } else {
            "ok"
        };

    Json(HealthResponse {
        status: overall_status,
        version: env!("CARGO_PKG_VERSION"),
        indexes: state.registry.list().len(),
        uptime_secs: state.started_at.elapsed().as_secs(),
        embedder: embedder_status,
        embedder_error,
        // Issue #1003: stall-observability fields.
        embedder_last_ok_secs_ago,
        embedder_recent_timeout_count,
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
        indexes_kg_disabled,
        indexes_vector_disabled,
        indexes_component_catch_up_in_progress,
        warmboot_summary,
        boot_reconcile,
        indexes_watcher_network_degraded,
        embedder_bootstrap,
    })
}

/// Recompute the persisted `warmboot_summary.warm_boot_degraded` flag when
/// the deferred-embed catch-up queue drains (issue #3748 slice A), instead of
/// leaving it frozen at whatever `record_warm_boot_result` computed once at
/// boot completion.
///
/// Why: `record_warm_boot_result` (`commands/prior_index_count.rs`) sets
/// `warm_boot_degraded` from three boot-time facts — `indexes_skipped_tcc`,
/// `indexes_skipped_timeout`, and a loaded-vs-prior-count drop — then never
/// touches it again. Two of those three (a TCC-denied volume, a scan
/// timeout) genuinely CANNOT heal without a daemon restart (the index in
/// question never loaded at all), so this function deliberately keeps them
/// as permanent, never-cleared inputs — the explicit instruction is "do not
/// weaken genuine degraded signaling". What CAN change over the daemon's
/// lifetime, and previously had NO effect on this flag, is whether any
/// registered index's stages currently show a `Failed` lane — most notably a
/// deferred-embed pass that itself failed (issue #928 marks `semantic =
/// Failed`, not `Ready`). Folding that live signal in here means a genuinely
/// broken embed pass still counts as degraded (satisfying the same
/// requirement from the other direction), while a boot that had NO tcc/
/// timeout/count issue and whose catch-up queue finished cleanly correctly
/// stops reporting degraded instead of never being reevaluated.
///
/// On "flipping earlier" (while the queue is merely tail-blocked by the
/// final, largest job — every other index already `Ready`): no separate
/// mechanism is needed. `warm_boot_degraded` was never derived from
/// `indexes_component_catch_up_in_progress` (an index mid-embed is
/// `InProgress`, not `Failed`) in the first place — only a genuine failure or
/// boot-time skip sets it. So the moment the last non-tail index reaches
/// Ready, this recompute (once it runs) already reports the honest,
/// non-degraded value; there is no separate "still embedding" penalty to
/// clear early. This is verified by
/// `recompute_does_not_flag_an_in_progress_tail_job_as_degraded`.
///
/// What: re-derives `warm_boot_degraded` as `degraded_by_tcc ||
/// degraded_by_timeout || degraded_by_count || any_stage_failed`, where the
/// first three reuse the ORIGINAL boot-time counters (`indexes_skipped_tcc`,
/// `indexes_skipped_timeout`, and a fresh `registry.list_handles().len()`
/// vs. `prior_index_count` comparison — live rather than the frozen
/// boot-time `total`, since new indexes can register between boot and
/// drain) and `any_stage_failed` is a live scan for any handle with
/// `stages.any_failed() == true` (the identical predicate `/health` already
/// uses for `indexes_corpus_failed`). Persists the result back into
/// `state.warmboot_summary` (not just a per-request clone) and logs the
/// old -> new transition.
/// Test: `warm_boot_degraded_recomputes_to_false_once_catch_up_drains_cleanly`,
/// `warm_boot_degraded_recompute_keeps_a_genuinely_failed_embed_degraded`,
/// `recompute_does_not_flag_an_in_progress_tail_job_as_degraded` in
/// `tests_health_degraded.rs`.
pub(super) fn recompute_warm_boot_degraded(state: &SearchAppState) {
    let (degraded_by_tcc, degraded_by_timeout, old) = match state.warmboot_summary.lock() {
        Ok(summary) => (
            summary.indexes_skipped_tcc > 0,
            summary.indexes_skipped_timeout > 0,
            summary.warm_boot_degraded,
        ),
        Err(_) => return,
    };

    let prior_count = state
        .prior_index_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let handles = state.registry.list_handles();
    let degraded_by_count = prior_count > 0 && handles.len() < prior_count * 4 / 5;
    let any_stage_failed = handles
        .iter()
        .any(|h| h.stages.try_read().map(|s| s.any_failed()).unwrap_or(false));
    let new = degraded_by_tcc || degraded_by_timeout || degraded_by_count || any_stage_failed;

    if let Ok(mut summary) = state.warmboot_summary.lock() {
        summary.warm_boot_degraded = new;
    }

    if new != old {
        tracing::info!(
            "warm_boot_degraded recompute (deferred-embed catch-up drained): {old} -> {new}"
        );
    } else {
        tracing::debug!(
            "warm_boot_degraded recompute (deferred-embed catch-up drained): unchanged at {new}"
        );
    }
}

/// Request body for `POST /upgrade` (issue #537).
///
/// Why: typed body avoids raw JSON field extraction in the handler, and serde
/// provides friendly error messages for malformed requests.
/// What: mirrors the MCP tool schema: `check` (default true) and `confirm`.
/// Test: the MCP `upgrade` tool calls this endpoint.
#[derive(Deserialize)]
pub(super) struct UpgradeRequest {
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
pub(super) async fn upgrade_handler(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `backend_kind_str` must cover every `BackendKind` variant with a
    /// distinct, stable, lowercase tag — a missing/renamed arm would fail to
    /// compile (the match is exhaustive), but this pins the exact strings
    /// `/health` consumers parse against regressions from an innocuous
    /// rename.
    /// Test: this test.
    #[test]
    fn backend_kind_str_maps_every_variant() {
        assert_eq!(backend_kind_str(BackendKind::Ort), "ort");
        assert_eq!(backend_kind_str(BackendKind::Python), "python");
        assert_eq!(backend_kind_str(BackendKind::Remote), "remote");
        assert_eq!(backend_kind_str(BackendKind::InProcess), "in_process");
        assert_eq!(backend_kind_str(BackendKind::Candle), "candle");
    }

    /// `bootstrap_state_str` must cover every `BootstrapState` variant with
    /// a distinct, stable string — same rationale as
    /// `backend_kind_str_maps_every_variant`.
    /// Test: this test.
    #[test]
    fn bootstrap_state_str_maps_every_variant() {
        assert_eq!(bootstrap_state_str(BootstrapState::NotApplicable), "n/a");
        assert_eq!(
            bootstrap_state_str(BootstrapState::Bootstrapping),
            "bootstrapping"
        );
        assert_eq!(bootstrap_state_str(BootstrapState::Ready), "ready");
        assert_eq!(bootstrap_state_str(BootstrapState::Failed), "failed");
        assert_eq!(
            bootstrap_state_str(BootstrapState::FellBackToOrt),
            "fell_back_to_ort"
        );
    }
}
