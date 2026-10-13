//! Whole-record writers that straddle a decision claim (#9841, critic MEDIUM).
//!
//! Why: `inject_into_owned`, `set_workspace`, `mark_errored` and
//! `set_claude_session_id` read the whole record, release the store guard,
//! and upsert the copy later. A claim landing in between was undone: the
//! copy put `pending_decision` back and dropped `last_answer`, and a retried
//! answer typed a second time.
//! What: each test lands a claim between a writer's read and its write, then
//! checks the claim survived, the writer's own field landed, and a retry is
//! a conflict. The send is paused by [`AnswerDriver`]'s park seam; the three
//! store-only writers are ordered by the store lock's FIFO queue (the writer
//! queues first, the claim second, so the claim runs between the writer's
//! read and its write). The last test lands a new decision inside a
//! decommission the same way.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use crate::core::sm::control::Submit;

use super::decision::AnswerClaim;
use super::decision_tests::{AnswerDriver, seed_answerable};
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// How long a spawned task gets to reach the store lock's queue.
const QUEUE: Duration = Duration::from_millis(50);

/// A manager over an [`AnswerDriver`] with one Active record and a pending
/// decision.
async fn pending() -> (
    TempDir,
    Arc<SessionManager>,
    Arc<AnswerDriver>,
    ManagedSessionId,
) {
    let dir = TempDir::new().unwrap();
    let driver = AnswerDriver::new();
    let mgr = Arc::new(
        SessionManager::new(dir.path(), driver.clone())
            .await
            .expect("manager"),
    );
    let id = seed_answerable(&mgr, &dir, ManagedSessionState::Active).await;
    mgr.set_pending_decision(&id, "merge or rebase?", Some("merge"))
        .await
        .expect("pending");
    (dir, mgr, driver, id)
}

/// Assert the claim on `id` survived and a retried claim is a conflict.
async fn assert_claim_survived(mgr: &SessionManager, id: &ManagedSessionId) -> SessionRecord {
    let record = mgr.get(id).await.expect("record");
    assert!(
        record.pending_decision.is_none() && record.pending_decision_id.is_none(),
        "a straddling write restored the claimed decision: {:?}",
        record.pending_decision
    );
    assert!(
        record.last_answer.is_some(),
        "a straddling write erased the claim"
    );
    let retry = mgr.claim_decision(id, &AnswerClaim::by("bob")).await;
    assert!(
        matches!(retry, Err(ManagedError::DecisionConflict(_))),
        "a retried answer must conflict: {retry:?}"
    );
    record
}

/// #9841: a `tm session send` in flight while an answer claims and delivers
/// cannot put the decision back, so a retried answer gets the 409 and types
/// nothing. The send is parked between its record read and its write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_in_flight_cannot_restore_a_claimed_decision() {
    let (_dir, mgr, driver, id) = pending().await;
    driver.hooks.lock().unwrap().park_text = Some("typed by send".into());

    let m = Arc::clone(&mgr);
    let send = tokio::spawn(async move { m.inject(&id, "typed by send", Submit::Enter).await });
    driver.wait_parked().await;
    mgr.answer_decision(&id, "merge", &AnswerClaim::by("alice"))
        .await
        .expect("the answer claims and delivers");
    driver.release();
    send.await.expect("join").expect("the send");

    assert_claim_survived(&mgr, &id).await;
    let retry = mgr
        .answer_decision(&id, "rebase", &AnswerClaim::by("bob"))
        .await;
    assert!(
        matches!(retry, Err(ManagedError::DecisionConflict(_))),
        "{retry:?}"
    );
    let sends = driver.sends();
    assert_eq!(
        sends.iter().filter(|s| *s == "merge").count(),
        1,
        "{sends:?}"
    );
    assert!(
        !sends.iter().any(|s| s == "rebase"),
        "typed twice: {sends:?}"
    );
}

/// Hold the store, queue `writer`, queue a claim behind it, release: the
/// claim runs between the writer's read and its write.
async fn straddle<F, Fut>(
    mgr: &Arc<SessionManager>,
    id: ManagedSessionId,
    writer: F,
) -> SessionRecord
where
    F: FnOnce(Arc<SessionManager>, ManagedSessionId) -> Fut,
    Fut: Future<Output = Result<(), ManagedError>> + Send + 'static,
{
    let held = mgr.store.write().await;
    let write = tokio::spawn(writer(Arc::clone(mgr), id));
    tokio::time::sleep(QUEUE).await;
    let m = Arc::clone(mgr);
    let claim = tokio::spawn(async move { m.claim_decision(&id, &AnswerClaim::by("alice")).await });
    tokio::time::sleep(QUEUE).await;
    drop(held);
    write.await.expect("join").expect("the writer");
    claim.await.expect("join").expect("the claim");
    assert_claim_survived(mgr, &id).await
}

/// #9841: `set_workspace` straddling a claim keeps the claim and still lands
/// its own fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_workspace_cannot_restore_a_claimed_decision() {
    let (dir, mgr, _driver, id) = pending().await;
    let ws = dir.path().join("moved");
    let want = ws.clone();
    let record = straddle(&mgr, id, move |m, id| async move {
        m.set_workspace(&id, ws, ManagedSessionState::Active).await
    })
    .await;
    assert_eq!(record.workspace_path, Some(want));
}

/// #9841: `mark_errored` straddling a claim keeps the claim and still lands
/// its own fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mark_errored_cannot_restore_a_claimed_decision() {
    let (_dir, mgr, _driver, id) = pending().await;
    let record = straddle(&mgr, id, |m, id| async move {
        m.mark_errored(&id, "spawn failed: straddle").await
    })
    .await;
    assert_eq!(record.state, ManagedSessionState::Errored);
    assert!(
        record.task.contains("[error: spawn failed: straddle]"),
        "{}",
        record.task
    );
}

/// #9841: `set_claude_session_id` straddling a claim keeps the claim and
/// still lands its own field.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_claude_session_id_cannot_restore_a_claimed_decision() {
    let (_dir, mgr, _driver, id) = pending().await;
    let record = straddle(&mgr, id, |m, id| async move {
        m.set_claude_session_id(&id, "claude-straddle").await
    })
    .await;
    assert_eq!(record.claude_session_id.as_deref(), Some("claude-straddle"));
}

/// #9841: a whole-record write read before `set_pending_decision` cannot
/// erase the newer decision either (the same rule, the other direction).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_write_cannot_erase_a_newer_decision() {
    let (dir, mgr, _driver, id) = pending().await;
    mgr.claim_decision(&id, &AnswerClaim::by("alice"))
        .await
        .expect("clear the first decision");
    let stale = mgr.get(&id).await.expect("read before the new decision");
    mgr.set_pending_decision(&id, "second question", None)
        .await
        .expect("a new decision");
    let mut write = stale;
    write.workspace_path = Some(dir.path().join("stale-writer"));
    mgr.store
        .write()
        .await
        .upsert(write)
        .await
        .expect("stale write");

    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.pending_decision.as_deref(), Some("second question"));
    assert!(record.pending_decision_id.is_some());
    assert_eq!(record.workspace_path, Some(dir.path().join("stale-writer")));
}

/// #9841: a decision set between `decommission_record_only`'s read and its
/// write does not sit live on the Decommissioned record; the new id and the
/// last answer carry, so a later answer is refused. Ordered by the store
/// lock's FIFO queue, as in [`straddle`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decommission_straddling_a_new_decision_leaves_none_live() {
    let (_dir, mgr, _driver, id) = pending().await;
    mgr.claim_decision(&id, &AnswerClaim::by("alice"))
        .await
        .expect("answer the first decision");
    let answered = mgr.get(&id).await.expect("record").last_answer;

    let held = mgr.store.write().await;
    let m = Arc::clone(&mgr);
    let decommission = tokio::spawn(async move { m.decommission_record_only(&id).await });
    tokio::time::sleep(QUEUE).await;
    let m = Arc::clone(&mgr);
    let decide = tokio::spawn(async move {
        m.set_pending_decision(&id, "second question", Some("yes"))
            .await
    });
    tokio::time::sleep(QUEUE).await;
    drop(held);
    decommission.await.expect("join").expect("the decommission");
    decide.await.expect("join").expect("the new decision");

    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Decommissioned);
    assert_eq!(
        record.pending_decision, None,
        "a terminal record kept a live decision"
    );
    assert_eq!(record.proposed_default, None);
    assert_eq!(record.live_decision_id(), None);
    assert_eq!(record.last_answer, answered, "the last answer was lost");
    assert!(
        record.pending_decision_id.is_some()
            && record.pending_decision_id != answered.and_then(|a| a.decision_id),
        "the new decision's id was lost: {:?}",
        record.pending_decision_id
    );
    let retry = mgr.claim_decision(&id, &AnswerClaim::by("bob")).await;
    assert!(
        matches!(retry, Err(ManagedError::DecisionConflict(_))),
        "{retry:?}"
    );
}
