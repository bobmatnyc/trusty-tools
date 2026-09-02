//! `GET /health` handler.
//!
//! Why: Health probes are polled at 2s intervals by external orchestrators;
//! keeping them in a focused module makes response-shape changes easy to review.
//! What: `health_handler` returns daemon liveness + embedder status + resource
//! metrics. `POST /upgrade` moved to `super::upgrade` in #6688, when this file
//! crossed the 500-SLOC production cap.
//! Test: `health_handler_reports_indexes_and_uptime` and related tests in
//! `super::tests`.
use axum::extract::State;
use axum::Json;
use serde::Serialize;
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
    /// #4390: deferred-embed (C2) catch-up passes queued or in flight.
    ///
    /// Why: `deferred_embed_queue_depth`'s own doc comment claimed it was
    /// exposed here and it was not — only the completion epoch was consumed. It
    /// is the one counter that says an index's semantic lane is still being
    /// filled in, including the boot re-arms this issue added, so an operator
    /// watching a restart drain had no way to see them.
    /// What: the same `QUEUE_DEPTH` gauge `defer_embed_queue` maintains,
    /// fleet-wide rather than per index. Additive field; every existing
    /// consumer parses this response with optional/ignored unknown fields.
    /// Test: `health_reports_the_deferred_embed_queue_depth_end_to_end` in
    /// `tests_stall` — drives a real queued job through
    /// `spawn_deferred_embed_pass` and asserts `/health` reports it, both
    /// enqueued and drained.
    pub(super) deferred_embed_queue_depth: usize,
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
    /// Issue #6688: the IDS of the indexes `warmboot_summary.indexes_stage_failed`
    /// counts. Absent — not `null`, not `[]` — when that count is `0`.
    ///
    /// Why: every failing-index signal on this response was a COUNT, so a
    /// consumer reading `indexes_stage_failed: 1` on a 41-index daemon could
    /// not tell which index failed and had to poll `GET /indexes/:id/status`
    /// for every registered index to find out. That blocks per-index
    /// remediation and made trusty-review degrade every review because one
    /// unrelated index on the host had failed lanes (#6686).
    /// What: `IndexStages::any_failed()` — the SAME predicate, evaluated in the
    /// SAME registry scan, on the same `handle.stages` read that produces
    /// `indexes_stage_failed`. So `indexes_stage_failed_ids.len()` and that
    /// counter can never disagree, including under the fail-open arm: a handle
    /// whose `stages` lock is contended is skipped by both (and counted in
    /// `indexes_health_scan_skipped`). Sorted, because the registry is a
    /// `DashMap` and its iteration order is not stable across polls.
    ///
    /// `any_failed()` and NOT `indexes_corpus_failed`'s
    /// `CodeIndexer::corpus_open_failed`: a corpus-open failure marks every
    /// lane `Failed` (`derive_warm_boot_stages`' `corpus_open_failed` guard),
    /// so a corpus-failed index is already named here by its lanes. Folding the
    /// flag in would name indexes that no counter on this response counts, for
    /// only the post-warm-boot backstop case #5927 added — a corpus-failure
    /// id list is a separate additive field, not a widening of this one.
    ///
    /// Additive and optional by construction (`skip_serializing_if`): a
    /// consumer built against an older daemon never sees the key, and every
    /// existing counter keeps its exact shape. A consumer mirroring this field
    /// MUST spell it `Option<Vec<String>>` or `#[serde(default)]` — a bare
    /// `Vec<String>` hard-fails against a daemon that omits it (#628).
    /// Test: `health_names_the_indexes_with_a_failed_lane`,
    /// `health_omits_the_failing_index_ids_when_nothing_is_failing`, and
    /// `health_counter_wire_shape_is_unchanged_by_the_id_field` in
    /// `stage_failed_5927_tests`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) indexes_stage_failed_ids: Option<Vec<String>>,
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
    /// Count of registered indexes with no embed pool currently attached
    /// (issue #3748 slice B PR 1, boot-race fix — PR #3784 review finding
    /// 2).
    ///
    /// Why: `CodeIndexer::set_embed_pool_source` self-heals a poolless index
    /// onto the daemon's pool the moment it next calls
    /// `resolve_embed_pool` (interactive query or catch-up embed) — but
    /// until that first call happens (or if the index never issues one),
    /// the index silently keeps calling the direct embedder with no
    /// priority routing. This is the machine-readable signal for that gap,
    /// mirroring `indexes_kg_disabled`'s "grep logs instead" motivation.
    /// What: computed live, in the same registry scan as
    /// `indexes_kg_disabled`, via `CodeIndexer::has_embed_pool()` — a
    /// lock-free, non-async read. `0` on a healthy daemon whose pool
    /// installed before any index was constructed (the steady-state warm-boot
    /// case); transiently non-zero only during the boot-race window this
    /// fix closes, and self-corrects to `0` as each poolless index makes
    /// its first embed call.
    /// Test: `health_surfaces_indexes_without_embed_pool` in
    /// `tests_components`.
    pub(super) indexes_embed_pool_missing: usize,
    /// Issue #4680: count of registered indexes that are STUCK — their lexical
    /// stage still owes work (`Pending`/`InProgress`, which
    /// `lifecycle_status` renders as `"created"`/`"walking"`) yet no walk has
    /// ever been driven for them in this daemon's lifetime.
    ///
    /// Why: this is the gap that let a production deployment serve nothing from
    /// 221 of its 222 indexes for 44 days while `/health` reported
    /// `status: ok`, `warm_boot_degraded: false`, `indexes_loaded: 222/222`.
    /// Every existing health signal was structurally blind to it:
    /// `indexes` and `warmboot_summary.indexes_loaded` count registered SLOTS,
    /// not populated ones; `indexes_stage_failed` (`indexes_corpus_failed`
    /// before #5927) only fires on
    /// `stages.any_failed()`, and a stuck index has no `Failed` lane at all —
    /// it is indefinitely, falsely, "walking". Operators had no signal to poll.
    /// What: computed live in the same single registry scan as
    /// `indexes_kg_disabled`, from
    /// [`crate::service::warm_boot::index_is_stuck_unwalked`] — the identical
    /// predicate boot reconcile uses to decide what to re-drive, so the count
    /// and the recovery can never disagree. Non-zero forces the top-level
    /// `status` to `"degraded"`. Converges to `0` on a healthy daemon as boot
    /// reconcile walks each index (the walk stamps `last_walk_started_at`);
    /// stays non-zero exactly when indexes really are stuck.
    /// Test: `health_reports_degraded_for_a_stuck_never_walked_index` and
    /// `health_does_not_flag_a_walked_index_as_stuck` in
    /// `tests_health_degraded`.
    pub(super) indexes_stuck_empty: usize,
    /// Issue #5336: count of registered indexes whose lexical walk STARTED and
    /// was then abandoned — the stage still says `InProgress` (rendered
    /// `"walking"`) but no task is driving the index.
    ///
    /// Why: #4680's `indexes_stuck_empty` deliberately only fires when no walk
    /// ever started, so an index whose walk began and died — the reindex task
    /// panicked, was cancelled, or returned early — was counted by neither. It
    /// is indistinguishable, on every other field of this response and on
    /// `GET /indexes/:id/status`, from an index that is genuinely mid-reindex:
    /// same stage, same `lifecycle_status`, same empty `search_capabilities`.
    /// Nothing completes it and nothing retries it, so it stays that way for
    /// the daemon's lifetime with no signal to poll.
    /// What: computed live in the same single registry scan as
    /// `indexes_stuck_empty`, from
    /// [`crate::service::warm_boot::index_is_stuck_mid_walk`]. The two
    /// predicates partition the `InProgress` lexical lane on `walk_started`, so
    /// an index is counted by at most one of them. Non-zero forces the
    /// top-level `status` to `"degraded"`. This SURFACES the condition; nothing
    /// recovers from it — see the predicate's doc for why.
    /// Test: `health_reports_degraded_for_an_abandoned_mid_walk_index` and
    /// `health_does_not_flag_an_in_flight_reindex_as_stuck_mid_walk` in
    /// `tests_health_degraded`.
    pub(super) indexes_stuck_mid_walk: usize,
    /// Issue #4839: registered indexes that actually hold chunks
    /// (`chunk_count > 0`), as opposed to `indexes` /
    /// `warmboot_summary.indexes_loaded`, which count registration SLOTS.
    ///
    /// Why: a production deployment reported `indexes_loaded: 222/222`,
    /// `indexes_failed: 0`, `status: ok` while only 2 of those 222 held any
    /// data. Every counter on this response was structurally incapable of
    /// showing it, so a consuming application returned empty context on 913
    /// consecutive operations across 44 days with no signal. Registration and
    /// population are different facts and this response now reports both.
    /// What: computed live in the same single registry scan as
    /// `indexes_kg_disabled`, from the DURABLE corpus count (the in-memory map
    /// reads 0 after idle eviction, so it cannot be the source).
    ///
    /// An index is excluded from BOTH this and `indexes_empty` when its count
    /// is unknown rather than zero: the corpus failed to open (`#4333`, already
    /// owned by `indexes_corpus_failed`), the durable count read errored, or
    /// the handle's lock was contended for this poll. So
    /// `indexes_populated + indexes_empty <= indexes` — the shortfall is
    /// indexes whose population could not be determined, never indexes silently
    /// assumed empty.
    /// Test: `health_reports_populated_and_empty_index_counts`,
    /// `health_counts_a_real_corpus_and_excludes_a_corpus_failed_index`.
    pub(super) indexes_populated: usize,
    /// Issue #4839: registered indexes holding zero chunks whose corpus opened
    /// fine — see `indexes_populated`.
    ///
    /// Why: NOT automatically a fault. Some indexes are legitimately empty
    /// (their walk completed and matched no indexable file). What made #4839 a
    /// defect was that nothing distinguished those from indexes that never
    /// walked at all; `indexes_stuck_empty` (#4680) names that subset, and this
    /// counter is the denominator it sits inside. `indexes_empty >
    /// indexes_stuck_empty` is the legitimately-empty cohort.
    /// What: the same scan's zero-chunk count. Deliberately does NOT force
    /// `status: "degraded"` — an empty-but-walked index is honest, and #4680's
    /// stuck predicate already degrades the genuinely broken case.
    /// Test: `health_reports_populated_and_empty_index_counts`.
    pub(super) indexes_empty: usize,
    /// Issue #4839: total chunks across every registered, corpus-healthy index.
    ///
    /// Why: the reporter's suggested at-a-glance signal — a near-zero corpus on
    /// a fleet of hundreds of indexes is visible in one number without diffing
    /// per-index `chunk_count` values by hand, which is how the 44-day outage
    /// was eventually found.
    /// What: summed in the same scan; excludes corpus-failed indexes for the
    /// reason given on `indexes_populated`.
    /// Test: `health_reports_populated_and_empty_index_counts`.
    pub(super) total_chunks: u64,
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
    ///
    /// #4125: `"failed"` and `"fell_back_to_ort"` also force the top-level
    /// `status` to `"degraded"` — both are permanent for this daemon's
    /// lifetime, so search runs on a backend the operator did not configure
    /// until someone restarts it.
    /// Test: `health_reports_embedder_bootstrap_state`,
    /// `health_is_degraded_when_the_embedder_backend_permanently_downgraded`.
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
    // Why: trusty-agents (and other external integrators) probe `/health` to detect
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
    // for the same duration, causing external probes (trusty-review, trusty-agents)
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
    // #4125: a permanent embedder capability downgrade must reach `status`.
    //
    // Why: `embedder_bootstrap` was reported and then aggregated nowhere, so
    // `BootstrapState::Failed` (the graceful python bootstrap gave up for this
    // daemon's lifetime) and `FellBackToOrt` (the swap-back watchdog abandoned a
    // dead python/MPS sidecar) sat next to `status: "ok"` forever. Note that
    // `embedder: "ready"` alongside them is not wrong — it reports the
    // CURRENTLY-ACTIVE backend, which genuinely is ready. What nothing reported
    // was "we did not get the backend we asked for": search silently runs on
    // CPU/ort instead of MPS/python, a real and permanent performance
    // regression, on the one field monitors gate on.
    // What: `Failed` and `FellBackToOrt` are terminal for this daemon (neither
    // path ever re-attempts), so both are degraded. `Bootstrapping` is
    // transient and deliberately excluded — flagging it would make every
    // Apple-Silicon boot report degraded for its first few seconds.
    // `NotApplicable`/`Ready` are the healthy states.
    let embedder_capability_degraded = switchable.as_ref().is_some_and(|sw| {
        matches!(
            sw.active().bootstrap,
            BootstrapState::Failed | BootstrapState::FellBackToOrt
        )
    });
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
    let last_seen_defer_embed_epoch = state
        .last_seen_defer_embed_epoch
        .load(std::sync::atomic::Ordering::Acquire);
    if current_defer_embed_epoch != last_seen_defer_embed_epoch {
        // #5633: consume the drain edge only once the scan behind it was
        // conclusive. Committing the epoch after an inconclusive scan spends
        // the one trigger this recompute gets, which is what made it a
        // one-shot with no re-scan; leaving it armed costs a cheap re-derive
        // on the next poll and lets a contended handle resolve itself.
        if recompute_warm_boot_degraded(&state) {
            state.last_seen_defer_embed_epoch.store(
                current_defer_embed_epoch,
                std::sync::atomic::Ordering::Release,
            );
        }
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
    // #4250: report the LIVE timeout cohort, the same way `indexes_lazy` and
    // `indexes_failed` are already re-read live, rather than the count frozen
    // at boot completion. Before the recovery pass existed the two were the
    // same number forever; now the count falls as indexes are driven back, and
    // a count that does NOT fall is the operator's signal that recovery is
    // failing — which the frozen counter could never distinguish.
    let live_timeout_cohort = state.cold_store.timed_out_len();
    let stored_timeout_count = warmboot_summary.indexes_skipped_timeout;
    warmboot_summary.indexes_skipped_timeout = live_timeout_cohort;
    if live_timeout_cohort > 0 {
        warmboot_summary.warm_boot_degraded = true;
    } else if stored_timeout_count > 0 {
        // Edge: this boot HAD timeouts and the cohort has now drained, so the
        // boot-time counter must stop pinning the flag.
        // `recompute_warm_boot_degraded` re-derives it from every live input
        // (TCC, count drop, failed lanes), so this un-latches without weakening
        // any other degraded signal. Edge-triggered, mirroring the
        // defer-embed-epoch trigger above — no extra handle scan per poll.
        recompute_warm_boot_degraded(&state);
        if let Ok(mut s) = state.warmboot_summary.lock() {
            s.indexes_skipped_timeout = 0;
            warmboot_summary.warm_boot_degraded = s.warm_boot_degraded;
        }
    }

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
    // Issue #3748 boot-race fix (PR #3784 review finding 2): folded into the
    // same scan. `handle.indexer` is a SEPARATE lock from `handle.stages`, so
    // a miss here is independent of `indexes_health_scan_skipped` above —
    // fails open the same way (skip this handle for this poll; a
    // persistently-poolless index self-corrects to 0 the moment it makes its
    // first embed call, so an undercount here is never permanently lost).
    let mut indexes_embed_pool_missing = 0usize;
    // #4680: count indexes whose lexical stage still owes work but that no walk
    // has ever touched — the state every other health signal was blind to.
    let mut indexes_stuck_empty = 0usize;
    // #5336: the complement — a walk that started and was then abandoned.
    let mut indexes_stuck_mid_walk = 0usize;
    // #4839: registration is not population. Folded into the SAME scan, from
    // the durable corpus rather than the in-memory map (which reads 0 after
    // idle eviction and would report a healthy index as empty).
    let mut indexes_populated = 0usize;
    let mut indexes_empty = 0usize;
    let mut total_chunks = 0u64;
    // #5927: the count that actually answers "did a corpus fail to open?".
    // Read from `CodeIndexer::corpus_open_failed` rather than inferred from
    // the stage lanes, so it can never disagree with the per-index
    // `corpus_open_failure` block on `GET /indexes/:id/status`.
    let mut indexes_corpus_failed = 0usize;
    // #6688: name the failing indexes, not just count them — same scan, same
    // `stages` read, same `any_failed()` predicate as the counter below.
    let mut indexes_stage_failed_ids: Vec<String> = Vec::new();
    let indexes_stage_failed = state
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
            if let Ok(indexer) = handle.indexer.try_read() {
                if !indexer.has_embed_pool() {
                    indexes_embed_pool_missing += 1;
                }
                // #4839: a corpus-failed index is counted as NEITHER populated
                // nor empty — its real chunk count is unknown (#4333), and
                // calling it empty would invent a fact.
                if !indexer.corpus_open_failed {
                    // #4839 review: a durable-count READ error gets the same
                    // treatment as a failed open, for the same reason. Falling
                    // back to `indexer.chunk_count()` here would have been a
                    // fail-open inside the fix for fail-opens: the in-memory
                    // map reads 0 after idle eviction (#681), so an unreadable
                    // corpus on a populated index would have been counted as
                    // empty — manufacturing the exact false signal this issue
                    // exists to remove.
                    let counted = match indexer.corpus_arc() {
                        Some(corpus) => match corpus.chunk_count() {
                            Ok(n) => Some(n),
                            Err(e) => {
                                tracing::warn!(
                                    index_id = %handle.id,
                                    error = %e,
                                    "health: durable chunk count unreadable — excluding this \
                                     index from indexes_populated/indexes_empty/total_chunks \
                                     for this poll rather than reporting a count it did not \
                                     produce (#4839)"
                                );
                                None
                            }
                        },
                        // No durable corpus wired at all (BM25-only or test
                        // indexer). The in-memory map is the AUTHORITY for that
                        // shape, not a fallback from a failure — no eviction
                        // can make it lie, because there is nothing else.
                        None => Some(indexer.chunk_count()),
                    };
                    if let Some(chunks) = counted {
                        total_chunks = total_chunks.saturating_add(chunks as u64);
                        if chunks > 0 {
                            indexes_populated += 1;
                        } else {
                            indexes_empty += 1;
                        }
                    }
                } else {
                    // #5927: the same branch that already excludes this index
                    // from the population counts is where the corpus-failure
                    // count belongs — one read of the flag, one meaning.
                    indexes_corpus_failed += 1;
                }
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
                    // #4680: same fail-open discipline as the stages lock above
                    // — a contended walk-diagnostics read undercounts this poll
                    // only, and the next 2 s poll re-scans.
                    if let Ok(diag) = handle.walk_diagnostics.try_read() {
                        let walk_started = diag.last_walk_started_at.is_some();
                        if crate::service::warm_boot::index_is_stuck_unwalked(
                            stages.lexical.status,
                            walk_started,
                        ) {
                            indexes_stuck_empty += 1;
                        }
                        // #5336: the walk started and nothing is driving this
                        // index any more — a frozen `"walking"` claim, not a
                        // live reindex.
                        if crate::service::warm_boot::index_is_stuck_mid_walk(
                            stages.lexical.status,
                            walk_started,
                            crate::service::reindex::index_task_in_flight(&handle.id),
                        ) {
                            indexes_stuck_mid_walk += 1;
                        }
                    }
                    let failed = stages.any_failed();
                    if failed {
                        // #6688: recorded from the same `any_failed()` the
                        // counter is derived from, so the two cannot diverge.
                        indexes_stage_failed_ids.push(handle.id.to_string());
                    }
                    failed
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
    // #6688: stable order — the registry is a `DashMap` and iterates arbitrarily.
    indexes_stage_failed_ids.sort_unstable();
    debug_assert_eq!(
        indexes_stage_failed_ids.len(),
        indexes_stage_failed,
        "#6688: the id list and indexes_stage_failed come from one predicate in one scan"
    );
    // #6688: absent rather than `[]` on a healthy daemon — see the field's doc.
    let indexes_stage_failed_ids = if indexes_stage_failed_ids.is_empty() {
        None
    } else {
        Some(indexes_stage_failed_ids)
    };
    warmboot_summary.indexes_corpus_failed = indexes_corpus_failed;
    warmboot_summary.indexes_stage_failed = indexes_stage_failed;
    warmboot_summary.indexes_health_scan_skipped = indexes_health_scan_skipped;
    // #5927: OR both counts. `indexes_stage_failed` alone reproduces the exact
    // pre-#5927 condition (a corpus-open failure fails every lane, so it is
    // already counted there), and `indexes_corpus_failed` is folded in as a
    // backstop for a corpus failure recorded after warm-boot stage derivation,
    // where nothing would have re-marked the lanes.
    if indexes_stage_failed > 0 || indexes_corpus_failed > 0 {
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
                || s.skipped_no_data > 0
                // #4680: a boot whose ONLY outcome was stuck-index recovery
                // must still surface its summary.
                || s.stuck_retried > 0;
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
    // path is unchanged (`indexes_stage_failed == 0` → `"ok"`).
    // Issue #3408: also downgrade when a watcher was refused for a
    // network-mounted root — the daemon is not silently broken, but it is
    // running with a real capability gap that the same `status != "ok"`
    // monitors should catch.
    // Issue #3706: fold in the FULL `warm_boot_degraded` flag rather than only
    // `indexes_stage_failed`. `warm_boot_degraded` (see its doc comment in
    // `state.rs`) already aggregates FOUR conditions — TCC/FDA denial, scan
    // timeout, corpus-open failure, and mass index loss (< 80% of prior) —
    // but only the corpus-open-failure case was previously reflected in the
    // top-level `status` field. A daemon that was genuinely warm-boot-degraded
    // purely from a TCC denial, a scan timeout, or a mass index loss still
    // reported `status: "ok"`, which is exactly what `warmboot_summary.
    // warm_boot_degraded` was supposed to make impossible to miss. Since
    // `indexes_stage_failed` is already OR'd into `warm_boot_degraded` above
    // (and by `recompute_warm_boot_degraded`), checking `warm_boot_degraded`
    // alone is a strict superset of the old condition — no case that used to
    // report `"degraded"` stops doing so.
    // #4680: a daemon holding indexes that owe lexical work but have never been
    // walked is not healthy — that is a silent, total search outage for every
    // such index and it was previously invisible on every field of this
    // response. Same `status != "ok"` channel as #1870 / #3408.
    // #4125: an embedder that permanently failed to reach the backend it was
    // configured for (or fell back off it) is the same class of capability gap
    // as #3408's refused watcher — see `embedder_capability_degraded` above.
    // #5336: an abandoned mid-walk index is the same outage as #4680's
    // never-walked one — the lexical lane serves nothing and no one is coming to
    // fix it — so it rides the same channel.
    let overall_status = if warmboot_summary.warm_boot_degraded
        || indexes_watcher_network_degraded > 0
        || indexes_stuck_empty > 0
        || indexes_stuck_mid_walk > 0
        || embedder_capability_degraded
    {
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
        // #4390: honour the claim `deferred_embed_queue_depth`'s doc already made.
        deferred_embed_queue_depth: crate::service::reindex::deferred_embed_queue_depth(),
        update_available,
        indexes_kg_disabled,
        indexes_vector_disabled,
        indexes_component_catch_up_in_progress,
        indexes_embed_pool_missing,
        indexes_stuck_empty,
        // #5336: the abandoned-mid-walk complement of the counter above.
        indexes_stuck_mid_walk,
        // #4839: registered vs. populated, plus the fleet-wide corpus size.
        indexes_populated,
        indexes_empty,
        total_chunks,
        warmboot_summary,
        // #6688: which indexes the counter above is about.
        indexes_stage_failed_ids,
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
/// degraded_by_unapproved || degraded_by_timeout || degraded_by_count ||
/// any_stage_failed`, where the
/// first three reuse the ORIGINAL boot-time counters (`indexes_skipped_tcc`,
/// `indexes_skipped_unapproved` — #5926, a boot-time allowlist exclusion that
/// nothing at runtime can heal, for the same reason the TCC counter cannot —
/// `indexes_skipped_timeout`, and a fresh `registry.list_handles().len()`
/// vs. `prior_index_count` comparison — live rather than the frozen
/// boot-time `total`, since new indexes can register between boot and
/// drain) and `any_stage_failed` is a live scan for any handle with
/// `stages.any_failed() == true` (the identical predicate `/health` already
/// uses for `indexes_stage_failed`). Persists the result back into
/// `state.warmboot_summary` (not just a per-request clone) and logs the
/// old -> new transition.
///
/// Returns whether the scan was CONCLUSIVE — i.e. every handle's `stages` was
/// readable (#5633). A contended handle proves nothing about that index, so an
/// inconclusive scan preserves the previous flag instead of clearing it, and the
/// caller leaves the drain edge armed so the next poll re-derives.
/// Test: `warm_boot_degraded_recomputes_to_false_once_catch_up_drains_cleanly`,
/// `warm_boot_degraded_recompute_keeps_a_genuinely_failed_embed_degraded`,
/// `recompute_does_not_flag_an_in_progress_tail_job_as_degraded`,
/// `recompute_does_not_clear_degraded_when_a_stages_read_is_contended`,
/// `recompute_reports_whether_its_scan_was_conclusive` in
/// `tests_health_degraded.rs`.
pub(super) fn recompute_warm_boot_degraded(state: &SearchAppState) -> bool {
    // #5926: `indexes_skipped_unapproved` is read alongside the TCC counter and
    // for the same reason — both are boot-time facts that a deferred-embed drain
    // cannot heal. An index the allowlist excluded never entered the registry,
    // so nothing this recompute scans can observe it; without this term the
    // first drain after boot would clear a degraded signal while 103 registered
    // indexes stayed unserved.
    let (degraded_by_tcc, degraded_by_unapproved, old) = match state.warmboot_summary.lock() {
        Ok(summary) => (
            summary.indexes_skipped_tcc > 0,
            summary.indexes_skipped_unapproved > 0,
            summary.warm_boot_degraded,
        ),
        Err(_) => return false,
    };
    // #4250: this used to read the FROZEN boot-time `indexes_skipped_timeout`,
    // on the stated premise that "a scan timeout genuinely CANNOT heal without
    // a daemon restart (the index in question never loaded at all)". That
    // premise held only because nothing retried a timed-out index; the
    // recovery pass makes it false in both directions. Reading the LIVE cohort
    // means a daemon that got its indexes back stops reporting degraded, and —
    // the direction that matters more — an index that never came back keeps a
    // signal of its own instead of being averaged away by a boot counter.
    let degraded_by_timeout = state.cold_store.timed_out_len() > 0;

    let prior_count = state
        .prior_index_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let handles = state.registry.list_handles();
    let degraded_by_count = prior_count > 0 && handles.len() < prior_count * 4 / 5;
    // #5633: separate what the scan PROVED from what it could not read. The
    // `/health` poll above folds a contended `try_read` into "did not fail" and
    // says why that is safe there — "the next 2 s poll re-scans". Nothing
    // re-scans behind THIS one: it is edge-triggered on the catch-up queue's
    // drain and it WRITES the sticky flag, so the same undercount clears a real
    // degraded signal until the daemon restarts. tokio's `RwLock` is fair, so a
    // merely QUEUED writer already fails `try_read` — and `reindex/defer_embed.rs`
    // and `reindex/stages.rs` take `stages.write()` on exactly the handles this
    // recompute's own trigger has been moving.
    let mut any_stage_failed = false;
    let mut unreadable = 0usize;
    for handle in handles.iter() {
        match handle.stages.try_read() {
            Ok(stages) => any_stage_failed |= stages.any_failed(),
            Err(_) => unreadable += 1,
        }
    }
    let conclusive = unreadable == 0;
    // An unreadable handle blocks only the CLEARING of the flag — the direction
    // that discards evidence. It never manufactures a failure: a clean scan
    // still clears, and a proven failure still sets. Defaulting the other way
    // would be this same defect with the sign flipped.
    let new = degraded_by_tcc
        || degraded_by_unapproved
        || degraded_by_timeout
        || degraded_by_count
        || any_stage_failed
        || (!conclusive && old);

    if let Ok(mut summary) = state.warmboot_summary.lock() {
        summary.warm_boot_degraded = new;
    }

    // Report WHY, not just the value: "nothing failed" and "N handles could not
    // be read" must not reach an operator as the same line.
    if !conclusive {
        tracing::warn!(
            unreadable,
            handles = handles.len(),
            warm_boot_degraded = new,
            "warm_boot_degraded recompute could not read {unreadable} handle(s)' stages \
             (lock contended) — the scan proves nothing about those indexes, so the \
             previous value is preserved rather than cleared, and the drain edge stays \
             armed for the next poll (#5633)"
        );
    } else if new != old {
        tracing::info!(
            "warm_boot_degraded recompute (deferred-embed catch-up drained): {old} -> {new}"
        );
    } else {
        tracing::debug!(
            "warm_boot_degraded recompute (deferred-embed catch-up drained): unchanged at {new}"
        );
    }
    conclusive
}

/// The `/health` body, for a caller that is not an axum route (#6285).
///
/// Why: `search.health` over the UDS socket and `GET /health` must never be
/// able to disagree. Both call [`health_handler`], so there is one probe
/// implementation and one report; this only strips the `Json` wrapper the
/// socket has no use for and serialises the result the socket's frame carries.
/// `HealthResponse` itself stays `pub(super)` — the socket needs the bytes,
/// not the type.
/// What: `health_handler(State(state))`, unwrapped and serialised.
/// Serialisation cannot fail for this type (no map with non-string keys, no
/// non-finite float), so the impossible branch reports itself rather than
/// panicking.
/// Test: `health_over_the_socket_matches_the_http_body`.
pub(crate) async fn health_report(state: Arc<SearchAppState>) -> serde_json::Value {
    let Json(resp) = health_handler(State(state)).await;
    serde_json::to_value(resp).unwrap_or_else(
        |e| serde_json::json!({ "status": "error", "error": format!("serialize health: {e}") }),
    )
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
