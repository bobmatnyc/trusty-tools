//! Warm every registered index ahead of an all-index search (#9027).
//!
//! Why: the idle-eviction ticker drops an index's chunk map and BM25 corpus
//! after 300 s idle, and the first all-index search afterwards paid for every
//! evicted index (~26 s measured). A user about to search across all projects
//! can warm them first, as a background job, and keep them resident for a
//! window. This replaces the deferred #1105 "opt-in load-all on global search":
//! the fan-out stays hot-only and deadline-bounded; loading cold-parked indexes
//! is this job's, through the same lazy loader `search` uses.
//! What: [`WarmTracker`] (one run at a time, per-index progress, residency
//! pins), [`warm_start_report`] (`POST /warm`, `search.warm.start`) and
//! [`warm_status_report`] (`GET /warm/status`, `search.warm.status`).
//! Test: `warm_all_tests.rs`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::state::SearchAppState;
use crate::core::registry::IndexId;

/// Env var: how long a warmed index stays resident, in seconds.
pub(crate) const WARM_WINDOW_ENV: &str = "TRUSTY_WARM_ALL_WINDOW_SECS";
/// Default residency window: 30 minutes.
pub(crate) const DEFAULT_WARM_WINDOW_SECS: u64 = 1_800;
/// Env var: how many indexes warm at once.
pub(crate) const WARM_CONCURRENCY_ENV: &str = "TRUSTY_WARM_ALL_CONCURRENCY";
/// Default warm concurrency. A rehydrate is an O(corpus) redb scan plus a BM25
/// rebuild; four keeps the disk busy without starving interactive searches.
pub(crate) const DEFAULT_WARM_CONCURRENCY: usize = 4;
/// Env var: RSS ceiling in MB; warming stops starting new indexes past it.
/// Unset falls back to the daemon's `TRUSTY_MEMORY_LIMIT_MB`.
pub(crate) const WARM_MAX_RSS_ENV: &str = "TRUSTY_WARM_ALL_MAX_RSS_MB";
/// Env var: per-index warm timeout, in seconds.
pub(crate) const WARM_INDEX_TIMEOUT_ENV: &str = "TRUSTY_WARM_ALL_INDEX_TIMEOUT_SECS";
/// Default per-index warm timeout: 10 minutes.
pub(crate) const DEFAULT_WARM_INDEX_TIMEOUT_SECS: u64 = 600;

/// A positive integer env var, or `None`.
fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
}

/// Body of `POST /warm` / params of `search.warm.start`. Both fields optional;
/// an absent body is the env/default configuration.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmStartRequest {
    /// Residency window override, seconds (`> 0`).
    #[serde(default)]
    pub window_secs: Option<u64>,
    /// Warm concurrency override (clamped to `>= 1`).
    #[serde(default)]
    pub concurrency: Option<usize>,
}

/// One run's resolved configuration.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WarmConfig {
    pub(crate) window: Duration,
    pub(crate) concurrency: usize,
    pub(crate) index_timeout: Duration,
    pub(crate) max_rss_mb: Option<u64>,
}

impl WarmConfig {
    /// Request override, then env var, then default, per field.
    ///
    /// Test: `warm_config_prefers_the_request_over_the_defaults`.
    pub(crate) fn resolve(req: &WarmStartRequest) -> Self {
        let window = req
            .window_secs
            .filter(|&s| s > 0)
            .or_else(|| env_u64(WARM_WINDOW_ENV))
            .unwrap_or(DEFAULT_WARM_WINDOW_SECS);
        let concurrency = req
            .concurrency
            .or_else(|| env_u64(WARM_CONCURRENCY_ENV).map(|n| n as usize))
            .unwrap_or(DEFAULT_WARM_CONCURRENCY)
            .max(1);
        let timeout = env_u64(WARM_INDEX_TIMEOUT_ENV).unwrap_or(DEFAULT_WARM_INDEX_TIMEOUT_SECS);
        Self {
            window: Duration::from_secs(window),
            concurrency,
            index_timeout: Duration::from_secs(timeout),
            max_rss_mb: env_u64(WARM_MAX_RSS_ENV).or_else(crate::core::memguard::memory_limit_mb),
        }
    }
}

/// One index's warm state as the status route reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmState {
    /// Evicted, cold-parked, or not yet reached by the running warm.
    Cold,
    /// The running warm is rehydrating it now.
    Warming,
    /// Resident: corpus caches in memory.
    Warm,
    /// The warm for this index failed; `error` says why.
    Failed,
}

#[derive(Debug, Clone)]
struct Progress {
    state: WarmState,
    error: Option<String>,
}

#[derive(Debug)]
struct WarmRun {
    run_id: u64,
    running: bool,
    started_unix_ms: u64,
    finished_unix_ms: Option<u64>,
    config: WarmConfig,
    indexes: BTreeMap<String, Progress>,
    rss_mb_before: Option<u64>,
    rss_mb_after: Option<u64>,
    ceiling_hit: bool,
}

#[derive(Default)]
struct Inner {
    run: Option<WarmRun>,
    last_run_id: u64,
    /// Index id → the instant its residency pin expires.
    pins: HashMap<String, tokio::time::Instant>,
}

/// The daemon's one warm-all job and the residency pins it grants.
///
/// Why: a second start while a warm runs must join it, the status route must
/// read the same progress the runner writes, and the idle-eviction and
/// residency tickers must honour the window — one shared object on
/// [`SearchAppState`].
/// What: a std `Mutex` (never held across an await) over the current run and
/// the pins, plus the RSS probe (swappable in tests).
/// Test: `warm_all_tests.rs`.
pub struct WarmTracker {
    inner: Mutex<Inner>,
    rss_probe: Mutex<fn() -> Option<u64>>,
}

impl Default for WarmTracker {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            rss_probe: Mutex::new(crate::core::memguard::current_rss_mb),
        }
    }
}

impl WarmTracker {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn rss_mb(&self) -> Option<u64> {
        (self.rss_probe.lock().unwrap_or_else(|e| e.into_inner()))()
    }

    /// Replace the RSS probe (tests only).
    #[cfg(test)]
    pub(crate) fn set_rss_probe(&self, probe: fn() -> Option<u64>) {
        *self.rss_probe.lock().unwrap_or_else(|e| e.into_inner()) = probe;
    }

    /// Pin `id` for `window` without running a warm (tests only).
    #[cfg(test)]
    pub(crate) fn pin_for_test(&self, id: &str, window: Duration) {
        self.lock()
            .pins
            .insert(id.to_string(), tokio::time::Instant::now() + window);
    }

    /// `true` while `id`'s residency pin from a warm has not expired.
    ///
    /// Why: the idle-eviction and residency-cap tickers skip a pinned index, so
    /// a warmed index stays resident for the window instead of 300 s.
    /// Test: `a_warmed_index_is_pinned_for_the_window_then_released`.
    pub fn is_pinned(&self, id: &str) -> bool {
        self.lock()
            .pins
            .get(id)
            .is_some_and(|until| *until > tokio::time::Instant::now())
    }

    fn set(&self, run_id: u64, id: &str, state: WarmState, error: Option<String>) {
        let mut inner = self.lock();
        if let Some(run) = inner.run.as_mut().filter(|r| r.run_id == run_id) {
            run.indexes
                .insert(id.to_string(), Progress { state, error });
        }
    }

    fn mark_warm(&self, run_id: u64, id: &str, window: Duration) {
        let mut inner = self.lock();
        inner
            .pins
            .insert(id.to_string(), tokio::time::Instant::now() + window);
        if let Some(run) = inner.run.as_mut().filter(|r| r.run_id == run_id) {
            run.indexes.insert(
                id.to_string(),
                Progress {
                    state: WarmState::Warm,
                    error: None,
                },
            );
        }
    }

    fn note_ceiling_hit(&self, run_id: u64) {
        if let Some(run) = self.lock().run.as_mut().filter(|r| r.run_id == run_id) {
            run.ceiling_hit = true;
        }
    }

    /// End `run_id`. Fail-Open Check: an index still cold or warming when the
    /// run ends (the runner panicked or was cancelled) is marked failed, so an
    /// interrupted run never reads as a warm set.
    fn finish(&self, run_id: u64, rss_mb_after: Option<u64>) {
        let mut inner = self.lock();
        let Some(run) = inner.run.as_mut().filter(|r| r.run_id == run_id) else {
            return;
        };
        for progress in run.indexes.values_mut() {
            if matches!(progress.state, WarmState::Cold | WarmState::Warming) {
                progress.state = WarmState::Failed;
                progress.error = Some("the warm run ended before this index finished".into());
            }
        }
        run.running = false;
        run.finished_unix_ms = Some(unix_ms_now());
        run.rss_mb_after = rss_mb_after;
    }
}

fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Every index a warm covers: the hot registry plus pending cold-parked entries,
/// sorted. A permanently-failed restore is excluded — `/health` reports those.
fn warm_targets(state: &SearchAppState) -> Vec<String> {
    let mut ids: Vec<String> = state.registry.list().into_iter().map(|id| id.0).collect();
    ids.extend(state.cold_store.snapshot().into_iter().map(|e| e.id));
    ids.sort();
    ids.dedup();
    ids
}

/// `POST /warm` — start warming every registered index, or join the running warm.
pub(super) async fn warm_start_handler(
    State(state): State<Arc<SearchAppState>>,
    body: Option<Json<WarmStartRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    warm_start_report(&state, body.map(|Json(b)| b))
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// `GET /warm/status` — per-index warm state and totals.
pub(super) async fn warm_status_handler(State(state): State<Arc<SearchAppState>>) -> Json<Value> {
    Json(warm_status_report(&state))
}

/// The body `POST /warm` and `search.warm.start` serve.
///
/// Why: see the module doc. Returns at once; the work runs on a spawned task.
/// What: a running warm is joined (`joined: true`, same `run_id`). Otherwise
/// the RSS ceiling is checked — at or past it the start is refused with
/// `503 warm_memory_ceiling` and nothing runs — and a new run is spawned over
/// [`warm_targets`]. Must be called inside a tokio runtime.
/// Test: `a_second_start_joins_the_running_warm`,
/// `a_start_past_the_memory_ceiling_is_refused_and_runs_nothing`.
pub(crate) fn warm_start_report(
    state: &Arc<SearchAppState>,
    req: Option<WarmStartRequest>,
) -> Result<Value, (StatusCode, Value)> {
    start_with(state, WarmConfig::resolve(&req.unwrap_or_default()))
}

/// [`warm_start_report`] with an explicit configuration (the test seam).
pub(crate) fn start_with(
    state: &Arc<SearchAppState>,
    config: WarmConfig,
) -> Result<Value, (StatusCode, Value)> {
    let tracker = &state.warm;
    let mut inner = tracker.lock();
    if let Some(run) = inner.run.as_ref().filter(|r| r.running) {
        return Ok(start_body(run, true));
    }
    let rss_now = tracker.rss_mb();
    if let (Some(max), Some(now)) = (config.max_rss_mb, rss_now) {
        if now >= max {
            tracing::warn!("warm-all: refused — daemon RSS {now} MB is at the {max} MB ceiling");
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                    "error": "warm_memory_ceiling",
                    "retryable": true,
                    "rss_mb": now,
                    "max_rss_mb": max,
                    "message": format!(
                        "daemon RSS {now} MB is at or past the warm ceiling {max} MB \
                         ({WARM_MAX_RSS_ENV}); nothing was warmed"
                    ),
                }),
            ));
        }
    }
    let ids = warm_targets(state);
    inner.last_run_id += 1;
    let run = WarmRun {
        run_id: inner.last_run_id,
        running: true,
        started_unix_ms: unix_ms_now(),
        finished_unix_ms: None,
        config,
        indexes: ids
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    Progress {
                        state: WarmState::Cold,
                        error: None,
                    },
                )
            })
            .collect(),
        rss_mb_before: rss_now,
        rss_mb_after: None,
        ceiling_hit: false,
    };
    let body = start_body(&run, false);
    let run_id = run.run_id;
    inner.run = Some(run);
    drop(inner);

    let s = Arc::clone(state);
    tokio::spawn(async move {
        // The runner on its own task, so `finish` runs even if it panics.
        let worker = tokio::spawn(run_warm(Arc::clone(&s), run_id, ids, config));
        if let Err(e) = worker.await {
            tracing::error!("warm-all: run {run_id} ended abnormally: {e}");
        }
        let after = s.warm.rss_mb();
        s.warm.finish(run_id, after);
        tracing::info!("warm-all: run {run_id} finished; daemon RSS {after:?} MB");
    });
    Ok(body)
}

fn start_body(run: &WarmRun, joined: bool) -> Value {
    json!({
        "run_id": run.run_id,
        "joined": joined,
        "total": run.indexes.len(),
        "window_secs": run.config.window.as_secs(),
        "concurrency": run.config.concurrency,
        "status_route": "/warm/status",
    })
}

/// Warm every id with bounded concurrency; one failure never stops the rest.
async fn run_warm(state: Arc<SearchAppState>, run_id: u64, ids: Vec<String>, config: WarmConfig) {
    use futures::stream::StreamExt;
    futures::stream::iter(ids)
        .for_each_concurrent(config.concurrency, |id| {
            let state = Arc::clone(&state);
            async move { warm_one_tracked(&state, run_id, &id, config).await }
        })
        .await;
}

async fn warm_one_tracked(state: &Arc<SearchAppState>, run_id: u64, id: &str, config: WarmConfig) {
    let tracker = &state.warm;
    if let (Some(max), Some(now)) = (config.max_rss_mb, tracker.rss_mb()) {
        if now >= max {
            tracing::warn!("warm-all: not warming '{id}' — daemon RSS {now} MB >= {max} MB");
            tracker.note_ceiling_hit(run_id);
            tracker.set(
                run_id,
                id,
                WarmState::Failed,
                Some(format!(
                    "skipped: daemon RSS {now} MB reached the warm ceiling {max} MB"
                )),
            );
            return;
        }
    }
    tracker.set(run_id, id, WarmState::Warming, None);
    match tokio::time::timeout(config.index_timeout, warm_one(state, id)).await {
        Ok(Ok(())) => tracker.mark_warm(run_id, id, config.window),
        Ok(Err(e)) => {
            tracing::warn!("warm-all: index '{id}' failed to warm: {e}");
            tracker.set(run_id, id, WarmState::Failed, Some(e));
        }
        Err(_) => {
            let secs = config.index_timeout.as_secs();
            tracing::warn!("warm-all: index '{id}' did not warm within {secs}s");
            let msg = format!("timed out after {secs}s; its rehydrate continues in the background");
            tracker.set(run_id, id, WarmState::Failed, Some(msg));
        }
    }
}

/// Load (when cold-parked) and rehydrate one index.
async fn warm_one(state: &Arc<SearchAppState>, id: &str) -> Result<(), String> {
    let index_id = IndexId::new(id.to_string());
    // #1105: a cold-parked index loads through the lazy loader `search` uses.
    let handle = super::index_resolve::resolve_or_load_index(state, &index_id)
        .await
        .map_err(|(_, Json(body))| {
            let code = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("load_failed");
            format!("could not load the index: {code}")
        })?;
    crate::core::indexer::warm_corpus(&handle.indexer).await
}

/// The body `GET /warm/status` and `search.warm.status` serve.
///
/// Why: the dashboard polls this for progress, and a caller decides from it
/// whether an all-index search will cover every index.
/// What: one row per [`warm_targets`] id (plus any id the current run named).
/// `warm` is reported only for an index whose corpus is resident right now —
/// a warmed index the pressure sweep evicted since reads `cold` — and a lock
/// held elsewhere falls back to the run's record. `all_warm` is true only when
/// no run is in progress and every row is `warm`. `expires_at_unix_ms` is the
/// earliest residency-pin expiry among pinned rows.
/// Test: `warm_all_rehydrates_an_evicted_index_and_reports_it_warm`,
/// `a_failed_index_is_reported_failed_and_the_set_is_not_warm`,
/// `a_warmed_index_is_pinned_for_the_window_then_released`.
pub(crate) fn warm_status_report(state: &Arc<SearchAppState>) -> Value {
    let inner = state.warm.lock();
    let now = tokio::time::Instant::now();
    let now_ms = unix_ms_now();
    let mut ids = warm_targets(state);
    if let Some(run) = inner.run.as_ref() {
        ids.extend(run.indexes.keys().cloned());
        ids.sort();
        ids.dedup();
    }
    let (mut cold, mut warming, mut warm, mut failed) = (0usize, 0usize, 0usize, 0usize);
    let mut earliest_pin: Option<tokio::time::Instant> = None;
    let rows: Vec<Value> = ids
        .iter()
        .map(|id| {
            let progress = inner.run.as_ref().and_then(|r| r.indexes.get(id));
            let state_now = row_state(state, id, progress);
            match state_now {
                WarmState::Cold => cold += 1,
                WarmState::Warming => warming += 1,
                WarmState::Warm => warm += 1,
                WarmState::Failed => failed += 1,
            }
            let pin = inner.pins.get(id).copied().filter(|until| *until > now);
            if let Some(until) = pin {
                earliest_pin = Some(earliest_pin.map_or(until, |e| e.min(until)));
            }
            json!({
                "index_id": id,
                "state": state_now,
                "error": progress.and_then(|p| p.error.clone()),
                "pin_expires_in_secs": pin.map(|u| (u - now).as_secs()),
            })
        })
        .collect();
    let running = inner.run.as_ref().is_some_and(|r| r.running);
    let total = rows.len();
    let window = inner
        .run
        .as_ref()
        .map_or(Duration::from_secs(DEFAULT_WARM_WINDOW_SECS), |r| {
            r.config.window
        });
    json!({
        "run": inner.run.as_ref().map(|r| json!({
            "run_id": r.run_id,
            "running": r.running,
            "started_at_unix_ms": r.started_unix_ms,
            "finished_at_unix_ms": r.finished_unix_ms,
            "concurrency": r.config.concurrency,
            "index_timeout_secs": r.config.index_timeout.as_secs(),
        })),
        "totals": { "total": total, "cold": cold, "warming": warming, "warm": warm, "failed": failed },
        "all_warm": !running && total > 0 && warm == total,
        "window_secs": window.as_secs(),
        "expires_in_secs": earliest_pin.map(|u| (u - now).as_secs()),
        "expires_at_unix_ms": earliest_pin.map(|u| now_ms + (u - now).as_millis() as u64),
        "memory": {
            "rss_mb_now": state.warm.rss_mb(),
            "rss_mb_before": inner.run.as_ref().and_then(|r| r.rss_mb_before),
            "rss_mb_after": inner.run.as_ref().and_then(|r| r.rss_mb_after),
            "max_rss_mb": inner.run.as_ref().and_then(|r| r.config.max_rss_mb),
            "ceiling_hit": inner.run.as_ref().is_some_and(|r| r.ceiling_hit),
        },
        "indexes": rows,
    })
}

/// One row's state: the run's `warming`/`failed` verdict wins; otherwise live
/// residency decides, falling back to the run's record while the lock is held.
fn row_state(state: &SearchAppState, id: &str, progress: Option<&Progress>) -> WarmState {
    match progress.map(|p| p.state) {
        Some(WarmState::Warming) => return WarmState::Warming,
        Some(WarmState::Failed) => return WarmState::Failed,
        _ => {}
    }
    let Some(handle) = state.registry.get(&IndexId::new(id.to_string())) else {
        return WarmState::Cold;
    };
    let live = match handle.indexer.try_read() {
        Ok(indexer) if indexer.corpus_evicted() => WarmState::Cold,
        Ok(_) => WarmState::Warm,
        Err(_) => progress.map_or(WarmState::Cold, |p| p.state),
    };
    live
}

#[cfg(test)]
#[path = "warm_all_tests.rs"]
mod warm_all_tests;
