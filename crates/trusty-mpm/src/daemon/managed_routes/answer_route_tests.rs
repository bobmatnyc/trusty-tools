//! The `/answer` route body under racing and failing answers (#9841).
//!
//! Why: the front gate's answer launches the withheld runtime, and two
//! answers racing it launched it twice: the route tested the decision with no
//! lock before clearing it.
//! What: drives [`answer_core_with`] over an [`AnswerDriver`]-backed daemon,
//! with the runtime launch replaced by a counting closure that can hold the
//! first launch until a second arrives.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;
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
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership_location::write_sentinel_bytes;
use crate::session_manager::{
    AnswerOutcome, ManagedSessionId, ManagedSessionState, SessionManager, SessionRecord,
};

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
    let h = Harness::new(ManagedSessionState::Provisioning).await;
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

/// #9843 (M2): a pending decision on a record that is not at its front gate
/// (an `Active` one from the pre-fix reconcile, or a `Stopped` one) is a 409
/// telling the operator to decommission; nothing is typed or launched, and
/// the decision and its hold stay.
#[tokio::test]
async fn a_pending_decision_off_the_front_gate_is_409_decommission() {
    for state in [ManagedSessionState::Active, ManagedSessionState::Stopped] {
        let h = Harness::new(state.clone()).await;
        h.ask().await;
        let before = h.record().await.live_decision_id();
        let launches = Arc::new(AtomicUsize::new(0));

        for answer in ["no", "yes"] {
            let l = Arc::clone(&launches);
            let out = h.answer(request(answer), move |_| counted(l)).await;
            assert_eq!(out.status, 409, "{state:?} {answer:?}: {out:?}");
            assert!(text(&out).contains("decommission"), "{out:?}");
        }

        assert!(
            h.driver.sends().is_empty(),
            "{state:?} typed: {:?}",
            h.driver.sends()
        );
        assert_eq!(launches.load(Ordering::SeqCst), 0, "{state:?}");
        let record = h.record().await;
        assert!(before.is_some());
        assert_eq!(record.live_decision_id(), before, "{state:?}");
        assert!(record.front_gate_hold().is_some(), "{state:?}");
        let mgr = h.state.session_manager().await;
        assert!(mgr.resume_inner(&h.id).await.is_err(), "{state:?}");
    }
}

/// #9843 (FO-3): a deny whose teardown fails is a 500, launches nothing, and
/// leaves a record nothing can launch: a repeat is a 409 "(denied)", and a
/// resume is refused even after the record is stopped.
#[tokio::test]
async fn front_gate_deny_teardown_failure_is_500_and_stays_unlaunchable() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let mgr = h.state.session_manager().await;
    // An Active sibling claiming the same OWNED workspace makes decommission
    // refuse (#9843 M3: an unowned one would not be checked).
    let mut owned = h.record().await;
    owned.workspace_owned = true;
    mgr.store.write().await.upsert(owned).await.expect("own ws");
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
    let err = resumed.expect_err("a denied task became resumable");
    assert!(err.to_string().contains("denied the launch"), "{err}");
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
/// gated task resumable; the decision stays answerable. A stop does not
/// either: resume itself refuses a record whose gate is still pending.
#[tokio::test]
async fn a_restart_never_makes_a_pending_front_gate_decision_resumable() {
    use crate::session_manager::ManagedTmuxDriver as _;

    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let before = h.record().await;
    let mgr = h.state.session_manager().await;
    // The pane died with the daemon.
    h.driver
        .inner
        .kill_session(&before.tmux_name)
        .expect("kill pane");
    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let resumed = mgr.resume_inner(&h.id).await;
    let after = h.record().await;
    assert!(
        resumed.is_err(),
        "resume launched a task its front gate holds: {:?}",
        after.state
    );
    assert_eq!(after.state, ManagedSessionState::Provisioning);
    assert_eq!(after.live_decision_id(), before.live_decision_id());
    let launches = Arc::new(AtomicUsize::new(0));
    let l = Arc::clone(&launches);
    let out = h.answer(request("no"), move |_| counted(l)).await;
    assert_eq!(out.status, 200, "{out:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 0);

    let stopped = Harness::new(ManagedSessionState::Provisioning).await;
    stopped.ask().await;
    let mgr = stopped.state.session_manager().await;
    mgr.stop_reporting(&stopped.id, crate::session_manager::StopCause::Deliberate)
        .await
        .expect("stop");
    let err = mgr
        .resume_inner(&stopped.id)
        .await
        .expect_err("resume launched a stopped task its front gate holds");
    assert!(err.to_string().contains("still pending"), "{err}");
}

/// A relauncher that counts and starts nothing, for the auto-resume path.
#[derive(Default)]
struct CountingRelauncher(AtomicUsize);

#[async_trait::async_trait]
impl crate::session_manager::relaunch::RuntimeRelauncher for CountingRelauncher {
    async fn relaunch(&self, _record: &SessionRecord) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Read `id` under the store guard until a claim has taken its decision; the
/// record seen is what a crash right after the claim's write leaves on disk.
async fn first_post_claim(mgr: Arc<SessionManager>, id: ManagedSessionId) -> SessionRecord {
    for _ in 0..20_000 {
        {
            let mut store = mgr.store.write().await;
            if let Ok(r) = store.get(&id).await
                && r.pending_decision.is_none()
            {
                return r;
            }
        }
        tokio::task::yield_now().await;
    }
    panic!("the claim never landed");
}

/// The rounds [`a_front_gate_deny_is_denied_in_the_claim_write`] runs, so the
/// reader lands between two writes if there are two.
const DENY_ROUNDS: usize = 8;

/// #9843 (item 1): a front-gate deny is `Denied` in the claim's own write.
/// A crash right after that write, followed by a restart, a stop, and a
/// manual and an automatic resume, launches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_front_gate_deny_is_denied_in_the_claim_write() {
    use crate::session_manager::ManagedTmuxDriver as _;

    for round in 0..DENY_ROUNDS {
        let h = Harness::new(ManagedSessionState::Provisioning).await;
        h.ask().await;
        let mgr = h.state.session_manager().await;
        let reader = tokio::spawn(first_post_claim(Arc::clone(&mgr), h.id));
        let launches = Arc::new(AtomicUsize::new(0));

        let l = Arc::clone(&launches);
        h.answer(request("no"), move |_| counted(l)).await;
        let crashed = reader.await.expect("join");

        assert_eq!(
            crashed.last_answer.as_ref().map(|a| a.outcome),
            Some(AnswerOutcome::Denied),
            "round {round}: the deny's claim was persisted as {:?}; a crash there leaves a launchable record",
            crashed.last_answer
        );
        // The crash: the record as the claim left it, the pane gone with the
        // daemon, then a restart.
        mgr.store
            .write()
            .await
            .upsert_decision(crashed.clone())
            .await
            .expect("restore");
        let _ = h.driver.inner.kill_session(&crashed.tmux_name);
        mgr.reconcile_on_boot(false).await.expect("reconcile");
        let _ = mgr
            .stop_reporting(&h.id, crate::session_manager::StopCause::Unexpected)
            .await;
        let relauncher = Arc::new(CountingRelauncher::default());
        mgr.install_relauncher(relauncher.clone());

        let manual = mgr.resume_inner(&h.id).await;
        let auto = mgr.resume_auto(&h.id).await;
        assert!(
            manual.is_err() && auto.is_err(),
            "round {round}: {manual:?} / {auto:?}"
        );
        assert_eq!(relauncher.0.load(Ordering::SeqCst), 0, "round {round}");
        assert_eq!(launches.load(Ordering::SeqCst), 0, "round {round}");
    }
}

/// #9843 (M3): a deny on a launch-on-main session closes it although an
/// Active sibling runs in the same main checkout; nothing is removed there.
#[tokio::test]
async fn a_launch_on_main_deny_ignores_an_active_sibling_on_main() {
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let mgr = h.state.session_manager().await;
    let main = h.record().await.workspace_path.expect("workspace");
    std::fs::write(main.join("README.md"), "main\n").expect("write");
    let sibling = seed_answerable(&mgr, &h._root, ManagedSessionState::Active).await;
    mgr.set_workspace(&sibling, main.clone(), ManagedSessionState::Active)
        .await
        .expect("sibling on main");
    let launches = Arc::new(AtomicUsize::new(0));

    let l = Arc::clone(&launches);
    let out = h.answer(request("no"), move |_| counted(l)).await;

    assert_eq!(out.status, 200, "{out:?}");
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert_eq!(h.record().await.state, ManagedSessionState::Decommissioned);
    assert_eq!(
        std::fs::read_to_string(main.join("README.md")).expect("main kept"),
        "main\n"
    );
}

/// A tm-created session worktree whose only dirt is the `TASK.md` tm wrote
/// for the seeded task, set as `h`'s workspace.
async fn provisioned_worktree(h: &Harness, fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    write_sentinel_bytes(&wt, b"").expect("ownership marker");
    let mut record = h.record().await;
    std::fs::write(wt.join("TASK.md"), &record.task).expect("TASK.md");
    record.workspace_path = Some(wt.clone());
    let mgr = h.state.session_manager().await;
    mgr.store.write().await.upsert(record).await.expect("ws");
    wt
}

/// #9843 (M4): a deny removes the provisioned worktree the runtime never ran
/// in; a user's file in it still keeps it.
#[tokio::test]
async fn a_front_gate_deny_removes_the_provisioned_worktree() {
    let fx = GitWorktreeFixture::new();
    let h = Harness::new(ManagedSessionState::Provisioning).await;
    h.ask().await;
    let wt = provisioned_worktree(&h, &fx, "deny-9843").await;

    let out = h.answer(request("no"), |_| async { Ok(()) }).await;

    assert_eq!(out.status, 200, "{out:?}");
    assert!(!wt.exists(), "the provisioned worktree was kept");

    let kept = Harness::new(ManagedSessionState::Provisioning).await;
    kept.ask().await;
    let wt = provisioned_worktree(&kept, &fx, "deny-work-9843").await;
    std::fs::write(wt.join("notes.rs"), "// unsaved\n").expect("user work");

    let out = kept.answer(request("no"), |_| async { Ok(()) }).await;

    assert_eq!(out.status, 200, "{out:?}");
    assert!(wt.join("notes.rs").exists(), "a deny discarded user work");
}
