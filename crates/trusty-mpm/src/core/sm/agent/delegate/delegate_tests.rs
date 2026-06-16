//! Acceptance tests for the SM-8 delegation loop (`delegate_goal`).
//!
//! Why: SM-8's acceptance criteria are the heart of the Session Manager — an
//! operator goal must (a) create a tracked goal LINKED to ≥1 launched session;
//! (b) observe sessions and update link state + goal progress; (c) be BLOCKED
//! from `Done` without observed evidence, then close once evidence is present
//! (the §3.5 gate); (d) REFUSE a direct-work attempt and redirect to launch (the
//! §3.2 SP guard); and (e) DELIVER the task to the launched session (#1299).
//! These tests pin every one of those deterministically with a mock provider
//! (scripted decision JSON), a mock `SessionControl`, and an in-memory goal store
//! — NO network, NO real spawn, NO ONNX.
//! What: builds an agent over a [`MockResolver`] whose provider replies with a
//! scripted decision, a [`MockSessionControl`], and an [`SmGoalStore`] over an
//! in-memory [`GoalMemory`] mock, then drives `delegate_goal`.
//! Test: this is the test module.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use tempfile::TempDir;
use tokio::sync::Mutex;

use super::mock_control::MockSessionControl;
use crate::core::sm::agent::SessionManagerAgent;
use crate::core::sm::agent::mock::{MockChatProvider, MockResolver};
use crate::core::sm::config::SessionManagerConfig;
use crate::core::sm::control::SessionControl;
use crate::core::sm::goals::{GOAL_TAG, GoalMemory, GoalStatus, SmGoalStore};

// ── In-memory goal palace (mock GoalMemory) ─────────────────────────────────────

/// A minimal in-memory [`GoalMemory`] for the delegation tests.
///
/// Why: the goal store needs a palace seam; a tiny in-memory upsert-by-id mock
/// keeps the loop tests free of the ONNX-backed palace while still exercising the
/// real store lifecycle (create/link/update/close gate).
/// What: upserts JSON by goal id; `list_goals` returns the current set.
struct MemPalace {
    entries: StdMutex<Vec<(String, String)>>,
}

impl MemPalace {
    fn arc() -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl GoalMemory for MemPalace {
    async fn remember_goal(&self, json: String, tag: &str) -> Result<(), String> {
        assert_eq!(tag, GOAL_TAG);
        let id = serde_json::from_str::<serde_json::Value>(&json)
            .ok()
            .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(str::to_string))
            .unwrap_or_default();
        let mut e = self.entries.lock().expect("lock");
        if let Some(slot) = e.iter_mut().find(|(eid, _)| *eid == id) {
            slot.1 = json;
        } else {
            e.push((id, json));
        }
        Ok(())
    }

    async fn list_goals(&self, _tag: &str) -> Result<Vec<String>, String> {
        Ok(self
            .entries
            .lock()
            .expect("lock")
            .iter()
            .map(|(_, j)| j.clone())
            .collect())
    }
}

// ── Harness ─────────────────────────────────────────────────────────────────────

/// Build an enabled SM config.
fn enabled_config() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build an agent over a mock resolver whose provider replies with `decision_json`.
///
/// Why: the loop's only LLM call is DECOMPOSE; scripting the provider reply lets a
/// test drive any decision path (delegate / respond / do_work) deterministically.
/// What: builds an agent via the feature-aware `with_runtime` over a
/// [`MockResolver`] returning a [`MockChatProvider`] that always replies with
/// `decision_json`.
fn agent_with(decision_json: &str, data_root: &std::path::Path) -> SessionManagerAgent {
    let provider = MockChatProvider::new(decision_json, 0.0);
    let resolver = Arc::new(MockResolver::with_provider(provider));
    #[cfg(feature = "sm-memory")]
    {
        SessionManagerAgent::with_runtime(enabled_config(), resolver, data_root.to_path_buf(), None)
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        SessionManagerAgent::with_runtime(enabled_config(), resolver, data_root.to_path_buf())
    }
}

/// Build an in-memory goal store handle for the loop.
fn goal_store(dir: &TempDir) -> Arc<Mutex<SmGoalStore>> {
    let palace = MemPalace::arc();
    Arc::new(Mutex::new(SmGoalStore::new(palace, dir.path())))
}

// ── Tests ───────────────────────────────────────────────────────────────────────

/// Why: the headline acceptance — an operator goal results in ≥1 launched session
/// LINKED to a `Goal` record. Proves INTAKE→DECOMPOSE→LAUNCH wired correctly.
/// What: scripts a single-task delegate decision, runs the loop, and asserts a
/// session launched, the goal carries the link, and the launch recorded the goal id.
/// Test: this is the test.
#[tokio::test]
async fn delegate_launches_and_links_session() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"delegate","tasks":[{"workdir":"/repo","prompt":"add login"}]}"#;
    let agent = agent_with(decision, tmp.path());
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("build the login feature", &control, &goals)
        .await
        .expect("delegation loop runs");

    // A session launched and is reported back.
    assert_eq!(outcome.launched.len(), 1, "exactly one session launched");

    // The launch recorded the goal id (so it links).
    let launches = mock.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(
        launches[0].1.goal_id.as_deref(),
        Some(outcome.goal_id.as_str())
    );

    // The goal carries the session link.
    let store = goals.lock().await;
    let goal = store.get(&outcome.goal_id).expect("goal exists");
    assert_eq!(goal.sessions.len(), 1, "session linked to goal");
    assert_eq!(goal.sessions[0].session_id, outcome.launched[0]);
    assert_eq!(goal.status, GoalStatus::InProgress);
}

/// Why: after LAUNCH the task must be DELIVERED to the session (#1299) — spawn
/// does not auto-inject the prompt, so without delivery the session sits idle.
/// What: runs the loop and asserts the mock session RECEIVED the task via `send`.
/// Test: this is the test.
#[tokio::test]
async fn delegate_delivers_task_to_session() {
    let tmp = TempDir::new().unwrap();
    let decision =
        r#"{"action":"delegate","tasks":[{"workdir":"/r","prompt":"implement the parser"}]}"#;
    let agent = agent_with(decision, tmp.path());
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("write a parser", &control, &goals)
        .await
        .expect("loop runs");

    let sends = mock.sends();
    assert_eq!(sends.len(), 1, "task delivered exactly once (#1299)");
    assert_eq!(
        sends[0].0, outcome.launched[0],
        "delivered to launched session"
    );
    assert_eq!(
        sends[0].1, "implement the parser",
        "the task prompt was delivered"
    );
}

/// Why: observation must update the link state and the goal progress. With a
/// running session and NO evidence, the link is `Running` and progress stays 0%.
/// What: default mock (running, no evidence); asserts link Running + progress 0
/// + goal not done.
/// Test: this is the test.
#[tokio::test]
async fn delegate_observes_and_updates_progress() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"delegate","tasks":[{"workdir":"/r","prompt":"task"}]}"#;
    let agent = agent_with(decision, tmp.path());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("do the thing", &control, &goals)
        .await
        .expect("loop runs");

    assert!(!outcome.goal_done, "no evidence ⇒ goal not done");
    // `goal_status` disambiguates the false `goal_done`: this goal is in flight.
    assert_eq!(
        outcome.goal_status, "InProgress",
        "an observed-but-unverified goal is InProgress, not blocked/failed"
    );
    let store = goals.lock().await;
    let goal = store.get(&outcome.goal_id).expect("goal");
    use crate::core::sm::goals::SessionTaskState;
    assert_eq!(goal.sessions[0].state, SessionTaskState::Running);
    assert_eq!(goal.progress, 0, "no verified tasks ⇒ 0% progress");
}

/// Why: THE verification gate (§3.5) — a goal CANNOT reach `Done` without observed
/// evidence. With a running, evidence-free session, the gated close must be refused
/// and the goal left open.
/// What: default mock (no evidence); asserts `goal_done == false` and the goal
/// status is NOT `Done`.
/// Test: this is the test.
#[tokio::test]
async fn delegate_gate_blocks_without_evidence() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"delegate","tasks":[{"workdir":"/r","prompt":"task"}]}"#;
    let agent = agent_with(decision, tmp.path());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("ship it", &control, &goals)
        .await
        .expect("loop runs");

    assert!(!outcome.goal_done, "gate blocks Done without evidence");
    let store = goals.lock().await;
    assert_ne!(
        store.get(&outcome.goal_id).unwrap().status,
        GoalStatus::Done,
        "goal must not be Done"
    );
    // And the honest report says so (no forbidden 'should be done' phrasing).
    let lower = outcome.reply.to_ascii_lowercase();
    assert!(!lower.contains("should be done"));
    assert!(!lower.contains("looks complete"));
}

/// Why: once observed evidence is present AND all tasks verified, the gated close
/// SUCCEEDS — the goal reaches `Done`. This is the positive end of the gate.
/// What: scripts a session whose pane carries a PR URL; asserts the link is
/// `Verified` with that evidence, progress is 100, and the goal closed Done.
/// Test: this is the test.
#[tokio::test]
async fn delegate_verifies_and_closes_with_evidence() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"delegate","tasks":[{"workdir":"/r","prompt":"open a PR"}]}"#;
    let agent = agent_with(decision, tmp.path());
    let evidence = "Opened PR https://github.com/acme/repo/pull/9 ready";
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::with_evidence(evidence));
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("open the PR", &control, &goals)
        .await
        .expect("loop runs");

    assert!(outcome.goal_done, "evidence present ⇒ gate passes ⇒ Done");
    let store = goals.lock().await;
    let goal = store.get(&outcome.goal_id).expect("goal");
    use crate::core::sm::goals::SessionTaskState;
    assert_eq!(goal.sessions[0].state, SessionTaskState::Verified);
    assert!(
        goal.sessions[0]
            .evidence
            .as_deref()
            .unwrap()
            .contains("pull/9"),
        "evidence captured into the link"
    );
    assert_eq!(goal.progress, 100);
    assert_eq!(goal.status, GoalStatus::Done);
    assert_eq!(outcome.goal_status, "Done", "closed goal reports Done");
    assert!(outcome.reply.to_ascii_lowercase().contains("done"));
}

/// Why: the PROHIBITION guard (§3.2 SP1–SP5) — a direct-work attempt must be
/// REFUSED and redirected to "launch a session", NOT performed. Proves the SM
/// does not do work itself even when its decision says to.
/// What: scripts a `do_work` decision; asserts NO session launched, the reply
/// redirects to launching a session, and the goal is not done.
/// Test: this is the test.
#[tokio::test]
async fn delegate_refuses_direct_work_and_redirects() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"do_work","summary":"I will edit main.rs myself"}"#;
    let agent = agent_with(decision, tmp.path());
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("just add the flag", &control, &goals)
        .await
        .expect("loop runs");

    // No work was done directly — nothing launched, and the reply redirects.
    assert!(
        outcome.launched.is_empty(),
        "direct work must NOT launch arbitrary work"
    );
    assert!(
        mock.launches().is_empty(),
        "control was never driven to do the work"
    );
    assert!(mock.sends().is_empty(), "no task delivered (no session)");
    assert!(!outcome.goal_done);
    let lower = outcome.reply.to_ascii_lowercase();
    assert!(
        lower.contains("launch a session"),
        "reply redirects to launching a session: {}",
        outcome.reply
    );
    assert!(lower.contains("sp1-sp5"), "reply names the prohibition");
    assert_eq!(
        outcome.goal_status, "Blocked",
        "a refused direct-work goal reports Blocked in goal_status"
    );
    // The refused goal is Blocked (awaiting operator re-issue), not an orphan Pending.
    let store = goals.lock().await;
    assert_eq!(
        store.get(&outcome.goal_id).unwrap().status,
        GoalStatus::Blocked,
        "a refused direct-work goal is Blocked, not left Pending"
    );
}

/// Why: a `respond` decision is an Allowlist-1 operator-facing reply (triage /
/// clarification) — the SM talks, launches nothing, goal stays Pending.
/// What: scripts a respond decision; asserts the message is returned and nothing
/// launched.
/// Test: this is the test.
#[tokio::test]
async fn delegate_respond_talks_to_operator() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"respond","message":"Which repository should I target?"}"#;
    let agent = agent_with(decision, tmp.path());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("do something vague", &control, &goals)
        .await
        .expect("loop runs");

    assert_eq!(outcome.reply, "Which repository should I target?");
    assert!(outcome.launched.is_empty());
    let store = goals.lock().await;
    assert_eq!(
        store.get(&outcome.goal_id).unwrap().status,
        GoalStatus::Pending
    );
}

/// Why: a goal may fan out to several sessions (§3.4); each must be launched,
/// linked, and delivered.
/// What: scripts a two-task delegate; asserts two sessions launched + two links +
/// two deliveries.
/// Test: this is the test.
#[tokio::test]
async fn delegate_fans_out_to_multiple_sessions() {
    let tmp = TempDir::new().unwrap();
    let decision = r#"{"action":"delegate","tasks":[
        {"workdir":"/r","prompt":"backend"},
        {"workdir":"/r","prompt":"frontend"}]}"#;
    let agent = agent_with(decision, tmp.path());
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let goals = goal_store(&tmp);

    let outcome = agent
        .delegate_goal("build the app", &control, &goals)
        .await
        .expect("loop runs");

    assert_eq!(outcome.launched.len(), 2);
    assert_eq!(mock.launches().len(), 2);
    assert_eq!(mock.sends().len(), 2, "both tasks delivered");
    let store = goals.lock().await;
    assert_eq!(store.get(&outcome.goal_id).unwrap().sessions.len(), 2);
}

/// Why: with no inference provider the DECOMPOSE reasoning cannot run; the loop
/// must degrade gracefully (a typed error), never panic.
/// What: builds an agent over a degraded resolver and asserts `Degraded`.
/// Test: this is the test.
#[tokio::test]
async fn delegate_degraded_without_provider() {
    use crate::core::sm::agent::DelegationError;
    let tmp = TempDir::new().unwrap();
    let resolver = Arc::new(MockResolver::degraded());
    #[cfg(feature = "sm-memory")]
    let agent = SessionManagerAgent::with_runtime(
        enabled_config(),
        resolver,
        tmp.path().to_path_buf(),
        None,
    );
    #[cfg(not(feature = "sm-memory"))]
    let agent =
        SessionManagerAgent::with_runtime(enabled_config(), resolver, tmp.path().to_path_buf());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let goals = goal_store(&tmp);

    let err = agent
        .delegate_goal("anything", &control, &goals)
        .await
        .expect_err("degraded loop errors gracefully");
    assert!(matches!(err, DelegationError::Degraded(_)));
}
