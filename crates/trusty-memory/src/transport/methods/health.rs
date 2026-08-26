//! `memory.health` — liveness probe with an optional store/recall smoke test.
//!
//! Why: Provides an unauthenticated round-trip check for operator and
//! orchestrator polling. Issues #35, #71, and #185 progressively enriched
//! the endpoint with metrics, round-trip semantics, and a dedicated probe
//! palace. Issue #1101 makes the expensive ONNX round-trip opt-in (via
//! `?probe=true`) so the default path remains cheap enough for 1 s LB polling.
//! Issue #1142 adds self-healing: `ensure_health_probe_palace` now seeds a
//! persistent sentinel drawer when the probe palace is empty (e.g. after a
//! redb v2→v3 migration wipes the vector store) so the palace re-populates
//! itself on the next deep-probe request without operator intervention.
//! Issue #6217 routes `PalaceHandle::drawer_load_degraded` out through the
//! payload so a partial drawer corpus is readable by a caller, not only in
//! logs.
//! Issue #6286 moved this off axum: the handler takes `(&AppState,
//! HealthQuery)` and the `?probe=true` query string is now a `params` field.
//! What: `health()`, `HealthQuery`, `HealthResponse`, `HealthProbeError`,
//! `ensure_health_probe_palace`, `run_health_round_trip`, and the testable
//! `run_health_round_trip_inner` helper.
//! Test: `rpc_health_*` in `crate::transport::uds::tests`.

use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::recall_with_default_embedder;
use uuid::Uuid;

/// Persistent content stored in the probe palace as an always-present
/// sentinel (issue #1142). A migration (e.g. redb v2→v3) may wipe the
/// vector store but leave the palace directory intact. The sentinel is
/// re-seeded automatically on the first deep probe after such an event.
///
/// Why: gives `ensure_health_probe_palace` a drawer it can check for
/// existence to determine whether the palace data was lost — and, if lost,
/// to re-plant it so the next probe round-trip has a healthy baseline.
/// What: a fixed string with a well-known prefix (`PROBE_SENTINEL_PREFIX`)
/// that is recognisable in drawer dumps / logs. `seed_probe_sentinel_if_absent`
/// matches by prefix rather than exact equality so future versions can append
/// a version tag without breaking the self-heal check (issue #1156).
/// Test: `health_probe_self_heals_after_migration_wipe` (issue #1142),
/// `health_sentinel_prefix_match_is_robust` (issue #1156).
pub(crate) const PROBE_SENTINEL_CONTENT: &str =
    "__trusty_memory_health_sentinel__ issue-#1142 self-heal probe";

/// Prefix used by `seed_probe_sentinel_if_absent` to identify sentinel drawers.
///
/// Why: Issue #1156 — matching sentinel drawers by the exact `PROBE_SENTINEL_CONTENT`
/// string is brittle: any future version bump in the content (e.g. adding a
/// version tag) would cause the old sentinel to be invisible to the check, forcing
/// an unnecessary re-seed cycle. A prefix match decouples detection from the
/// full literal so older sentinels remain recognisable even after content evolution.
/// What: The leading token `"__trusty_memory_health_sentinel__"` is stable across
/// versions and uniquely identifies health-probe sentinel drawers.
/// Test: `health_sentinel_prefix_match_is_robust` in `web::tests::health_tests`.
pub(crate) const PROBE_SENTINEL_PREFIX: &str = "__trusty_memory_health_sentinel__";

use crate::AppState;

use super::{to_value, HEALTH_PROBE_PALACE};
use crate::transport::api_error::ApiError;

/// Query parameters for `GET /health`.
///
/// Why (issue #1101): the default `/health` path must be cheap enough for
/// 1-second load-balancer polling. The expensive remember/recall/forget
/// round-trip (ONNX embedder calls) is now opt-in: callers that genuinely
/// want to probe the data plane pass `?probe=true` (or `?deep=true` for
/// symmetry with other endpoints).
/// What: both `probe` and `deep` default to `false`. When either is `true`
/// the handler runs the full `run_health_round_trip`; otherwise it returns
/// a lightweight liveness response without touching the memory store.
/// Test: `health_endpoint_cheap_by_default` and
/// `health_endpoint_probe_param_triggers_round_trip`.
#[derive(serde::Deserialize)]
pub struct HealthQuery {
    /// When `true`, run the full remember/recall/forget round-trip.
    #[serde(default)]
    probe: bool,
    /// Alias for `probe` (matches the `deep=` convention on other endpoints).
    #[serde(default)]
    deep: bool,
}

impl HealthQuery {
    /// Returns `true` if either the `probe` or `deep` flag is set.
    fn wants_deep_probe(&self) -> bool {
        self.probe || self.deep
    }
}

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
pub struct HealthResponse {
    /// `"ok"` when the round-trip smoke test succeeds (or no palace exists
    /// yet), `"degraded"` when store/recall is broken (issue #71). Owned
    /// `String` so the handler can report different statuses without
    /// requiring static lifetimes.
    pub status: String,
    /// Populated only when `status == "degraded"` (issue #71). Carries a
    /// short phrase identifying which round-trip stage failed so operators
    /// can triage quickly (e.g. `"store failed: ..."`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub version: &'static str,
    /// Current process Resident Set Size in megabytes (issue #35). Sampled
    /// via the shared `SysMetrics` on each health request.
    pub rss_mb: u64,
    /// On-disk footprint of the daemon's `data_root` in bytes (issue #35):
    /// the sum of every palace file. Refreshed by a background task every
    /// 10 s; `0` until the first walk completes.
    pub disk_bytes: u64,
    /// Current process CPU usage as a percentage (issue #35), where `100.0`
    /// means one fully-saturated core. The first reading after daemon start
    /// may be `0.0` until a delta window exists.
    pub cpu_pct: f32,
    /// Seconds elapsed since the daemon started (issue #35).
    pub uptime_secs: u64,
    /// The socket this daemon serves, as both it and its consumers resolve it
    /// (#6286). It replaced `addr`, which advertised a dynamically-chosen TCP
    /// port because a client could not assume 7070. A socket path is derived
    /// rather than chosen, so this field is confirmation rather than
    /// discovery. `None` only when the data directory cannot be resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
    /// Number of file descriptors currently open by this process (fd-exhaustion
    /// gauge). `None` when the platform does not expose this cheaply (rare).
    /// Sampled on every `/health` call via [`crate::fd_metrics::count_open_fds`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_fds: Option<u64>,
    /// Soft `RLIMIT_NOFILE` ceiling for this process (fd-exhaustion gauge).
    /// `None` when `getrlimit` fails or returns `RLIM_INFINITY` (unlimited).
    /// Together with `open_fds`, lets operators see "244 / 256" before EMFILE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fd_soft_limit: Option<u64>,
    /// Newer crates.io version available, if any (issue #537).
    ///
    /// Why: surfaces update availability without polling crates.io on every
    /// health call — a single background check at startup stores the result
    /// here for the health handler to read cheaply.
    /// What: `null`/absent = up to date or check not completed; `"x.y.z"` =
    /// the available newer version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_available: Option<String>,
    /// Daemon readiness state (issues #910 / #911).
    ///
    /// Why: operators and monitoring scripts need to distinguish "the daemon
    /// is alive but the embedder hasn't finished compiling yet" from "the
    /// daemon is fully operational". Before this field, a fresh daemon looked
    /// healthy to external monitors even while `memory_remember` /
    /// `memory_recall` calls were returning warming errors.
    /// What: `"warming"` until the embedder init succeeds; `"ready"` once
    /// `spawn_startup_tasks` flips `AppState::daemon_readiness`.
    pub daemon_state: String,
    /// Live worker-pool occupancy (issue #4001).
    ///
    /// Why: this is the field that makes `/health` report what it OBSERVED
    /// rather than what it assumed. Every other field describes the process;
    /// this one describes the work. Without it an out-of-process doctor has no
    /// way to tell a busy-but-healthy daemon from one whose every worker is
    /// parked, which is why #3992 read as HEALTHY for the whole incident.
    /// What: see [`WorkerHealth`].
    pub worker: WorkerHealth,
    /// Palaces that exist on disk but which startup hydration could not open,
    /// each with the reason (issue #4911).
    ///
    /// Why: a palace the daemon skips is absent from the handle cache and so is
    /// indistinguishable, from outside, from one that was never created —
    /// `palace_list` simply does not mention it. That is how a refused open
    /// reads as data loss to an operator even though the bytes are intact. The
    /// registry records every skip; this is the only place a human or a script
    /// can read that record back.
    /// What: `(palace_id, reason)` pairs from
    /// [`trusty_common::memory_core::PalaceRegistry::unopenable`]. Omitted
    /// entirely when empty, so a healthy daemon's payload is unchanged.
    /// Test: `health_reports_unopenable_palaces`,
    /// `health_omits_unopenable_palaces_when_none`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unopenable_palaces: Vec<UnopenablePalace>,
    /// Ids of currently-open palaces serving a partial drawer corpus
    /// (issue #6217).
    ///
    /// Why: `PalaceHandle::open_with_intent` fails open on drawer-load trouble
    /// (#6201) — a corrupt `DRAWERS` table or an unreadable row does not stop
    /// the palace opening, it just yields fewer drawers than the palace holds.
    /// #6201 recorded that on the handle as `drawer_load_degraded` but wired it
    /// to nothing, so the only trace an operator got was a `warn!` line at open
    /// time. Recall keeps answering, with silently missing corpus. This field is
    /// the API surface for that state.
    /// What: palace ids read from the handle cache via
    /// [`trusty_common::memory_core::PalaceRegistry::peek`], sorted, and omitted
    /// entirely when none are degraded so a healthy payload is unchanged. It
    /// deliberately leaves `status` at `"ok"`: a partial corpus is durable state
    /// an operator must repair, not a live outage a restart can clear, and
    /// `status: "degraded"` already means "the round-trip probe just failed".
    /// Cache-only, so a degraded palace that has been idle-evicted is absent
    /// until the next access re-opens it — the price of a `/health` that does no
    /// I/O.
    /// Test: `health_reports_drawer_degraded_palace`,
    /// `health_omits_drawer_degraded_when_all_healthy`,
    /// `health_drawer_degraded_names_only_the_degraded_palace`,
    /// `health_drawer_degraded_check_opens_no_palace`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drawer_degraded_palaces: Vec<String>,
}

/// One palace the daemon has on disk but could not open (issue #4911).
///
/// Why: a bare id would tell an operator which palace is missing but not what
/// to do about it — "incompatible on-disk format" and "too many open files"
/// call for opposite responses, and the reason is what separates them.
/// What: the palace id and the formatted open error the hydration walk
/// recorded.
/// Test: `health_reports_unopenable_palaces`.
#[derive(serde::Serialize)]
pub struct UnopenablePalace {
    /// The palace id as it appears on disk and in `palace_list`.
    pub id: String,
    /// The formatted error from the failed open.
    pub reason: String,
}

/// Worker-pool occupancy block of the `/health` payload (issue #4001).
///
/// Why: doctor must be able to distinguish three states, not two — healthy,
/// wedged, and *unknown*. Reporting the raw `oldest_age_secs` alongside the
/// `wedged` verdict lets a consumer form its own opinion, and lets an operator
/// see a pool trending toward a wedge before it trips.
/// What: in-flight count, age of the oldest outstanding operation (absent when
/// idle), and the threshold-crossed verdict.
/// Test: `health_reports_idle_worker_pool`, `health_reports_wedged_worker_pool`.
#[derive(serde::Serialize)]
pub struct WorkerHealth {
    /// Operations currently inside the palace open path.
    pub in_flight: usize,
    /// Seconds the oldest outstanding operation has been running. Absent when
    /// nothing is in flight — an idle pool has no age to report, and reporting
    /// `0` would be indistinguishable from "a request just started".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_age_secs: Option<u64>,
    /// True when `oldest_age_secs` exceeds
    /// [`crate::worker_liveness::wedge_threshold`] — i.e. an operation has
    /// blown past the bound that was supposed to release it.
    pub wedged: bool,
}

/// `GET /health[?probe=true]` — unauthenticated liveness probe with optional
/// store/recall smoke test.
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
/// Issue #1101: the expensive ONNX round-trip is now OPT-IN. Pass `?probe=true`
/// (or `?deep=true`) to run the full store/recall/forget cycle and report
/// `"ok"` or `"degraded"`. Without that flag the handler returns
/// `status: "ok"` immediately after sampling the cheap resource metrics —
/// suitable for 1-second LB polling without ONNX overhead.
/// Issue #4911: `unopenable_palaces` lists palaces present on disk that startup
/// hydration refused, so a skipped palace is visible instead of merely absent.
/// Issue #6217: `drawer_degraded_palaces` lists open palaces whose drawer table
/// loaded only partially, so a corpus with holes is visible instead of merely
/// logged. `status` stays `"ok"` for it — see the field's own docs for why.
/// What: Returns HTTP 200 with `{status, version, rss_mb, disk_bytes,
/// cpu_pct, uptime_secs, open_fds?, fd_soft_limit?, detail?,
/// unopenable_palaces?, drawer_degraded_palaces?}`. Without
/// `?probe=true`, `status` is always `"ok"` (daemon is alive). With
/// `?probe=true`, `status` is `"ok"` or `"degraded"` based on the
/// remember/recall/forget cycle. The handler never returns non-200 so
/// monitors keyed on HTTP status still see the daemon as up.
/// Test: `health_endpoint_returns_ok`,
/// `health_endpoint_includes_resource_fields`,
/// `health_endpoint_includes_fd_gauge`,
/// `health_endpoint_cheap_by_default`,
/// `health_endpoint_round_trip_on_fresh_install_is_ok`,
/// `health_endpoint_round_trip_with_palace_is_ok`,
/// `health_probe_palace_is_invisible`,
/// `health_probe_cleans_up_on_success`,
/// `health_probe_cleans_up_on_recall_miss`,
/// `health_reports_drawer_degraded_palace`,
/// `health_drawer_degraded_check_opens_no_palace`.
pub async fn health(state: &AppState, query: HealthQuery) -> Result<serde_json::Value, ApiError> {
    let (rss_mb, cpu_pct) = {
        let mut metrics = state.sys_metrics.lock().await;
        metrics.sample()
    };
    let disk_bytes = state.disk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let uptime_secs = state.started_at.elapsed().as_secs();
    let socket = crate::transport::uds::socket_path()
        .ok()
        .map(|p| p.display().to_string());

    // fd-exhaustion gauge: sample best-effort; failures return None (not an
    // error so we do not have to import the fd_metrics crate in every test
    // that drives this handler via in-process TestServer).
    let open_fds = crate::fd_metrics::count_open_fds();
    let fd_soft_limit = crate::fd_metrics::fd_soft_limit();

    // Issue #1101: the expensive ONNX round-trip only runs when the caller
    // explicitly requests it via ?probe=true or ?deep=true. Without either
    // flag the handler returns "ok" immediately — cheap enough for 1 s LB
    // polling without ONNX embedder calls.
    // Issue #4001: observe the worker pool BEFORE deciding the status. This is
    // the whole point of the fix — a listener that answers proves the process
    // is alive, not that work is moving, so the verdict must be derived from
    // an actual observation of outstanding work. Reading the gauge is a
    // handful of relaxed atomic loads; it adds no I/O and takes no lock, so it
    // is safe on the cheap (non-probe) path that monitors poll every second.
    let wedge_threshold = state.wedge_threshold;
    let oldest_age = state.worker_liveness.oldest_age();
    let worker = WorkerHealth {
        in_flight: state.worker_liveness.in_flight(),
        oldest_age_secs: oldest_age.map(|d| d.as_secs()),
        wedged: oldest_age.is_some_and(|age| age > wedge_threshold),
    };

    let (status, detail) = if query.wants_deep_probe() {
        match run_health_round_trip(&state).await {
            Ok(()) => ("ok".to_string(), None),
            Err(err) => {
                tracing::warn!("/health round-trip degraded: {err}");
                ("degraded".to_string(), Some(err.to_string()))
            }
        }
    } else {
        ("ok".to_string(), None)
    };

    // A wedged pool outranks whatever the round-trip concluded: if the oldest
    // in-flight operation has blown past its bound, the daemon is not healthy
    // no matter how cheerful the rest of the payload looks. Reported on the
    // cheap path too — #3992 was invisible precisely because nobody was
    // passing ?probe=true during the incident.
    let (status, detail) = if worker.wedged {
        let secs = worker.oldest_age_secs.unwrap_or_default();
        tracing::warn!(
            oldest_age_secs = secs,
            in_flight = worker.in_flight,
            "/health: worker pool appears wedged"
        );
        (
            "wedged".to_string(),
            Some(format!(
                "oldest in-flight palace operation has been running {secs}s \
                 (threshold {}s, {} in flight) — workers are not making progress",
                wedge_threshold.as_secs(),
                worker.in_flight
            )),
        )
    } else {
        (status, detail)
    };

    let update_available = state.update_available.lock().ok().and_then(|g| g.clone());
    // Issues #910/#911: surface readiness so monitors and Claude Code can
    // distinguish "alive but warming" from "fully ready".
    // #4836: report the embedder's real state, not the raw latch. The latch is
    // written only by the startup warm-up task, and the HTTP path never calls
    // `AppState::embedder`, so a daemon serving correct vector results could
    // report `warming` indefinitely — the exact misleading signal that made
    // #4836 hard to find (observed: uptime 24680 s, `daemon_state: warming`,
    // while `/recall` returned correctly ranked L2 hits).
    let daemon_state = match state.readiness() {
        crate::DaemonReadiness::Ready => "ready",
        crate::DaemonReadiness::Warming
            if trusty_common::memory_core::retrieval::shared_embedder_initialized() =>
        {
            "ready"
        }
        crate::DaemonReadiness::Warming => "warming",
    }
    .to_string();

    // #4911: report palaces present on disk that hydration refused, so a
    // skipped palace is visible rather than merely absent.
    let mut unopenable_palaces: Vec<UnopenablePalace> = state
        .registry
        .unopenable()
        .into_iter()
        .map(|(id, reason)| UnopenablePalace {
            id: id.as_str().to_string(),
            reason,
        })
        .collect();
    unopenable_palaces.sort_by(|a, b| a.id.cmp(&b.id));

    // #6217: a palace whose drawer table loaded only partially is serving a
    // corpus with holes, and until now said so nowhere but a startup warn line.
    // `list` + `peek` are both cache-only — a `parking_lot::Mutex` lock and an
    // `Arc` clone, no disk access and no LRU promotion — so this stays safe on
    // the cheap path monitors poll every second. The cost is that an
    // idle-evicted palace drops out of the report until it is next opened;
    // reporting it would mean opening it, and `/health` must not do I/O.
    let mut drawer_degraded_palaces: Vec<String> = state
        .registry
        .list()
        .into_iter()
        .filter(|id| {
            state
                .registry
                .peek(id)
                .is_some_and(|handle| handle.drawer_load_degraded)
        })
        .map(|id| id.as_str().to_string())
        .collect();
    drawer_degraded_palaces.sort();
    if !drawer_degraded_palaces.is_empty() {
        tracing::warn!(
            palaces = ?drawer_degraded_palaces,
            "/health: open palaces are serving a partial drawer corpus"
        );
    }

    to_value(HealthResponse {
        status,
        detail,
        version: env!("CARGO_PKG_VERSION"),
        rss_mb,
        disk_bytes,
        cpu_pct,
        uptime_secs,
        socket,
        open_fds,
        fd_soft_limit,
        update_available,
        daemon_state,
        worker,
        unopenable_palaces,
        drawer_degraded_palaces,
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
/// `health_probe_cleans_up_on_recall_miss`,
/// `health_probe_self_heals_after_migration_wipe` (issue #1142).
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

/// Seed or re-seed the persistent sentinel drawer in the probe palace.
///
/// Why (issue #1142): after a redb v2→v3 migration the probe palace
/// directory and `palace.json` survive but the internal vector/drawer stores
/// are wiped. The first deep-probe after migration finds an empty palace,
/// stores an ephemeral probe drawer, then runs recall — but the vector index
/// was just reset and may not return the just-stored item, producing a
/// spurious `ProbeMissing` on every probe. The fix: seed a *persistent*
/// sentinel drawer that outlives ephemeral round-trip drawers. On the first
/// deep probe after any migration event, if the sentinel is absent the
/// current call seeds it and returns `Ok(())` immediately (skipping the
/// full round-trip) — the palace is healthy, it just lost its sentinel.
/// On the next probe the sentinel will be present and the normal round-trip
/// executes.
/// What: Checks `handle.drawers` for [`PROBE_SENTINEL_CONTENT`]. If absent,
/// calls `handle.remember_with_options` with `force = true` to bypass the
/// token-length gate and store the sentinel. Returns `true` when seeding
/// occurred (caller should skip the normal round-trip for this request to
/// avoid a false ProbeMissing from the freshly-seeded vector), `false` when
/// the sentinel was already present.
/// Test: `health_probe_self_heals_after_migration_wipe` (issue #1142).
pub(crate) async fn seed_probe_sentinel_if_absent(
    handle: &std::sync::Arc<trusty_common::memory_core::PalaceHandle>,
) -> Result<bool, HealthProbeError> {
    // Issue #1156: use a prefix match rather than exact equality so that
    // future content changes (e.g. appending a version tag) don't make older
    // sentinel drawers invisible to this check. The prefix `PROBE_SENTINEL_PREFIX`
    // is stable across versions and is unique enough to identify health sentinels.
    let sentinel_present = handle
        .drawers
        .read()
        .iter()
        .any(|d| d.content().starts_with(PROBE_SENTINEL_PREFIX));

    if sentinel_present {
        return Ok(false);
    }

    use trusty_common::memory_core::retrieval::RememberOptions;
    handle
        .remember_with_options(
            PROBE_SENTINEL_CONTENT.to_string(),
            RoomType::General,
            vec!["healthcheck".to_string(), "sentinel".to_string()],
            0.0,
            RememberOptions {
                force: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| HealthProbeError::EnsureProbePalace(format!("seed sentinel: {e:#}")))?;
    // Issue #1156: include a structured `self_heal = true` field so log
    // aggregators can alert on self-heal events without string-parsing the
    // message (e.g. `filter[self_heal=true]` in a tracing subscriber query).
    tracing::info!(
        palace = HEALTH_PROBE_PALACE,
        self_heal = true,
        "health probe: seeded sentinel drawer (issue #1142 self-heal)"
    );
    Ok(true)
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

    // Issue #1142: self-heal the sentinel when the palace is empty (e.g. after
    // a redb migration wipes the vector/drawer stores). If the sentinel was
    // absent and we just seeded it, skip the normal round-trip for THIS request
    // — the vector index on a just-seeded single item may not return it yet,
    // and the palace is clearly healthy (remember just succeeded). The next
    // probe will find the sentinel and exercise the full round-trip.
    if seed_probe_sentinel_if_absent(&handle).await? {
        return Ok(());
    }

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
