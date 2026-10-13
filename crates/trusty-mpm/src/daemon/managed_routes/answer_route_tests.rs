//! The `/answer` route body under racing and failing answers (#9841).
//!
//! Why: the front gate's answer launches the withheld runtime, and two
//! answers racing it launched it twice: the route tested the decision with no
//! lock before clearing it.
//! What: drives [`answer_core_with`] over an [`AnswerDriver`]-backed daemon,
//! with the runtime launch replaced by a counting closure that can hold the
//! first launch until a second arrives.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use super::AnswerRequest;
use super::cores::answer_core_with;
use super::summary::{record_to_json, record_to_summary};
use crate::daemon::rpc::managed::outcome::{RouteBody, RouteOutcome};
use crate::daemon::state::DaemonState;
use crate::session_manager::decision_tests::{AnswerDriver, seed_answerable};
use crate::session_manager::{AnswerOutcome, ManagedSessionId, ManagedSessionState, SessionRecord};

/// A daemon over an [`AnswerDriver`] with one answerable record.
struct Harness {
    _root: TempDir,
    state: Arc<DaemonState>,
    driver: Arc<AnswerDriver>,
    id: ManagedSessionId,
}

impl Harness {
    async fn new(state: ManagedSessionState) -> Self {
        let root = TempDir::new().unwrap();
        let driver = AnswerDriver::new();
        let daemon = Arc::new(
            DaemonState::with_root_isolated_managed_and_driver(
                root.path().to_path_buf(),
                driver.clone(),
            )
            .await,
        );
        let mgr = daemon.session_manager().await;
        let id = seed_answerable(&mgr, &root, state).await;
        Self {
            _root: root,
            state: daemon,
            driver,
            id,
        }
    }

    async fn ask(&self) {
        self.ask_with(Some("no")).await;
    }

    async fn ask_with(&self, proposed_default: Option<&str>) {
        let mgr = self.state.session_manager().await;
        mgr.set_pending_decision(&self.id, "run rm -rf build?", proposed_default)
            .await
            .expect("set_pending_decision");
    }

    async fn record(&self) -> SessionRecord {
        let mgr = self.state.session_manager().await;
        mgr.get(&self.id).await.expect("record")
    }

    async fn answer<F, Fut>(&self, req: AnswerRequest, spawn: F) -> RouteOutcome
    where
        F: FnOnce(SessionRecord) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        answer_core_with(&self.state, &self.id.to_string(), req, "http", spawn).await
    }
}

fn request(answer: &str) -> AnswerRequest {
    AnswerRequest {
        answer: answer.to_owned(),
        ..AnswerRequest::default()
    }
}

fn text(outcome: &RouteOutcome) -> String {
    match &outcome.body {
        RouteBody::Text(t) => t.clone(),
        RouteBody::Json(v) => v.to_string(),
    }
}

/// Count one launch, then wait (bounded) until a second launch arrives.
async fn held_launch(launches: Arc<AtomicUsize>) -> Result<(), String> {
    launches.fetch_add(1, Ordering::SeqCst);
    for _ in 0..50 {
        if launches.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// #9841 (D6): two answers racing one front-gate decision launch the
/// runtime once; the other gets the 409 conflict, and nothing is typed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_front_gate_answers_spawn_once() {
    let h = Arc::new(Harness::new(ManagedSessionState::Provisioning).await);
    h.ask().await;
    let launches = Arc::new(AtomicUsize::new(0));

    let race = |h: Arc<Harness>, launches: Arc<AtomicUsize>| {
        tokio::spawn(async move {
            h.answer(request("yes"), move |_| held_launch(launches))
                .await
        })
    };
    let first = race(Arc::clone(&h), Arc::clone(&launches));
    let second = race(Arc::clone(&h), Arc::clone(&launches));
    let (a, b) = (first.await.expect("join"), second.await.expect("join"));

    assert_eq!(launches.load(Ordering::SeqCst), 1, "{a:?} / {b:?}");
    assert!(h.driver.sends().is_empty(), "typed: {:?}", h.driver.sends());
    let mut statuses = [a.status, b.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "{a:?} / {b:?}");
    let refused = if a.status == 409 { &a } else { &b };
    assert!(
        text(refused).contains("already answered by http at "),
        "{refused:?}"
    );
}

/// #9841 (D4): a failed launch closes the decision; a repeat is a 409 that
/// says so, and launches nothing. The failing launch marks the record errored,
/// as `spawn_runtime_for` does, so the repeat meets a session whose pane
/// cannot be proven: the conflict must still be the answer.
#[tokio::test]
async fn front_gate_launch_failure_closes_the_decision() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let launches = Arc::new(AtomicUsize::new(0));
    *h.driver.inner.pane_exists_override.lock().unwrap() = Some(false);

    let (l, m) = (Arc::clone(&launches), h.state.session_manager().await);
    let failed = h
        .answer(request("yes"), move |record| async move {
            l.fetch_add(1, Ordering::SeqCst);
            m.mark_errored(&record.id, "spawn failed: fake")
                .await
                .map_err(|e| e.to_string())?;
            Err("fake: claude binary not found".to_string())
        })
        .await;
    assert_eq!(failed.status, 500, "{failed:?}");
    assert!(
        text(&failed).contains("fake: claude binary not found"),
        "{failed:?}"
    );

    let l = Arc::clone(&launches);
    let repeat = h.answer(request("yes"), move |_| held_launch(l)).await;
    assert_eq!(repeat.status, 409, "{repeat:?}");
    assert!(text(&repeat).contains("(delivery failed)"), "{repeat:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert!(h.driver.sends().is_empty(), "typed: {:?}", h.driver.sends());
}

/// #9841 (D1, D73 ruling: strict): once the front gate's runtime launched,
/// a later `/approve` has no decision to answer: a 409 naming the first
/// answer, and nothing typed.
#[tokio::test]
async fn a_delivered_front_gate_answer_leaves_nothing_to_answer() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let mgr = h.state.session_manager().await;
    let (m, id) = (Arc::clone(&mgr), h.id);
    let launched = h
        .answer(request("yes"), move |record| async move {
            let ws = record.workspace_path.unwrap_or(record.cwd);
            m.set_workspace(&id, ws, ManagedSessionState::Active)
                .await
                .map_err(|e| e.to_string())
        })
        .await;
    assert_eq!(launched.status, 200, "{launched:?}");

    let live = h.answer(request("/approve"), |_| async { Ok(()) }).await;
    assert_eq!(live.status, 409, "{live:?}");
    assert!(
        text(&live).contains("already answered by http at "),
        "{live:?}"
    );
    assert!(h.driver.sends().is_empty(), "typed: {:?}", h.driver.sends());
}

/// #9841: a `decision_id` that is not a UUID is a 400 naming it; nothing is
/// claimed or typed.
#[tokio::test]
async fn a_malformed_decision_id_is_400() {
    let h = Harness::new(ManagedSessionState::Active).await;
    h.ask().await;
    let req = AnswerRequest {
        decision_id: Some("not-a-uuid".into()),
        ..request("yes")
    };
    let out = h.answer(req, |_| async { Ok(()) }).await;
    assert_eq!(out.status, 400, "{out:?}");
    assert!(text(&out).contains("not-a-uuid"), "{out:?}");
    assert!(h.record().await.pending_decision.is_some());
    assert!(h.driver.sends().is_empty());
}

/// #9841 (D7): the list (HTTP summary and MCP JSON) carries the pending
/// decision's id, and `null` once it is answered.
#[tokio::test]
async fn list_exposes_pending_decision_id() {
    let h = Harness::new(ManagedSessionState::Active).await;
    h.ask().await;
    let record = h.record().await;
    let want = serde_json::json!(record.pending_decision_id.map(|d| d.to_string()));
    assert!(
        !want.is_null(),
        "set_pending_decision must mint a decision id"
    );
    let summary = serde_json::to_value(record_to_summary(&record)).expect("summary");
    assert_eq!(summary["pending_decision_id"], want);
    assert_eq!(record_to_json(&record)["pending_decision_id"], want);

    let out = h.answer(request("yes"), |_| async { Ok(()) }).await;
    assert_eq!(out.status, 200, "{out:?}");
    let record = h.record().await;
    assert!(record_to_json(&record)["pending_decision_id"].is_null());
}

/// Count one launch and start nothing.
async fn counted(launches: Arc<AtomicUsize>) -> Result<(), String> {
    launches.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

/// #9843 (AC-A): a deny to the front gate closes the decision, launches
/// nothing, types nothing, and decommissions the session.
#[tokio::test]
async fn front_gate_deny_closes_the_decision_without_launch() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask_with(None).await;
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let out = h.answer(request("deny"), move |_| counted(l)).await;

    assert_eq!(
        launches.load(Ordering::SeqCst),
        0,
        "a deny launched: {out:?}"
    );
    assert_eq!(out.status, 200, "{out:?}");
    assert!(h.driver.sends().is_empty(), "typed: {:?}", h.driver.sends());
    let record = h.record().await;
    assert!(record.pending_decision.is_none());
    assert!(record.live_decision_id().is_none());
    assert_eq!(
        record.last_answer.as_ref().map(|a| a.outcome),
        Some(AnswerOutcome::Denied)
    );
    assert_eq!(record.state, ManagedSessionState::Decommissioned);
    let mgr = h.state.session_manager().await;
    assert!(mgr.resume_inner(&h.id).await.is_err());
}

/// #9843 (AC-B): after a deny, any answer is a 409 that says it was denied,
/// and nothing launches.
#[tokio::test]
async fn a_repeat_after_a_front_gate_deny_is_409_denied() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let first = h.answer(request("no"), move |_| counted(l)).await;
    let l = Arc::clone(&launches);
    let repeat = h.answer(request("yes"), move |_| counted(l)).await;

    assert_eq!(repeat.status, 409, "{first:?} / {repeat:?}");
    assert!(text(&repeat).contains("(denied)"), "{repeat:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 0);
}

/// #9843 (AC-C, FO-4): front-gate text that is neither an approve nor a
/// deny is refused before the claim; the decision stays pending, same id.
#[tokio::test]
async fn an_unclear_front_gate_answer_is_refused_before_the_claim() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let before = h.record().await.live_decision_id();
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let out = h.answer(request("maybe later"), move |_| counted(l)).await;

    assert_eq!(out.status, 422, "{out:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    let record = h.record().await;
    assert!(before.is_some());
    assert_eq!(record.live_decision_id(), before);
    assert!(record.last_answer.is_none(), "{:?}", record.last_answer);
}

/// #9843 (AC-D): "no" denies even when the proposed default is itself "no".
#[tokio::test]
async fn deny_word_wins_over_a_proposed_default_of_no() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let out = h.answer(request(" No "), move |_| counted(l)).await;

    assert_eq!(launches.load(Ordering::SeqCst), 0, "{out:?}");
    assert_eq!(h.record().await.state, ManagedSessionState::Decommissioned);
}

/// #9843 (AC-D guard): "yes", "approve" and the exact proposed default each
/// launch the withheld runtime once.
#[tokio::test]
async fn front_gate_approve_and_proposed_default_still_launch() {
    for answer in ["yes", "approve", "use cursor"] {
        let h = Harness::new(ManagedSessionState::Provisioning).await;
        h.ask_with(Some("use cursor")).await;
        let launches = Arc::new(AtomicUsize::new(0));

        let l = Arc::clone(&launches);
        let out = h.answer(request(answer), move |_| counted(l)).await;

        assert_eq!(out.status, 200, "{answer:?}: {out:?}");
        assert_eq!(launches.load(Ordering::SeqCst), 1, "{answer:?}");
        assert!(h.driver.sends().is_empty(), "{answer:?}");
    }
}

/// #9843 (AC-E guard): on a live harness, "no" is still typed into the
/// proven pane; only the front gate reads the text.
#[tokio::test]
async fn live_decision_deny_still_types_no() {
    let h = Harness::new(ManagedSessionState::Active).await;
    h.ask().await;
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let out = h.answer(request("no"), move |_| counted(l)).await;

    assert_eq!(out.status, 200, "{out:?}");
    assert_eq!(h.driver.sends(), vec!["no".to_string()]);
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert_eq!(h.record().await.state, ManagedSessionState::Active);
}

/// #9843 (FO-3): a deny whose teardown fails is a 500, launches nothing, and
/// leaves a record nothing can launch: a repeat is a 409 "(denied)", and a
/// resume is refused even after the record is stopped.
#[tokio::test]
async fn front_gate_deny_teardown_failure_is_500_and_stays_unlaunchable() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let mgr = h.state.session_manager().await;
    // An Active sibling claiming the same workspace makes decommission refuse.
    let ws = h.record().await.workspace_path.expect("workspace");
    let other = seed_answerable(&mgr, &h._root, ManagedSessionState::Active).await;
    mgr.set_workspace(&other, ws, ManagedSessionState::Active)
        .await
        .expect("sibling claim");
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let out = h.answer(request("no"), move |_| counted(l)).await;

    assert_eq!(launches.load(Ordering::SeqCst), 0, "{out:?}");
    assert_eq!(out.status, 500, "{out:?}");
    let l = Arc::clone(&launches);
    let repeat = h.answer(request("yes"), move |_| counted(l)).await;
    assert_eq!(repeat.status, 409, "{repeat:?}");
    assert!(text(&repeat).contains("(denied)"), "{repeat:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert!(mgr.resume_inner(&h.id).await.is_err());

    mgr.stop_reporting(&h.id, crate::session_manager::StopCause::Deliberate)
        .await
        .expect("stop");
    let resumed = mgr.resume_inner(&h.id).await;
    assert!(
        resumed.is_err(),
        "a denied task became resumable: {:?}",
        resumed.map(|r| r.state)
    );
}

/// #9843 (AC-F): a front-gate launch whose runtime never came up is a 500
/// that settles `DeliveryFailed`, not `Delivered`. Takes the production
/// verify budget (~5 s).
#[tokio::test]
async fn front_gate_runtime_that_never_started_settles_delivery_failed() {
    use super::front_gate_answer::launch_withheld;
    use super::front_gate_answer::tests::CountingAdapter;

    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask_with(None).await;
    h.driver.hooks.lock().unwrap().runtime_never_starts = true;
    let spec = TempDir::new().unwrap();
    let (mgr, spec_dir) = (h.state.session_manager().await, spec.path().to_path_buf());

    let out = h
        .answer(request("yes"), move |record| async move {
            let adapter = CountingAdapter::default();
            launch_withheld(&mgr, &record, &adapter, &[], Some(&spec_dir)).await
        })
        .await;

    assert_eq!(out.status, 500, "{out:?}");
    assert!(text(&out).contains("did not start"), "{out:?}");
    let record = h.record().await;
    assert_eq!(
        record.last_answer.as_ref().map(|a| a.outcome),
        Some(AnswerOutcome::DeliveryFailed)
    );
    assert_eq!(record.state, ManagedSessionState::Errored);
}

/// #9844: a restart while a front-gate decision is pending never makes the
/// gated task resumable; the decision stays answerable.
#[tokio::test]
async fn a_restart_never_makes_a_pending_front_gate_decision_resumable() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let before = h.record().await;
    let mgr = h.state.session_manager().await;
    // The pane died with the daemon.
    use crate::session_manager::ManagedTmuxDriver as _;
    h.driver
        .inner
        .kill_session(&before.tmux_name)
        .expect("kill pane");
    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let resumed = mgr.resume_inner(&h.id).await;

    assert!(
        resumed.is_err(),
        "resume launched a task its front gate holds: {:?}",
        resumed.map(|r| r.state)
    );
    let after = h.record().await;
    assert_eq!(after.live_decision_id(), before.live_decision_id());
    assert_eq!(after.state, ManagedSessionState::Provisioning);
    let launches = Arc::new(AtomicUsize::new(0));
    let l = Arc::clone(&launches);
    let out = h.answer(request("no"), move |_| counted(l)).await;
    assert_eq!(out.status, 200, "{out:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 0);
}
