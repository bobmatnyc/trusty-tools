//! Unit + integration tests for the unattended supervisor.
//!
//! Why: the supervisor is a long-running unattended process; its consequential
//! behavior (auto-resume gating, the N-session fleet sweep, metrics derivation,
//! snapshot publication) must be proven offline — no real timers, tmux, or LLM. A
//! separate test file also keeps the production modules under the 500 SLOC cap.
//! What: a self-contained `FakeTmux` driver and a `StubClassifier`, plus tests
//! for config env parsing, [`FleetMetrics`] derivation, per-tick sweeps, the
//! N-session fleet auto-resume (the #1206 acceptance criterion), the
//! never-answer-pending-decision invariant, and the #6288 publish/read contract.
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
    SessionRecord, StopCause,
};

use super::Supervisor;
use super::config::{
    DEFAULT_LLM_MODEL, ENV_AUTO_RESUME, ENV_CLASSIFY_IDLE, ENV_INTERVAL_SECS, ENV_LLM_MODEL,
    SupervisorConfig,
};
use super::metrics::FleetMetrics;
use super::poller::run_tick;
use super::publish::{self, SupervisorMetricsStatus};

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

/// A desired-state path inside a temp dir where no override file exists.
///
/// Why: #5208 made `Supervisor::tick` read `~/.trusty-mpm/auto_resume` every
/// sweep. Left unpinned, every supervisor test would depend on whatever the
/// developer's own console toggle last wrote. Pointing at a nonexistent file in
/// the test's temp dir keeps `resolve_auto_resume` on the "no override → boot
/// flag stands" arm, which is what these tests mean to exercise.
/// What: `<dir>/auto_resume`, deliberately never created.
/// Test: used by every `Supervisor::new` call site in this file.
fn no_override(dir: &TempDir) -> PathBuf {
    dir.path().join("auto_resume")
}

/// Build a `SupervisorConfig` with auto-resume on and classification off.
fn resume_cfg() -> SupervisorConfig {
    SupervisorConfig {
        auto_resume: true,
        classify_idle: false,
        ..SupervisorConfig::default()
    }
}

/// Stamp a seeded record's [`StopCause`] (#6194).
///
/// Why: `seed_sessions` writes records straight into the store, so there is no
/// transition to write a cause. The auto-resume gate reads one, and these tests
/// need each of its values.
/// Test: `tick_never_resumes_a_deliberately_stopped_session`,
/// `tick_still_resumes_a_session_whose_runtime_exited`.
async fn set_stop_cause(
    mgr: &Arc<SessionManager>,
    id: &ManagedSessionId,
    cause: Option<StopCause>,
) {
    let mut store = mgr.store.write().await;
    let mut record = store.cached_get(id).expect("seeded record");
    record.stop_cause = cause;
    store.upsert(record).await.expect("stamp stop_cause");
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
            injection_status: Default::default(),
            worktree_owner: None,
            terminal_at: None,
            stop_cause: None,
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
/// `classify_idle_env_parsing`.
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

/// REGRESSION (#6288): the supervisor binds no address, so no env var, CLI flag,
/// or config field may reintroduce one. `TRUSTY_MPM_SUPERVISOR_ADDR` was the
/// name; a config built with it set must be indistinguishable from the default,
/// which is only true while `SupervisorConfig` has no address field at all.
#[test]
fn supervisor_config_ignores_a_stale_bind_address() {
    let with_stale_addr = SupervisorConfig::from_env_with(fake_env(&[(
        "TRUSTY_MPM_SUPERVISOR_ADDR",
        "127.0.0.1:7881",
    )]));
    assert_eq!(
        with_stale_addr,
        SupervisorConfig::default(),
        "a leftover TRUSTY_MPM_SUPERVISOR_ADDR in an operator's environment must \
         change nothing — the supervisor publishes to a file (#6288)"
    );
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
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
        stop_cause: None,
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

/// #4400 defense in depth: a terminal (`Decommissioned`/`Deleted`) record
/// that still carries a stale `pending_decision` — e.g. one persisted before
/// the decommission-path fix landed, or not yet swept by the boot backfill —
/// must never be surfaced in the human-confirmation queue. A dead entry next
/// to a real T4 gate is indistinguishable and trains operators to ignore the
/// whole queue.
#[test]
fn metrics_filters_pending_decisions_on_terminal_states() {
    let records = vec![
        rec(
            ManagedSessionState::Decommissioned,
            Some("stale — never acted on"),
        ),
        rec(ManagedSessionState::Deleted, Some("also stale")),
        rec(ManagedSessionState::Active, Some("Merge PR #42?")),
    ];
    let m = FleetMetrics::from_records(&records);
    assert_eq!(
        m.pending_decisions.len(),
        1,
        "terminal-state pending_decisions must be filtered out: {:?}",
        m.pending_decisions
    );
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

/// A sweep never relaunches a session somebody stopped on purpose (#6194).
///
/// Why: this is the reported defect. `tmux kill-session` on a leaked pane and
/// `tm session stop` both leave a `Stopped` record carrying
/// [`StopCause::Deliberate`], and the sweep resumed every `Stopped` record it
/// saw — so the session was back within one interval and only
/// `tm session decommission` broke the loop. `create_calls` is the load-bearing
/// assertion: against the pre-fix code the record reads `Active` at the end
/// because the sweep respawned it.
/// Test: this function IS the test.
#[tokio::test]
async fn tick_never_resumes_a_deliberately_stopped_session() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    let ids = seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;
    set_stop_cause(&mgr, &ids[0], Some(StopCause::Deliberate)).await;

    let report = run_tick::<StubClassifier>(&mgr, &resume_cfg(), None).await;

    assert!(
        report.resumed.is_empty(),
        "the sweep respawned a session the operator had stopped: {:?}",
        report.resumed
    );
    assert_eq!(report.resume_failures, 0, "not resuming is not a failure");
    assert_eq!(
        *tmux.create_calls.lock().unwrap(),
        0,
        "no tmux session may be created for a deliberately stopped record"
    );
    assert_eq!(mgr.list().await[0].state, ManagedSessionState::Stopped);
}

/// A sweep still relaunches a session whose runtime exited on its own (#6194).
///
/// Why: the other direction. Gating auto-resume on the stop cause must not
/// disable auto-resume — a runtime that died with nothing asking is exactly
/// what the mode exists to bring back.
/// Test: this function IS the test.
#[tokio::test]
async fn tick_still_resumes_a_session_whose_runtime_exited() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    let ids = seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;
    set_stop_cause(&mgr, &ids[0], Some(StopCause::Unexpected)).await;

    let report = run_tick::<StubClassifier>(&mgr, &resume_cfg(), None).await;

    assert_eq!(report.resumed.len(), 1);
    assert_eq!(*tmux.create_calls.lock().unwrap(), 1);
    let after = mgr.list().await;
    assert_eq!(after[0].state, ManagedSessionState::Active);
    assert_eq!(
        after[0].stop_cause, None,
        "a resumed session carries no stop cause"
    );
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

    let mut sup: Supervisor<StubClassifier> =
        Supervisor::new(mgr, resume_cfg(), None).with_auto_resume_path(no_override(&dir));
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

    let mut sup: Supervisor<StubClassifier> =
        Supervisor::new(mgr, resume_cfg(), None).with_auto_resume_path(no_override(&dir));
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
    let mut sup = Supervisor::new(mgr, cfg, Some(monitor)).with_auto_resume_path(no_override(&dir));
    let report = sup.tick().await;
    assert_eq!(report.classified, 2);
    assert_eq!(sup.stats().classified, 2);
}

// ── #5208: persisted console desired-state drives the supervisor ─────────────

/// Why: #5208 — `auto_resume_set` wrote `~/.trusty-mpm/auto_resume`, reported
/// success, and no code path read it, so an operator toggling auto-resume in the
/// console changed nothing in the running supervisor. This is the acceptance
/// test: the console's write, and ONLY that write, must make a live supervisor
/// resume a stopped session, with no process restart and no env var set.
/// What: boots a supervisor whose config has `auto_resume = false` (as
/// `TRUSTY_MPM_AUTO_RESUME` unset would give it), sweeps once and asserts the
/// session is untouched; then writes the desired-state file exactly as
/// `auto_resume_set` does and sweeps the SAME supervisor instance again — the
/// session must now be `Active`, with a real tmux create behind it.
/// Test: this test.
#[tokio::test]
async fn supervisor_honours_console_desired_state_without_restart() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let desired = state_dir.path().join("auto_resume");
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;

    let cfg = SupervisorConfig {
        auto_resume: false,
        classify_idle: false,
        ..SupervisorConfig::default()
    };
    let mut sup: Supervisor<StubClassifier> =
        Supervisor::new(mgr.clone(), cfg, None).with_auto_resume_path(&desired);

    // No override file yet: the boot flag (off) stands.
    let before = sup.tick().await;
    assert!(
        before.resumed.is_empty(),
        "no desired-state file must leave the boot flag (off) in force"
    );
    assert_eq!(mgr.list().await[0].state, ManagedSessionState::Stopped);
    assert_eq!(*tmux.create_calls.lock().unwrap(), 0);

    // The operator flips the console toggle. This is exactly what
    // `daemon::mcp_console::auto_resume_set` does — no restart, no env change.
    crate::core::auto_resume::write_desired_at(&desired, true).expect("console write");

    let after = sup.tick().await;
    assert_eq!(
        after.resumed.len(),
        1,
        "the persisted console setting must make the SAME running supervisor resume"
    );
    assert_eq!(
        mgr.list().await[0].state,
        ManagedSessionState::Active,
        "the session must actually be resumed, not merely counted"
    );
    assert_eq!(
        *tmux.create_calls.lock().unwrap(),
        1,
        "resume must have re-spawned a tmux session"
    );
    assert_eq!(sup.stats().auto_resumed, 1);
}

/// Why: the toggle has to work in both directions. An operator disabling
/// auto-resume in the console must stop a supervisor that booted with
/// `TRUSTY_MPM_AUTO_RESUME=1` — otherwise "off" is the write-only half of the
/// same defect.
/// What: boots with `auto_resume = true`, writes `false` to the desired-state
/// file, and asserts the sweep leaves the stopped session alone.
/// Test: this test.
#[tokio::test]
async fn supervisor_console_disable_overrides_env_enabled() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let desired = state_dir.path().join("auto_resume");
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 2, ManagedSessionState::Stopped, &ws).await;

    crate::core::auto_resume::write_desired_at(&desired, false).expect("console write");

    let mut sup: Supervisor<StubClassifier> =
        Supervisor::new(mgr.clone(), resume_cfg(), None).with_auto_resume_path(&desired);
    let report = sup.tick().await;

    assert!(
        report.resumed.is_empty(),
        "an explicit console `false` must outrank the boot env flag"
    );
    assert!(
        mgr.list()
            .await
            .iter()
            .all(|r| r.state == ManagedSessionState::Stopped)
    );
    assert_eq!(*tmux.create_calls.lock().unwrap(), 0);
}

/// Why: an absent file means "the operator never touched the toggle", which must
/// leave an env-enabled supervisor enabled. Reading the file with the display
/// helper (`read_desired_at`, which flattens absent → `false`) would disable it
/// instead — the original fail-open, relocated into the fix.
/// What: boots with `auto_resume = true` and no desired-state file; the sweep
/// must still resume.
/// Test: this test.
#[tokio::test]
async fn supervisor_absent_desired_file_keeps_boot_flag() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;

    let mut sup: Supervisor<StubClassifier> = Supervisor::new(mgr.clone(), resume_cfg(), None)
        .with_auto_resume_path(state_dir.path().join("auto_resume"));
    let report = sup.tick().await;

    assert_eq!(
        report.resumed.len(),
        1,
        "no override file must leave the boot flag (on) in force"
    );
    assert_eq!(mgr.list().await[0].state, ManagedSessionState::Active);
}

/// Why: the read the fix introduces is itself a new failure arm. If an
/// unreadable desired-state file collapsed to `false`, a permissions accident or
/// a stray directory at that path would silently turn an operator-enabled
/// supervisor into an observer — the same "reports fine, does nothing" shape
/// #5208 closes, one layer out.
/// What: puts a DIRECTORY where the file belongs (reads fail with EISDIR, not
/// NotFound) and asserts an `auto_resume = true` supervisor still resumes.
/// Test: this test.
#[tokio::test]
async fn supervisor_unreadable_desired_file_does_not_disable_resume() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let desired = state_dir.path().join("auto_resume");
    std::fs::create_dir(&desired).expect("mkdir over the desired-state path");
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;

    let mut sup: Supervisor<StubClassifier> =
        Supervisor::new(mgr.clone(), resume_cfg(), None).with_auto_resume_path(&desired);
    let report = sup.tick().await;

    assert_eq!(
        report.resumed.len(),
        1,
        "an unreadable desired-state file must not fail open to auto-resume off"
    );
    assert_eq!(mgr.list().await[0].state, ManagedSessionState::Active);
}

/// Why: #5208's layer above — once the persisted setting makes the supervisor
/// resume, a resume that FAILS must not degrade to a log line while the session
/// stays dead and `Stopped`. A `Stopped` record is retried by every subsequent
/// sweep forever, invisibly; the failure has to land somewhere an operator
/// looks. `FleetMetrics.errored` is that place — it is what drives the console's
/// Degraded health.
/// What: seeds a stopped session whose workspace directory has been deleted (so
/// `resume` fails in `resolve_existing_workdir`), enables auto-resume via the
/// console file, and asserts the sweep counts the failure, marks the record
/// `Errored`, surfaces it in the snapshot, and does NOT retry it next sweep.
/// Test: this test.
#[tokio::test]
async fn supervisor_failed_resume_surfaces_as_errored() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let desired = state_dir.path().join("auto_resume");
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 1, ManagedSessionState::Stopped, &ws).await;
    // Delete the workspace so every resume path (last_cwd → workspace_path →
    // cwd) is missing on disk and `resume` returns WorkspaceMissing.
    ws.close()
        .expect("drop the workspace the session resumes into");

    crate::core::auto_resume::write_desired_at(&desired, true).expect("console write");
    let mut sup: Supervisor<StubClassifier> = Supervisor::new(
        mgr.clone(),
        SupervisorConfig {
            auto_resume: false,
            classify_idle: false,
            ..SupervisorConfig::default()
        },
        None,
    )
    .with_auto_resume_path(&desired);

    let report = sup.tick().await;
    assert!(report.resumed.is_empty(), "the resume must have failed");
    assert_eq!(report.resume_failures, 1);

    let after = mgr.list().await;
    assert_eq!(
        after[0].state,
        ManagedSessionState::Errored,
        "a failed auto-resume must leave the session visibly errored, not silently stopped"
    );
    assert!(
        after[0].task.contains("auto-resume failed"),
        "the failure reason must be readable from the record: {}",
        after[0].task
    );

    // It surfaces where the console looks: FleetMetrics.errored drives Degraded.
    let snapshot = sup.snapshot().await;
    assert_eq!(snapshot.errored, 1);
    assert_eq!(snapshot.stopped, 0);

    // And the doomed session is not retried forever on every subsequent sweep.
    let second = sup.tick().await;
    assert_eq!(
        second.resume_failures, 0,
        "an errored session must not be re-attempted every interval"
    );
}

// ── #6288: snapshot publication (the file the daemon reads) ──────────────────

/// Why: the daemon deserialises exactly what the supervisor serialised; a field
/// rename on either side silently degrades `run_stats` back to zero, which is
/// the defect #6288 exists to close. Round-tripping through the real file pins
/// the wire shape.
/// Test: this is the test.
#[test]
fn published_metrics_round_trip() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("nested").join("supervisor-metrics.json");
    let mut fleet = FleetMetrics::from_records(&[
        rec(ManagedSessionState::Active, None),
        rec(ManagedSessionState::Stopped, None),
    ]);
    fleet.run_stats.sweeps = 7;
    fleet.run_stats.auto_resumed = 3;
    let now = Utc::now();

    publish::write_at(&path, &fleet, now).expect("publish writes");
    let back = publish::read_at(&path)
        .expect("read succeeds")
        .expect("a snapshot exists");
    assert_eq!(back.fleet, fleet);
    assert_eq!(back.written_at.timestamp(), now.timestamp());

    // Atomic publish: the temp file is renamed into place, never left behind.
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

/// FAIL-OPEN CHECK (#6288): no snapshot must read as `Unavailable` carrying a
/// reason, never as a silently-zeroed `run_stats`. Before this issue the daemon
/// had no file to read and reported zero sweeps with nothing to explain it.
/// Test: this is the test.
#[test]
fn read_status_absent_is_unavailable() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("supervisor-metrics.json");
    match publish::read_status_at(&path, Utc::now()) {
        SupervisorMetricsStatus::Unavailable { reason } => {
            assert!(
                reason.contains("supervisor"),
                "the reason must tell an operator what is missing: {reason}"
            );
        }
        other => panic!("an absent snapshot must be Unavailable, got {other:?}"),
    }
}

/// FAIL-OPEN CHECK (#6288): a corrupt or truncated file must NOT be flattened
/// into "absent and therefore zero" — it reports `Unavailable` with the parse
/// error, so a broken publisher is visible rather than looking like an idle one.
/// Test: this is the test.
#[test]
fn read_status_corrupt_is_unavailable() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("supervisor-metrics.json");
    std::fs::write(&path, "{ this is not json").expect("write corrupt");
    match publish::read_status_at(&path, Utc::now()) {
        SupervisorMetricsStatus::Unavailable { reason } => {
            assert!(
                reason.contains("json"),
                "a corrupt file must report the parse failure: {reason}"
            );
        }
        other => panic!("a corrupt snapshot must be Unavailable, got {other:?}"),
    }
}

/// Why (#6288): a stopped supervisor leaves its last file on disk forever.
/// Presenting month-old counters as current is the same lie as presenting zero,
/// so the reader ages them out. It still hands back the snapshot, because the
/// last real observation beats no observation.
/// Test: this is the test.
#[test]
fn read_status_old_snapshot_is_stale() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("supervisor-metrics.json");
    let mut fleet = FleetMetrics::default();
    fleet.run_stats.sweeps = 42;
    let written_at = Utc::now() - chrono::Duration::seconds(publish::STALE_AFTER_SECS + 60);
    publish::write_at(&path, &fleet, written_at).expect("publish writes");

    match publish::read_status_at(&path, Utc::now()) {
        SupervisorMetricsStatus::Stale { snapshot, age_secs } => {
            assert!(age_secs > publish::STALE_AFTER_SECS, "age {age_secs}");
            assert_eq!(
                snapshot.fleet.run_stats.sweeps, 42,
                "a stale snapshot still carries the last real counters"
            );
        }
        other => panic!("an old snapshot must be Stale, got {other:?}"),
    }

    // The same file republished now is current.
    publish::write_at(&path, &fleet, Utc::now()).expect("republish");
    assert!(
        matches!(
            publish::read_status_at(&path, Utc::now()),
            SupervisorMetricsStatus::Current { .. }
        ),
        "a freshly-published snapshot must be Current"
    );
}

/// Why: `metrics_path` is the one location the supervisor writes and the daemon
/// reads; a drift between them is invisible until `run_stats` silently zeroes.
/// Test: this is the test.
#[test]
fn metrics_path_is_under_root() {
    let paths = crate::core::paths::FrameworkPaths::under("/tmp/test-base");
    let p = publish::metrics_path(&paths);
    assert!(p.ends_with(".trusty-mpm/supervisor-metrics.json"), "{p:?}");
}

/// BEHAVIORAL BAR (#6288): after real sweeps against a real session manager, the
/// counters a reader picks out of the published file are the supervisor's actual
/// `run_stats` — non-zero sweeps and non-zero auto-resumes. This is the
/// end-to-end path `console_metrics` / `supervisor_status` now take; the daemon
/// side of the same round trip is
/// `supervisor_metrics_merge_reports_real_run_stats` below.
/// Test: this is the test.
#[tokio::test]
async fn supervisor_publishes_run_stats_after_sweeps() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 2, ManagedSessionState::Stopped, &ws).await;

    let cfg = SupervisorConfig {
        auto_resume: true,
        classify_idle: false,
        ..SupervisorConfig::default()
    };
    let metrics_file = dir.path().join("supervisor-metrics.json");
    let mut sup: Supervisor<StubClassifier> = Supervisor::new(mgr, cfg, None)
        .with_auto_resume_path(no_override(&dir))
        .with_metrics_path(&metrics_file);

    // Nothing published yet: a reader must say so rather than report zero.
    assert!(
        matches!(
            publish::read_status_at(&metrics_file, Utc::now()),
            SupervisorMetricsStatus::Unavailable { .. }
        ),
        "before the first sweep there is no snapshot to read"
    );

    sup.tick().await;
    sup.publish_snapshot().await;
    sup.tick().await;
    sup.publish_snapshot().await;

    match publish::read_status_at(&metrics_file, Utc::now()) {
        SupervisorMetricsStatus::Current { snapshot, .. } => {
            let stats = &snapshot.fleet.run_stats;
            assert_eq!(stats.sweeps, 2, "two real sweeps must be published");
            assert_eq!(
                stats.auto_resumed, 2,
                "both seeded stopped sessions were auto-resumed; the published \
                 counters must say so rather than defaulting to zero"
            );
        }
        other => panic!("a just-published snapshot must be Current, got {other:?}"),
    }
}

/// Why: the loop must publish an initial snapshot BEFORE parking on the timer,
/// so the console sees the supervisor immediately on start rather than after one
/// full interval, and must return cleanly on shutdown — never killed mid-sweep
/// (CLAUDE.md #534).
/// Test: this is the test.
#[tokio::test]
async fn supervisor_run_until_stops_cleanly() {
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
    };
    let metrics_file = dir.path().join("supervisor-metrics.json");
    let sup: Supervisor<StubClassifier> = Supervisor::new(mgr, cfg, None)
        .with_auto_resume_path(no_override(&dir))
        .with_metrics_path(&metrics_file);

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
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), sup.run_until(shutdown))
        .await
        .expect("run_until must return promptly after shutdown");
    result.expect("clean shutdown returns Ok");

    // The initial snapshot was published before the loop parked on the timer.
    let published = publish::read_at(&metrics_file)
        .expect("read")
        .expect("the loop published before parking on the timer");
    assert_eq!(published.fleet.stopped, 2);
}

/// BEHAVIORAL BAR (#6288), the daemon half: after real sweeps, the exact
/// function `console_metrics` and `supervisor_status` call reports NON-ZERO
/// `run_stats` read out of the published file, and labels them `current`.
///
/// Why this and not `supervisor_status(&state)` directly: that entry point
/// resolves `FrameworkPaths::default()`, so asserting on it would read (and the
/// supervisor half would overwrite) the developer's live
/// `~/.trusty-mpm/supervisor-metrics.json`. `merge_supervisor_metrics` is the
/// whole of what `fleet_snapshot` adds on top of `FleetMetrics::from_records`,
/// driven here against a temp file. `supervisor_status_reports_fleet_and_auto_resume`
/// covers the wiring from the tool down to it.
/// Test: this is the test.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn supervisor_metrics_merge_reports_real_run_stats() {
    let dir = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let tmux = FakeTmux::new();
    let mgr = make_manager(&dir, tmux.clone()).await;
    seed_sessions(&mgr, 3, ManagedSessionState::Stopped, &ws).await;

    let cfg = SupervisorConfig {
        auto_resume: true,
        classify_idle: false,
        ..SupervisorConfig::default()
    };
    let metrics_file = dir.path().join("supervisor-metrics.json");
    let mut sup: Supervisor<StubClassifier> = Supervisor::new(mgr, cfg, None)
        .with_auto_resume_path(no_override(&dir))
        .with_metrics_path(&metrics_file);
    sup.tick().await;
    sup.publish_snapshot().await;

    // What the daemon does: derive the fleet from the session store (run_stats
    // default), then merge in what the supervisor published.
    let mut fleet = FleetMetrics::default();
    assert_eq!(
        fleet.run_stats.sweeps, 0,
        "precondition: the daemon starts from zeroed counters"
    );
    let block =
        crate::daemon::mcp_console::merge_supervisor_metrics(&mut fleet, &metrics_file, Utc::now());

    assert_eq!(block["status"], "current");
    assert_eq!(
        fleet.run_stats.sweeps, 1,
        "the daemon must report the supervisor's real sweep count, not zero"
    );
    assert_eq!(
        fleet.run_stats.auto_resumed, 3,
        "the daemon must report the supervisor's real auto-resume count: {block}"
    );
}
