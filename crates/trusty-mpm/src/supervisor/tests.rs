//! Unit + integration tests for the unattended supervisor.
//!
//! Why: the supervisor is a long-running unattended process; its consequential
//! behavior (auto-resume gating, the N-session fleet sweep, metrics derivation,
//! HTTP handlers) must be proven offline — no real timers, tmux, or LLM. A
//! separate test file also keeps the production modules under the 500 SLOC cap.
//! What: a self-contained `FakeTmux` driver and a `StubClassifier`, plus tests
//! for config env parsing, [`FleetMetrics`] derivation, per-tick sweeps, the
//! N-session fleet auto-resume (the #1206 acceptance criterion), the
//! never-answer-pending-decision invariant, and the `/metrics` + `/health` routes.
//! Test: this file IS the test module; run `cargo test -p trusty-mpm`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tempfile::TempDir;

use crate::activity::cache::{ActivityState, ActivityVerdict};
use crate::activity::monitor::{ActivityError, ActivityMonitor, LlmClassifier};
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionManager,
    SessionRecord,
};

use super::Supervisor;
use super::config::{
    DEFAULT_LLM_MODEL, ENV_AUTO_RESUME, ENV_CLASSIFY_IDLE, ENV_INTERVAL_SECS, ENV_LLM_MODEL,
    ENV_METRICS_ADDR, SupervisorConfig,
};
use super::metrics::FleetMetrics;
use super::poller::run_tick;

// ── Test doubles ─────────────────────────────────────────────────────────────

/// A minimal fake tmux driver for supervisor tests.
///
/// Why: the session manager needs a `ManagedTmuxDriver` to operate, but the
/// supervisor tests must run with no real tmux; this records the calls the
/// supervisor causes (kills + creates during resume) so tests can assert resume
/// actually re-spawned a tmux session.
/// What: tracks created sessions in a map and counts create/kill calls.
/// Test: used by every supervisor test that builds a `SessionManager`.
struct FakeTmux {
    sessions: Mutex<HashMap<String, String>>,
    create_calls: Mutex<u32>,
    kill_calls: Mutex<u32>,
}

impl FakeTmux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            create_calls: Mutex::new(0),
            kill_calls: Mutex::new(0),
        })
    }
}

impl ManagedTmuxDriver for FakeTmux {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        *self.create_calls.lock().expect("lock") += 1;
        self.sessions
            .lock()
            .expect("lock")
            .insert(name.to_owned(), workdir.to_owned());
        Ok(())
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        *self.kill_calls.lock().expect("lock") += 1;
        self.sessions.lock().expect("lock").remove(name);
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok("working on the task...\n$ cargo test".to_owned())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self
            .sessions
            .lock()
            .expect("lock")
            .keys()
            .cloned()
            .collect())
    }
}

/// A stub LLM classifier that never touches the network.
///
/// Why: idle-session classification must be testable without an API key; this
/// returns a fixed verdict and counts calls so tests can assert classification ran.
/// What: implements [`LlmClassifier::classify`] returning a constant `Working`
/// verdict and bumping a call counter.
/// Test: `tick_classifies_active`.
struct StubClassifier;

impl StubClassifier {
    fn new() -> Self {
        Self
    }
}

impl LlmClassifier for StubClassifier {
    async fn classify(
        &self,
        _pane_text: &str,
    ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
        Ok((
            ActivityVerdict {
                state: ActivityState::Working,
                summary: "stub".into(),
                confidence: 1.0,
            },
            10,
            2,
        ))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a session manager backed by a fake tmux driver under a temp dir.
async fn make_manager(dir: &TempDir, tmux: Arc<FakeTmux>) -> Arc<SessionManager> {
    Arc::new(
        SessionManager::new(dir.path(), tmux)
            .await
            .expect("session manager"),
    )
}

/// Build a `SupervisorConfig` with auto-resume on and classification off.
fn resume_cfg() -> SupervisorConfig {
    SupervisorConfig {
        auto_resume: true,
        classify_idle: false,
        ..SupervisorConfig::default()
    }
}

/// Seed `n` records directly into the store in a given state, with a workspace
/// so `resume` has a directory to re-spawn into.
async fn seed_sessions(
    mgr: &Arc<SessionManager>,
    n: usize,
    state: ManagedSessionState,
    ws: &TempDir,
) -> Vec<ManagedSessionId> {
    let mut ids = Vec::new();
    let mut store = mgr.store.write().await;
    for i in 0..n {
        let id = ManagedSessionId::new();
        let rec = SessionRecord {
            id,
            tmux_name: format!("tmpm-fleet-{i}"),
            cwd: ws.path().to_path_buf(),
            task: format!("fleet task {i}"),
            state: state.clone(),
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: Some(ws.path().to_path_buf()),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
        };
        store.upsert(rec).await.expect("seed upsert");
        ids.push(id);
    }
    drop(store);
    ids
}

// ── Config tests ─────────────────────────────────────────────────────────────

/// Build an env resolver over a fixed `(key, value)` map.
///
/// Why: the `*_env_parsing` tests must exercise `SupervisorConfig::from_env_with`
/// with deterministic input and NO process-wide mutation, so they cannot flake
/// when the test runner schedules them on parallel threads. A closure over a local
/// `HashMap` is the injectable fake env.
/// What: returns an `impl Fn(&str) -> Option<String>` that looks each key up in the
/// supplied pairs, mirroring `std::env::var(key).ok()`.
/// Test: used by `auto_resume_env_parsing`, `interval_env_parsing`,
/// `classify_idle_env_parsing`, `metrics_addr_env_parsing`.
fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

#[test]
fn config_defaults() {
    let c = SupervisorConfig::default();
    assert_eq!(c.interval.as_secs(), 30);
    assert!(!c.auto_resume);
    assert!(c.classify_idle);
    assert_eq!(c.metrics_addr.to_string(), "127.0.0.1:7881");
    // An empty injected env yields the same defaults (no process env touched).
    let from_empty = SupervisorConfig::from_env_with(fake_env(&[]));
    assert_eq!(from_empty, c);
}

#[test]
fn default_llm_model_is_documented() {
    // The classification model env var + default are exposed as named constants
    // so the read site (`commands/supervisor.rs`) and the README config table
    // share one source of truth rather than a buried string literal.
    assert_eq!(ENV_LLM_MODEL, "TRUSTY_LLM_MODEL");
    assert_eq!(DEFAULT_LLM_MODEL, "openai/gpt-4o-mini");
}

#[test]
fn auto_resume_env_parsing() {
    // Injected env — no std::env::set_var, so this is parallel-safe.
    let on = SupervisorConfig::from_env_with(fake_env(&[(ENV_AUTO_RESUME, "true")]));
    assert!(on.auto_resume);
    let off = SupervisorConfig::from_env_with(fake_env(&[(ENV_AUTO_RESUME, "0")]));
    assert!(!off.auto_resume);
    // Absent → default (off).
    let unset = SupervisorConfig::from_env_with(fake_env(&[]));
    assert!(!unset.auto_resume);
}

#[test]
fn interval_env_parsing() {
    let five = SupervisorConfig::from_env_with(fake_env(&[(ENV_INTERVAL_SECS, "5")]));
    assert_eq!(five.interval.as_secs(), 5);
    // Zero is rejected (falls back to default), as is garbage.
    let zero = SupervisorConfig::from_env_with(fake_env(&[(ENV_INTERVAL_SECS, "0")]));
    assert_eq!(zero.interval.as_secs(), 30);
    let garbage = SupervisorConfig::from_env_with(fake_env(&[(ENV_INTERVAL_SECS, "notanumber")]));
    assert_eq!(garbage.interval.as_secs(), 30);
}

#[test]
fn classify_idle_env_parsing() {
    let off = SupervisorConfig::from_env_with(fake_env(&[(ENV_CLASSIFY_IDLE, "off")]));
    assert!(!off.classify_idle);
    // Unset → default (enabled).
    let unset = SupervisorConfig::from_env_with(fake_env(&[]));
    assert!(unset.classify_idle);
}

#[test]
fn metrics_addr_env_parsing() {
    let custom = SupervisorConfig::from_env_with(fake_env(&[(ENV_METRICS_ADDR, "127.0.0.1:9999")]));
    assert_eq!(custom.metrics_addr.to_string(), "127.0.0.1:9999");
    // Garbage falls back to the default address.
    let garbage = SupervisorConfig::from_env_with(fake_env(&[(ENV_METRICS_ADDR, "garbage")]));
    assert_eq!(garbage.metrics_addr.to_string(), "127.0.0.1:7881");
}

#[test]
fn env_bool_recognizes_truthy_and_falsy() {
    // Every documented truthy spelling enables auto-resume…
    for truthy in ["1", "true", "yes", "on", "TRUE", " On "] {
        let c = SupervisorConfig::from_env_with(fake_env(&[(ENV_AUTO_RESUME, truthy)]));
        assert!(c.auto_resume, "{truthy:?} should be truthy");
    }
    // …and every falsy spelling keeps it off.
    for falsy in ["0", "false", "no", "off", "FALSE"] {
        let c = SupervisorConfig::from_env_with(fake_env(&[(ENV_AUTO_RESUME, falsy)]));
        assert!(!c.auto_resume, "{falsy:?} should be falsy");
    }
    // Unrecognized spelling → fall back to the default (off).
    let unknown = SupervisorConfig::from_env_with(fake_env(&[(ENV_AUTO_RESUME, "maybe")]));
    assert!(!unknown.auto_resume);
}

// ── Metrics tests ────────────────────────────────────────────────────────────

fn rec(state: ManagedSessionState, pending: Option<&str>) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-x".into(),
        cwd: PathBuf::from("/tmp"),
        task: "t".into(),
        state,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: pending.map(|s| s.to_owned()),
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
    }
}

#[test]
fn metrics_counts_by_state() {
    let records = vec![
        rec(ManagedSessionState::Active, None),
        rec(ManagedSessionState::Active, None),
        rec(ManagedSessionState::Stopped, None),
        rec(ManagedSessionState::Errored, None),
        rec(ManagedSessionState::Decommissioned, None),
        rec(ManagedSessionState::Provisioning, None),
    ];
    let m = FleetMetrics::from_records(&records);
    assert_eq!(m.active, 2);
    assert_eq!(m.stopped, 1);
    assert_eq!(m.errored, 1);
    assert_eq!(m.decommissioned, 1);
    assert_eq!(m.provisioning, 1);
    assert_eq!(m.total, 6);
}

#[test]
fn metrics_surfaces_pending_decisions() {
    let records = vec![
        rec(ManagedSessionState::Active, Some("Merge PR #42?")),
        rec(ManagedSessionState::Active, None),
    ];
    let m = FleetMetrics::from_records(&records);
    assert_eq!(m.pending_decisions.len(), 1);
    assert_eq!(m.pending_decisions[0].question, "Merge PR #42?");
}

#[test]
fn metrics_last_activity_is_max() {
    let early = Utc::now() - chrono::Duration::hours(2);
    let late = Utc::now();
    let mut r1 = rec(ManagedSessionState::Active, None);
    r1.last_activity_at = Some(early);
    let mut r2 = rec(ManagedSessionState::Active, None);
    r2.last_activity_at = Some(late);
    let m = FleetMetrics::from_records(&[r1, r2]);
    assert_eq!(m.last_activity_at, Some(late));
}

// ── Tick / sweep tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn tick_auto_resumes_stopped() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;

    let report = run_tick::<StubClassifier>(&mgr, &resume_cfg(), None).await;
    assert_eq!(report.resumed.len(), 1);
    assert_eq!(report.resume_failures, 0);

    // The session is now Active and tmux create was called.
    let after = mgr.list().await;
    assert_eq!(after[0].state, ManagedSessionState::Active);
    assert_eq!(*tmux.create_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn tick_skips_resume_when_disabled() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 3, ManagedSessionState::Stopped, &ws).await;

    let cfg = SupervisorConfig {
        auto_resume: false,
        classify_idle: false,
        ..SupervisorConfig::default()
    };
    let report = run_tick::<StubClassifier>(&mgr, &cfg, None).await;
    assert!(report.resumed.is_empty());
    // Still stopped — observer mode did not resume.
    assert!(
        mgr.list()
            .await
            .iter()
            .all(|r| r.state == ManagedSessionState::Stopped)
    );
}

#[tokio::test]
async fn tick_fleet_of_n_resumed() {
    // The #1206 acceptance criterion: a fleet of N stopped sessions is
    // auto-resumed unattended in a single sweep.
    const N: usize = 12;
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, N, ManagedSessionState::Stopped, &ws).await;

    let report = run_tick::<StubClassifier>(&mgr, &resume_cfg(), None).await;
    assert_eq!(report.observed, N);
    assert_eq!(report.resumed.len(), N);
    assert_eq!(report.resume_failures, 0);

    let after = mgr.list().await;
    assert_eq!(after.len(), N);
    assert!(after.iter().all(|r| r.state == ManagedSessionState::Active));
    assert_eq!(*tmux.create_calls.lock().unwrap(), N as u32);
}

#[tokio::test]
async fn tick_classifies_active() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 2, ManagedSessionState::Active, &ws).await;

    let monitor = ActivityMonitor::new(StubClassifier::new(), "test-model");
    let cfg = SupervisorConfig {
        auto_resume: false,
        classify_idle: true,
        ..SupervisorConfig::default()
    };
    let report = run_tick(&mgr, &cfg, Some(&monitor)).await;
    assert_eq!(report.classified, 2);
}

#[tokio::test]
async fn tick_never_answers_pending_decision() {
    // The supervisor is a passive observer: a session blocked on a decision is
    // NOT auto-answered. After a sweep the pending_decision must be intact.
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    let ids = seed_sessions(&mgr, 1, ManagedSessionState::Active, &ws).await;
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&ids[0]).await.expect("get");
        r.pending_decision = Some("Force-push to main?".into());
        store.upsert(r).await.expect("upsert");
    }

    let monitor = ActivityMonitor::new(StubClassifier::new(), "test-model");
    let cfg = SupervisorConfig {
        auto_resume: true,
        classify_idle: true,
        ..SupervisorConfig::default()
    };
    run_tick(&mgr, &cfg, Some(&monitor)).await;

    let after = mgr.list().await;
    assert_eq!(
        after[0].pending_decision.as_deref(),
        Some("Force-push to main?"),
        "supervisor must never answer or clear a pending decision"
    );
}

// ── Supervisor struct tests ──────────────────────────────────────────────────

#[tokio::test]
async fn supervisor_tick_updates_stats() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 4, ManagedSessionState::Stopped, &ws).await;

    let mut sup: Supervisor<StubClassifier> = Supervisor::new(mgr, resume_cfg(), None);
    sup.tick().await;
    assert_eq!(sup.stats().sweeps, 1);
    assert_eq!(sup.stats().auto_resumed, 4);
    // A second sweep finds them all Active now → no more resumes.
    sup.tick().await;
    assert_eq!(sup.stats().sweeps, 2);
    assert_eq!(sup.stats().auto_resumed, 4);
}

#[tokio::test]
async fn supervisor_snapshot_reflects_fleet() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 3, ManagedSessionState::Stopped, &ws).await;

    let mut sup: Supervisor<StubClassifier> = Supervisor::new(mgr, resume_cfg(), None);
    let before = sup.snapshot().await;
    assert_eq!(before.stopped, 3);
    assert_eq!(before.active, 0);

    sup.tick().await;
    let after = sup.snapshot().await;
    assert_eq!(after.stopped, 0);
    assert_eq!(after.active, 3);
    assert_eq!(after.run_stats.auto_resumed, 3);
}

#[tokio::test]
async fn supervisor_classifier_invoked_on_active() {
    // Confirm the supervisor actually drives the classifier on active sessions.
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 2, ManagedSessionState::Active, &ws).await;

    let monitor = ActivityMonitor::new(StubClassifier::new(), "test-model");
    let cfg = SupervisorConfig {
        auto_resume: false,
        classify_idle: true,
        ..SupervisorConfig::default()
    };
    let mut sup = Supervisor::new(mgr, cfg, Some(monitor));
    let report = sup.tick().await;
    assert_eq!(report.classified, 2);
    assert_eq!(sup.stats().classified, 2);
}

// ── HTTP tests (daemon feature) ──────────────────────────────────────────────

#[cfg(feature = "daemon")]
mod http_tests {
    use super::*;
    use crate::supervisor::http;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn health_endpoint_ok() {
        let handle = http::new_handle();
        let app = http::router(handle);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
    }

    #[tokio::test]
    async fn supervisor_run_until_stops_cleanly() {
        // The loop must finish cleanly when its injected shutdown future resolves,
        // having published at least the initial snapshot — never killed mid-sweep.
        let dir = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();
        let tmux = FakeTmux::new();
        let mgr = make_manager(&dir, tmux.clone()).await;
        seed_sessions(&mgr, 2, ManagedSessionState::Stopped, &ws).await;

        // A long interval guarantees the loop is parked on the timer when the
        // shutdown signal fires, exercising the biased select's shutdown arm.
        let cfg = SupervisorConfig {
            interval: std::time::Duration::from_secs(3600),
            auto_resume: true,
            classify_idle: false,
            ..SupervisorConfig::default()
        };
        let sup: Supervisor<StubClassifier> = Supervisor::new(mgr, cfg, None);

        let handle = http::new_handle();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Fire shutdown almost immediately; the loop should observe it and return.
        tokio::spawn(async move {
            let _ = tx.send(());
        });
        let shutdown = async move {
            let _ = rx.await;
        };

        // Bound the test so a regression (loop ignoring shutdown) fails fast
        // instead of hanging the suite.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sup.run_until(handle.clone(), shutdown),
        )
        .await
        .expect("run_until must return promptly after shutdown");
        result.expect("clean shutdown returns Ok");

        // The initial snapshot was published before the loop parked on the timer.
        assert_eq!(handle.read().await.stopped, 2);
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_snapshot() {
        let handle = http::new_handle();
        // Seed the shared snapshot as the loop would.
        {
            let records = vec![
                rec(ManagedSessionState::Active, None),
                rec(ManagedSessionState::Stopped, None),
            ];
            *handle.write().await = FleetMetrics::from_records(&records);
        }
        let app = http::router(handle);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let m: FleetMetrics = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m.active, 1);
        assert_eq!(m.stopped, 1);
        assert_eq!(m.total, 2);
    }
}
