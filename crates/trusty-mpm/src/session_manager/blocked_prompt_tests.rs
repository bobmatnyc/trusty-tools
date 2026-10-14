//! Tests for [`SessionManager::sync_blocked_prompt`] (#8393).
//!
//! Why: the setter is the one write path for the classifier's blocked prompt;
//! its Active-only guard and its idempotence are the contract the supervisor
//! relies on.
//! What: set, repeat, replace and clear on an `Active` record; the refusal on
//! every other live state; an old record that has no `blocked_prompt` loads.
//! Test: this file IS the test module.

use std::sync::Arc;

use tempfile::TempDir;

use super::decision_tests::seed_answerable;
use super::{ManagedSessionState, SessionManager};
use crate::session_manager::manager::ManagedError;

/// A manager over a no-op driver with one record in `state`.
async fn seeded(
    state: ManagedSessionState,
) -> (TempDir, Arc<SessionManager>, super::ManagedSessionId) {
    let dir = TempDir::new().unwrap();
    let driver = super::decision_tests::AnswerDriver::new();
    let mgr = Arc::new(
        SessionManager::new(dir.path(), driver)
            .await
            .expect("manager"),
    );
    let id = seed_answerable(&mgr, &dir, state).await;
    (dir, mgr, id)
}

/// #8393: set stamps the record, a repeat writes nothing, a new text replaces
/// it, and `None` clears it. No decision field moves.
#[tokio::test]
async fn set_repeat_replace_and_clear() {
    let (_dir, mgr, id) = seeded(ManagedSessionState::Active).await;

    assert!(mgr.sync_blocked_prompt(&id, Some("one?")).await.unwrap());
    let first = mgr.get(&id).await.unwrap().blocked_prompt.expect("set");
    assert_eq!(first.question, "one?");

    assert!(!mgr.sync_blocked_prompt(&id, Some("one?")).await.unwrap());
    assert_eq!(mgr.get(&id).await.unwrap().blocked_prompt, Some(first));

    assert!(mgr.sync_blocked_prompt(&id, Some("two?")).await.unwrap());
    let rec = mgr.get(&id).await.unwrap();
    assert_eq!(rec.blocked_prompt.expect("replaced").question, "two?");
    assert_eq!(
        (rec.pending_decision, rec.pending_decision_id),
        (None, None)
    );

    assert!(mgr.sync_blocked_prompt(&id, None).await.unwrap());
    assert_eq!(mgr.get(&id).await.unwrap().blocked_prompt, None);
    assert!(!mgr.sync_blocked_prompt(&id, None).await.unwrap());
}

/// #8393: a record that is not `Active` gets no prompt, and a missing record
/// is an error rather than a silent success.
#[tokio::test]
async fn a_non_active_session_gets_nothing() {
    for state in [
        ManagedSessionState::Stopped,
        ManagedSessionState::Provisioning,
        ManagedSessionState::Errored,
    ] {
        let (_dir, mgr, id) = seeded(state.clone()).await;
        let changed = mgr.sync_blocked_prompt(&id, Some("which?")).await.unwrap();
        assert!(!changed, "{state:?}");
        assert_eq!(
            mgr.get(&id).await.unwrap().blocked_prompt,
            None,
            "{state:?}"
        );
    }

    let (_dir, mgr, _id) = seeded(ManagedSessionState::Active).await;
    let missing = super::ManagedSessionId::new();
    let err = mgr.sync_blocked_prompt(&missing, Some("x")).await;
    assert!(
        matches!(err, Err(ManagedError::SessionNotFound(_))),
        "{err:?}"
    );
}

/// #8393: a record persisted before the field existed loads with `None`, and
/// a record without a prompt serializes without the key.
#[tokio::test]
async fn an_old_record_loads_without_the_field() {
    let (_dir, mgr, id) = seeded(ManagedSessionState::Active).await;
    let json = serde_json::to_value(mgr.get(&id).await.unwrap()).unwrap();
    assert!(!json.as_object().unwrap().contains_key("blocked_prompt"));
    let old: super::SessionRecord = serde_json::from_value(json).expect("old record loads");
    assert_eq!(old.blocked_prompt, None);
}
