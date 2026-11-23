//! The handoff flow against fakes at the client boundary (#11650 items 1–5).

use std::sync::Arc;
use std::time::Duration;

use super::super::{
    CallError, Clients, HandoffArgs, HandoffError, Ledger, Outcome, Target, TmSessionState,
    TmSessionUse, handoff,
};
use super::{FakeTc, FakeTm, Seen, home};
use crate::assistants::AssistantHome;

fn args(target: Target, key: Option<&str>, task: &str) -> HandoffArgs {
    HandoffArgs {
        instance: "izzie".into(),
        target,
        key: key.map(str::to_string),
        task: task.into(),
    }
}

async fn run(
    home: &AssistantHome,
    tm: &FakeTm,
    tc: &FakeTc,
    a: &HandoffArgs,
) -> Result<super::super::HandoffReport, HandoffError> {
    handoff(home, a, Clients { tm, tc }).await
}

async fn outcome(home: &AssistantHome, key: &str) -> Outcome {
    Ledger::open(&Ledger::path_for(home))
        .await
        .expect("ledger")
        .entries()
        .remove(key)
        .expect("key")
        .outcome
}

/// Why: item 1 — the task text reaches either target byte for byte.
/// Test: itself.
#[tokio::test]
async fn the_task_text_reaches_the_target_unchanged() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    let task = "  --to tc; rm -rf /tmp/x\n\"quoted\" `tick` $HOME  ";
    run(&home, &tm, &tc, &args(Target::Tc, None, task))
        .await
        .expect("tc");
    run(&home, &tm, &tc, &args(Target::Tm, None, task))
        .await
        .expect("tm");
    assert_eq!(
        tc.seen(),
        vec![Seen::Run {
            task: task.into(),
            project: None
        }]
    );
    assert!(matches!(&tm.seen()[0], Seen::New { task: t, .. } if t == task));
}

/// Why: item 2 — the first tm handoff creates one durable session, in the
/// instance's project (its home when none is configured), and the ledger
/// records its id.
/// Test: itself.
#[tokio::test]
async fn the_first_tm_handoff_creates_a_durable_session() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    let report = run(&home, &tm, &tc, &args(Target::Tm, Some("k1"), "first"))
        .await
        .expect("handoff");
    assert_eq!(report.handle, "tm-1");
    assert_eq!(report.tm_session, Some(TmSessionUse::Created));
    assert!(!report.replayed);
    assert_eq!(
        tm.seen(),
        vec![Seen::New {
            repo: home.path().to_path_buf(),
            task: "first".into()
        }]
    );
    let ledger = Ledger::open(&Ledger::path_for(&home)).await.expect("open");
    assert_eq!(ledger.tm_session().as_deref(), Some("tm-1"));
}

/// Why: item 2 — later handoffs of the instance send into the same session.
/// Test: itself.
#[tokio::test]
async fn the_second_tm_handoff_reuses_the_session() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    run(&home, &tm, &tc, &args(Target::Tm, None, "first"))
        .await
        .expect("first");
    // A tc handoff in between does not disturb the tm session.
    run(&home, &tm, &tc, &args(Target::Tc, None, "other"))
        .await
        .expect("tc");
    let report = run(&home, &tm, &tc, &args(Target::Tm, None, "second"))
        .await
        .expect("second");
    assert_eq!(
        (report.handle.as_str(), report.tm_session),
        ("tm-1", Some(TmSessionUse::Reused))
    );
    assert_eq!(
        tm.seen()[1..],
        [
            Seen::State { id: "tm-1".into() },
            Seen::Send {
                id: "tm-1".into(),
                text: "second".into()
            }
        ]
    );
}

/// Why: item 2 — a session tm reports gone is replaced by a new one, and the
/// replacement is recorded and reported, never silent.
/// Test: itself.
#[tokio::test]
async fn a_gone_tm_session_is_replaced_and_reported() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    run(&home, &tm, &tc, &args(Target::Tm, None, "first"))
        .await
        .expect("first");
    tm.set_state(
        "tm-1",
        TmSessionState::Gone {
            reason: "tm reports the session decommissioned".into(),
        },
    );
    let report = run(&home, &tm, &tc, &args(Target::Tm, Some("k2"), "second"))
        .await
        .expect("second");
    assert_eq!(report.handle, "tm-2");
    assert_eq!(
        report.tm_session,
        Some(TmSessionUse::Replaced {
            previous: "tm-1".into(),
            reason: "tm reports the session decommissioned".into()
        })
    );
    assert!(
        matches!(outcome(&home, "k2").await, Outcome::Dispatched { ref handle, .. } if handle == "tm-2")
    );
    let third = run(&home, &tm, &tc, &args(Target::Tm, None, "third"))
        .await
        .expect("third");
    assert_eq!(
        (third.handle.as_str(), third.tm_session),
        ("tm-2", Some(TmSessionUse::Reused))
    );
}

/// Why: item 2 — a stopped session still exists and is resumable, so it is
/// reported as a failure naming the resume command, not replaced.
/// Test: itself.
#[tokio::test]
async fn a_stopped_tm_session_is_reported_not_replaced() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    run(&home, &tm, &tc, &args(Target::Tm, None, "first"))
        .await
        .expect("first");
    tm.set_state(
        "tm-1",
        TmSessionState::Parked {
            state: "stopped".into(),
        },
    );
    let err = run(&home, &tm, &tc, &args(Target::Tm, Some("k2"), "second"))
        .await
        .expect_err("parked");
    assert!(matches!(err, HandoffError::Failed { .. }), "{err:?}");
    assert!(err.to_string().contains("tm session resume tm-1"), "{err}");
    assert_eq!(tm.seen().len(), 2, "{:?}", tm.seen());
    assert!(matches!(outcome(&home, "k2").await, Outcome::Failed { .. }));
}

/// Why: item 3 — a tc handoff calls `task.run` with the instance's project
/// and records the returned handle.
/// Test: itself.
#[tokio::test]
async fn a_tc_handoff_records_the_task_run_handle() {
    let (_root, home) = home();
    let project = tempfile::tempdir().expect("project");
    std::fs::write(
        home.config_path(),
        format!(
            "id = \"izzie\"\nproject_dir = \"{}\"\n",
            project.path().display()
        ),
    )
    .expect("config");
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    let report = run(&home, &tm, &tc, &args(Target::Tc, Some("k"), "build it"))
        .await
        .expect("tc");
    assert_eq!((report.handle.as_str(), report.tm_session), ("tc-1", None));
    assert_eq!(
        tc.seen(),
        vec![Seen::Run {
            task: "build it".into(),
            project: Some(project.path().to_path_buf())
        }]
    );
    assert_eq!(
        outcome(&home, "k").await,
        Outcome::Dispatched {
            handle: "tc-1".into(),
            tm_session: None
        }
    );
    assert!(tm.seen().is_empty());
}

/// Why: item 4 — a retried handoff with the same key returns the recorded
/// handle and dispatches nothing.
/// Test: itself.
#[tokio::test]
async fn a_retried_key_does_not_dispatch_twice() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    let a = args(Target::Tc, Some("same"), "once");
    let first = run(&home, &tm, &tc, &a).await.expect("first");
    let again = run(&home, &tm, &tc, &a).await.expect("again");
    assert_eq!((again.handle, again.replayed), (first.handle, true));
    assert_eq!(tc.seen().len(), 1);

    let err = run(
        &home,
        &tm,
        &tc,
        &args(Target::Tc, Some("same"), "other task"),
    )
    .await
    .expect_err("conflict");
    assert!(matches!(err, HandoffError::KeyConflict { .. }), "{err:?}");
    assert_eq!(tc.seen().len(), 1);
}

/// Why: item 4 — concurrent retries of one key dispatch once; the ledger
/// lock spans the whole handoff.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_retries_of_one_key_dispatch_once() {
    let (_root, home) = home();
    let tm = Arc::new(FakeTm::default());
    let tc = Arc::new(FakeTc {
        delay: Some(Duration::from_millis(50)),
        ..FakeTc::default()
    });
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let (home, tm, tc) = (home.clone(), Arc::clone(&tm), Arc::clone(&tc));
            tokio::spawn(async move {
                let a = args(Target::Tc, Some("race"), "once");
                handoff(&home, &a, Clients { tm: &*tm, tc: &*tc }).await
            })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.expect("join").expect("handoff").handle, "tc-1");
    }
    assert_eq!(tc.seen().len(), 1);
}

/// Why: item 4 — an intent left by a crash reads `outcome_unknown`, and a
/// retry of that key never dispatches.
/// Test: itself.
#[tokio::test]
async fn an_open_intent_reads_outcome_unknown_and_never_dispatches() {
    let (_root, home) = home();
    std::fs::create_dir_all(home.stores_dir()).expect("stores");
    std::fs::write(
        Ledger::path_for(&home),
        "{\"kind\":\"intent\",\"v\":1,\"key\":\"crashed\",\"at\":\"t\",\"target\":\"tc\",\"task\":\"x\"}\n",
    )
    .expect("write");
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    let err = run(&home, &tm, &tc, &args(Target::Tc, Some("crashed"), "x"))
        .await
        .expect_err("unknown");
    assert!(
        matches!(err, HandoffError::OutcomeUnknown { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("outcome_unknown"), "{err}");
    assert!(tc.seen().is_empty());
}

/// Why: Fail-Open Check — a dispatch whose effect is unknown is recorded and
/// reported as `outcome_unknown`, and its key does not dispatch again.
/// Test: itself.
#[tokio::test]
async fn an_unknown_dispatch_is_recorded_as_outcome_unknown() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    tc.fail_next(CallError::Unknown("read timed out".into()));
    let a = args(Target::Tc, Some("k"), "x");
    let err = run(&home, &tm, &tc, &a).await.expect_err("unknown");
    assert!(
        matches!(err, HandoffError::OutcomeUnknown { .. }),
        "{err:?}"
    );
    assert_eq!(
        outcome(&home, "k").await,
        Outcome::OutcomeUnknown {
            error: "read timed out".into()
        }
    );
    run(&home, &tm, &tc, &a).await.expect_err("still unknown");
    assert_eq!(tc.seen().len(), 1);

    // tm: a send the target did not confirm is unknown too.
    tm.fail_send
        .lock()
        .expect("fail")
        .push_back(CallError::Unknown("unsubmitted_paste".into()));
    run(&home, &tm, &tc, &args(Target::Tm, None, "a"))
        .await
        .expect("create");
    let err = run(&home, &tm, &tc, &args(Target::Tm, None, "b"))
        .await
        .expect_err("send unknown");
    assert!(
        matches!(err, HandoffError::OutcomeUnknown { .. }),
        "{err:?}"
    );
}

/// Why: Fail-Open Check — a refused dispatch is recorded as failed, never
/// success, and the same key may then dispatch.
/// Test: itself.
#[tokio::test]
async fn a_failed_dispatch_is_recorded_and_may_be_retried() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    tc.fail_next(CallError::NotSent("no tcode daemon".into()));
    let a = args(Target::Tc, Some("k"), "x");
    let err = run(&home, &tm, &tc, &a).await.expect_err("failed");
    assert!(matches!(err, HandoffError::Failed { .. }), "{err:?}");
    assert!(matches!(outcome(&home, "k").await, Outcome::Failed { .. }));
    let report = run(&home, &tm, &tc, &a).await.expect("retry");
    assert_eq!((report.handle.as_str(), report.replayed), ("tc-2", false));

    tm.fail_new
        .lock()
        .expect("fail")
        .push_back(CallError::NotSent("refused".into()));
    run(&home, &tm, &tc, &args(Target::Tm, None, "a"))
        .await
        .expect_err("tm refused");
    let ledger = Ledger::open(&Ledger::path_for(&home)).await.expect("open");
    assert_eq!(
        ledger.tm_session(),
        None,
        "a failed create records no session"
    );
}

/// Why: item 4 — the intent is written before the dispatch; when it cannot
/// be written nothing is dispatched.
/// Test: itself.
#[tokio::test]
async fn an_intent_write_failure_dispatches_nothing() {
    let (_root, home) = home();
    // `stores/` is a file, so the ledger cannot be created.
    std::fs::write(home.stores_dir(), "not a dir").expect("block stores");
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    let err = run(&home, &tm, &tc, &args(Target::Tc, None, "x"))
        .await
        .expect_err("ledger");
    assert!(matches!(err, HandoffError::LedgerIo { .. }), "{err:?}");
    assert!(tc.seen().is_empty() && tm.seen().is_empty());
}

/// Why: empty tasks and bad keys are refused before the ledger is touched.
/// Test: itself.
#[tokio::test]
async fn keys_are_validated() {
    let (_root, home) = home();
    let (tm, tc) = (FakeTm::default(), FakeTc::default());
    for key in ["", "a\nb", &"k".repeat(129)] {
        let err = run(&home, &tm, &tc, &args(Target::Tc, Some(key), "x"))
            .await
            .expect_err("bad key");
        assert!(
            matches!(err, HandoffError::InvalidKey(_)),
            "{key:?}: {err:?}"
        );
    }
    let err = run(&home, &tm, &tc, &args(Target::Tc, None, " \n "))
        .await
        .expect_err("empty");
    assert!(matches!(err, HandoffError::EmptyTask), "{err:?}");
    assert!(!Ledger::path_for(&home).exists());
    assert!(tc.seen().is_empty());
}
