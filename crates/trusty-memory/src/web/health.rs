//! `GET /health` handler — liveness probe with store/recall smoke test.
//!
//! Why: Provides an unauthenticated round-trip check for operator and
//! orchestrator polling. Issues #35, #71, and #185 progressively enriched
//! the endpoint with metrics, round-trip semantics, and a dedicated probe
//! palace.
//! What: `health()` axum handler, `HealthResponse` wire struct,
//! `HealthProbeError`, `ensure_health_probe_palace`, `run_health_round_trip`,
//! and the testable `run_health_round_trip_inner` helper.
//! Test: `health_endpoint_*` and `health_probe_*` tests in
//! `web::tests::health_tests`.

use axum::{extract::State, Json};
use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::recall_with_default_embedder;
use uuid::Uuid;

use crate::AppState;

use super::HEALTH_PROBE_PALACE;

/// Liveness/version payload for `GET /health`.
///
/// Why: `daemon_probe` requires an HTTP 200 from `/health` to confirm that the
/// port is owned by this daemon (and not a stale or foreign process). Issue
/// #35 enriches it with process resource metrics so operators (and the admin
/// UI) can see RSS, disk footprint, CPU, and uptime in one cheap call.
/// The fd-exhaustion fix adds `open_fds` and `fd_soft_limit` so operators can
/// see "244 / 256" before EMFILE hits.
/// What: Carries a fixed `status` string, the compile-time crate version,
/// the issue-#35 resource block, and `open_fds` / `fd_soft_limit`.
/// Test: Asserted by `health_endpoint_returns_ok`,
/// `health_endpoint_includes_resource_fields`, and
/// `health_endpoint_includes_fd_gauge` in this module's tests.
#[derive(serde::Serialize)]
pub(super) struct HealthResponse {
    /// `"ok"` when the round-trip smoke test succeeds (or no palace exists
    /// yet), `"degraded"` when store/recall is broken (issue #71). Owned
    /// `String` so the handler can report different statuses without
    /// requiring static lifetimes.
    pub(super) status: String,
    /// Populated only when `status == "degraded"` (issue #71). Carries a
    /// short phrase identifying which round-trip stage failed so operators
    /// can triage quickly (e.g. `"store failed: ..."`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
    pub(super) version: &'static str,
    /// Current process Resident Set Size in megabytes (issue #35). Sampled
    /// via the shared `SysMetrics` on each health request.
    pub(super) rss_mb: u64,
    /// On-disk footprint of the daemon's `data_root` in bytes (issue #35):
    /// the sum of every palace file. Refreshed by a background task every
    /// 10 s; `0` until the first walk completes.
    pub(super) disk_bytes: u64,
    /// Current process CPU usage as a percentage (issue #35), where `100.0`
    /// means one fully-saturated core. The first reading after daemon start
    /// may be `0.0` until a delta window exists.
    pub(super) cpu_pct: f32,
    /// Seconds elapsed since the daemon started (issue #35).
    pub(super) uptime_secs: u64,
    /// Bound `host:port` of the HTTP listener. Why: dynamic port selection
    /// (7070..=7079 + OS fallback) means clients cannot assume `7070`; this
    /// field advertises the real port without forcing them to read
    /// `~/.trusty-memory/http_addr`. `None` when the daemon was constructed
    /// without ever binding (tests that drive the router with `TestServer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) addr: Option<String>,
    /// Number of file descriptors currently open by this process (fd-exhaustion
    /// gauge). `None` when the platform does not expose this cheaply (rare).
    /// Sampled on every `/health` call via [`crate::fd_metrics::count_open_fds`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) open_fds: Option<u64>,
    /// Soft `RLIMIT_NOFILE` ceiling for this process (fd-exhaustion gauge).
    /// `None` when `getrlimit` fails or returns `RLIM_INFINITY` (unlimited).
    /// Together with `open_fds`, lets operators see "244 / 256" before EMFILE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fd_soft_limit: Option<u64>,
    /// Newer crates.io version available, if any (issue #537).
    ///
    /// Why: surfaces update availability without polling crates.io on every
    /// health call — a single background check at startup stores the result
    /// here for the health handler to read cheaply.
    /// What: `null`/absent = up to date or check not completed; `"x.y.z"` =
    /// the available newer version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) update_available: Option<String>,
    /// Daemon readiness state (issues #910 / #911).
    ///
    /// Why: operators and monitoring scripts need to distinguish "the daemon
    /// is alive but the embedder hasn't finished compiling yet" from "the
    /// daemon is fully operational". Before this field, a fresh daemon looked
    /// healthy to external monitors even while `memory_remember` /
    /// `memory_recall` calls were returning warming errors.
    /// What: `"warming"` until the embedder init succeeds; `"ready"` once
    /// `spawn_startup_tasks` flips `AppState::daemon_readiness`.
    pub(super) daemon_state: String,
}

/// `GET /health` — unauthenticated liveness probe with store/recall smoke test.
///
/// Why: Gives `daemon_probe` and external monitors a cheap way to confirm port
/// ownership without touching palace state. Issue #35 additionally reports
/// process RSS, CPU, the `data_root` disk footprint, and uptime. Issue #71
/// upgrades the check to a full memory round-trip (store → recall → verify →
/// delete) so operators learn about store/recall regressions immediately
/// instead of after a real request fails. Issue #185 routes the round-trip
/// to a dedicated `__health_probe__` palace (hidden from user listings) so
/// the probe never leaks drawers into a real user palace even on recall
/// failures. The fd-exhaustion fix adds `open_fds` and `fd_soft_limit` so
/// operators can catch "approaching ceiling" before EMFILE hits.
/// What: Returns HTTP 200 with `{status, version, rss_mb, disk_bytes,
/// cpu_pct, uptime_secs, open_fds?, fd_soft_limit?, detail?}`. RSS + CPU are
/// sampled live; `disk_bytes` is read from the background ticker;
/// `uptime_secs` is elapsed since `state.started_at`; `open_fds` and
/// `fd_soft_limit` are sampled best-effort (absent when the platform does not
/// expose them cheaply). The handler provisions the dedicated probe palace if
/// missing and then attempts a full remember/recall/forget cycle — `status`
/// is `"ok"` on success, `"degraded"` with a `detail` string explaining the
/// failing stage otherwise. The probe never returns non-200 so monitors
/// keyed on HTTP status still see the daemon as up.
/// Test: `health_endpoint_returns_ok`,
/// `health_endpoint_includes_resource_fields`,
/// `health_endpoint_includes_fd_gauge`,
/// `health_endpoint_round_trip_on_fresh_install_is_ok`,
/// `health_endpoint_round_trip_with_palace_is_ok`,
/// `health_probe_palace_is_invisible`,
/// `health_probe_cleans_up_on_success`,
/// `health_probe_cleans_up_on_recall_miss`.
pub(super) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let (rss_mb, cpu_pct) = {
        let mut metrics = state.sys_metrics.lock().await;
        metrics.sample()
    };
    let disk_bytes = state.disk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let uptime_secs = state.started_at.elapsed().as_secs();
    let addr = state.bound_addr.get().map(|a| a.to_string());

    // fd-exhaustion gauge: sample best-effort; failures return None (not an
    // error so we do not have to import the fd_metrics crate in every test
    // that drives this handler via in-process TestServer).
    let open_fds = crate::fd_metrics::count_open_fds();
    let fd_soft_limit = crate::fd_metrics::fd_soft_limit();

    let (status, detail) = match run_health_round_trip(&state).await {
        Ok(()) => ("ok".to_string(), None),
        Err(err) => {
            tracing::warn!("/health round-trip degraded: {err}");
            ("degraded".to_string(), Some(err.to_string()))
        }
    };

    let update_available = state.update_available.lock().ok().and_then(|g| g.clone());
    // Issues #910/#911: surface readiness so monitors and Claude Code can
    // distinguish "alive but warming" from "fully ready".
    let daemon_state = match state.readiness() {
        crate::DaemonReadiness::Warming => "warming",
        crate::DaemonReadiness::Ready => "ready",
    }
    .to_string();

    Json(HealthResponse {
        status,
        detail,
        version: env!("CARGO_PKG_VERSION"),
        rss_mb,
        disk_bytes,
        cpu_pct,
        uptime_secs,
        addr,
        open_fds,
        fd_soft_limit,
        update_available,
        daemon_state,
    })
}

/// Stages of the `/health` round-trip that can fail (issue #71).
///
/// Why: `thiserror`-derived enum gives every failure point a stable phrase the
/// handler can render into the `detail` field without printing implementation
/// detail or full backtraces. Issue #185 dropped the `NoPalaces` and
/// `ListPalaces` sentinels: the probe now provisions its dedicated
/// `__health_probe__` palace itself, so neither short-circuit can occur.
/// What: One variant per stage (open palace, ensure-probe-palace, store,
/// recall, missing-in-results, delete).
/// Test: Exercised indirectly by the `health_endpoint_round_trip_*` and
/// `health_probe_*` tests.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HealthProbeError {
    #[error("open palace failed: {0}")]
    OpenPalace(String),
    #[error("provision health probe palace failed: {0}")]
    EnsureProbePalace(String),
    #[error("store failed: {0}")]
    Store(String),
    #[error("recall failed: {0}")]
    Recall(String),
    #[error("recall did not return the probe drawer (id={0})")]
    ProbeMissing(Uuid),
    #[error("delete probe drawer failed: {0}")]
    Delete(String),
}

/// Ensure the dedicated `__health_probe__` palace exists on disk.
///
/// Why: Issue #185 — picking whichever palace `list_palaces` returns first
/// leaked health-probe drawers into a real user palace whenever recall failed
/// or returned an empty result. Routing the probe to a dedicated palace whose
/// id starts with the reserved `__` prefix confines any leak (e.g. a daemon
/// crash mid-round-trip) to a palace the user can never see. This helper is
/// idempotent: it is safe to call on every `/health` request, even when the
/// palace already exists.
/// What: Calls `PalaceRegistry::open_palace` first (cheap cache hit when the
/// palace is already registered). If the palace metadata is missing on disk,
/// creates it via `PalaceRegistry::create_palace` with a description that
/// flags its purpose. Either path returns success when the palace is ready
/// for the round-trip; failures propagate as `HealthProbeError::EnsureProbePalace`.
/// Test: `health_probe_palace_is_invisible`, `health_probe_cleans_up_on_success`,
/// `health_probe_cleans_up_on_recall_miss`.
pub(crate) fn ensure_health_probe_palace(state: &AppState) -> Result<(), HealthProbeError> {
    let id = PalaceId::new(HEALTH_PROBE_PALACE);

    // Fast path: already registered in-memory, no disk hit needed.
    if state.registry.get(&id).is_some() {
        return Ok(());
    }

    // Try to open from disk first — succeeds on every request after the
    // first one once the palace has been persisted.
    if state.registry.open_palace(&state.data_root, &id).is_ok() {
        return Ok(());
    }

    // Cold path: first run on this `data_root`. Create the palace metadata
    // on disk so subsequent probes hit the open-path above.
    let palace = Palace {
        id: id.clone(),
        name: HEALTH_PROBE_PALACE.to_string(),
        description: Some(
            "Internal health-probe palace (issue #185). Hidden from listings; \
             holds short-lived round-trip drawers cleaned up on every probe."
                .to_string(),
        ),
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(HEALTH_PROBE_PALACE),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .map_err(|e| HealthProbeError::EnsureProbePalace(format!("{e:#}")))?;
    Ok(())
}

/// Execute a remember/recall/forget cycle against the dedicated probe palace.
///
/// Why: `/health` used to return `status: "ok"` even when `POST /drawers` or
/// the recall path was broken — only that the process was alive. Issue #71
/// asks the probe to actually exercise the store and recall service layer
/// (no HTTP loopback) so monitors detect data-plane regressions on the next
/// poll instead of waiting for a real client to surface them. Issue #185
/// additionally requires the probe to (a) never touch user-facing palaces and
/// (b) never leak drawers even when recall fails or returns an empty result.
/// What: Provisions the dedicated `__health_probe__` palace via
/// [`ensure_health_probe_palace`], opens its handle, stores a content-unique
/// probe drawer via `PalaceHandle::remember`, runs
/// `recall_with_default_embedder` with the probe phrase, and then **always**
/// attempts `PalaceHandle::forget` *before* propagating any recall error so a
/// failing recall (Err *or* empty result) can never leave a drawer behind.
/// The probe palace is hidden from `MemoryService::list_palaces`, so any rare
/// leak (e.g. mid-call daemon crash) is confined to a palace the user can't see.
/// Test: Indirect — `health_endpoint_round_trip_with_palace_is_ok`,
/// `health_endpoint_round_trip_on_fresh_install_is_ok`, plus the three
/// `health_probe_*` cleanup tests added for issue #185.
pub(crate) async fn run_health_round_trip(state: &AppState) -> Result<(), HealthProbeError> {
    // Issue #185: always use the dedicated probe palace. Provision it on the
    // first request so a fresh install with zero user palaces still exercises
    // the full data plane — no more `NoPalaces` short-circuit.
    ensure_health_probe_palace(state)?;
    let probe_id = PalaceId::new(HEALTH_PROBE_PALACE);
    let handle = state
        .registry
        .open_palace(&state.data_root, &probe_id)
        .map_err(|e| HealthProbeError::OpenPalace(format!("{e:#}")))?;

    // Delegate the cleanup-ordering logic to the testable helper so unit tests
    // can substitute the recall implementation. The real handler always uses
    // the shared ONNX embedder.
    run_health_round_trip_inner(handle, |handle, query| async move {
        recall_with_default_embedder(&handle, &query, 5)
            .await
            .map_err(|e| HealthProbeError::Recall(format!("{e:#}")))
    })
    .await
}

/// Store-recall-forget core that always cleans up the probe drawer.
///
/// Why: Issue #185 — the cleanup invariant ("the probe drawer is always
/// deleted before any error returns") is the central correctness property of
/// the health round-trip. Splitting it out from `run_health_round_trip` lets
/// the tests inject a recall stub that returns `Ok(empty)` or
/// `Err(Recall(...))` and prove the invariant directly, without relying on
/// the ONNX embedder.
/// What: Stores a content-unique probe drawer via `PalaceHandle::remember`,
/// invokes `recall` with the probe phrase, and then **always** calls
/// `PalaceHandle::forget` *before* propagating any recall error. The recall
/// result is evaluated after the forget so a missing or errored recall can
/// never leave a drawer behind. Cleanup errors are reported only when recall
/// succeeded; otherwise the upstream recall failure is preserved as the root
/// cause for operators.
/// Test: `health_probe_cleans_up_on_recall_miss` and
/// `health_probe_cleans_up_on_recall_error` exercise both failure modes with
/// a stubbed recall; `health_probe_cleans_up_on_success` covers the happy path.
pub(crate) async fn run_health_round_trip_inner<F, Fut>(
    handle: std::sync::Arc<trusty_common::memory_core::PalaceHandle>,
    recall: F,
) -> Result<(), HealthProbeError>
where
    F: FnOnce(std::sync::Arc<trusty_common::memory_core::PalaceHandle>, String) -> Fut,
    Fut: std::future::Future<
        Output = Result<Vec<trusty_common::memory_core::retrieval::RecallResult>, HealthProbeError>,
    >,
{
    // Content-unique probe phrase. `__trusty_memory_healthcheck__` makes the
    // probe identifiable in logs / drawer dumps if a forget step is ever
    // skipped (e.g. handler panic between store and delete); the UUID
    // guarantees uniqueness across concurrent probes.
    let probe_token = Uuid::new_v4();
    let probe_content = format!("__trusty_memory_healthcheck__ probe {probe_token}");

    let drawer_id = handle
        .remember(
            probe_content.clone(),
            RoomType::General,
            vec!["healthcheck".to_string()],
            0.0,
        )
        .await
        .map_err(|e| HealthProbeError::Store(format!("{e:#}")))?;

    let recall_result = recall(handle.clone(), probe_content).await;

    // Issue #185: cleanup runs BEFORE we propagate any recall error so the
    // probe can never leave a drawer behind. Both the Err and the
    // empty-result failure modes used to bypass forget; this ordering closes
    // both holes. Cleanup errors are surfaced only when the recall path
    // itself succeeded; otherwise we preserve the upstream recall failure as
    // the root cause for operators.
    let delete_result = handle.forget(drawer_id).await;

    match recall_result {
        Ok(hits) => {
            if !hits.iter().any(|hit| hit.drawer.id == drawer_id) {
                return Err(HealthProbeError::ProbeMissing(drawer_id));
            }
        }
        Err(e) => return Err(e),
    }

    delete_result.map_err(|e| HealthProbeError::Delete(format!("{e:#}")))?;
    Ok(())
}
