//! The resume claim `resume_managed` holds (#8233 item 1).
//!
//! Why: r5 held the claim only for the record transition — `SessionManager::
//! resume` took it and dropped it on return, so the self-heal, the prompt
//! refresh, the pane handshake, the spawn and the post-send check all ran
//! unclaimed over a record already written `Active` with a bare shell behind
//! it. The reaper then stopped that record and the next supervisor tick
//! launched it a second time.
//! What: the route's entry-side refusal, driven through the real
//! `resume_managed` against a claim a concurrent path already holds. The two
//! consequences of the claim's SPAN are pinned where they are observable —
//! `the_reaper_leaves_a_session_whose_resume_is_in_flight_alone` and
//! `a_supervisor_tick_does_nothing_to_a_session_being_resumed`.
//! Test: this file.

use std::sync::Arc;

use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedSessionState;

/// #8233 item 1: a resume arriving while another path holds this session is
/// refused as "already resuming", not reported as a launch failure.
///
/// Why: the refusal has to be a CONFLICT. Mapped through `Other` it became an
/// HTTP 500 and the CLI printed "daemon returned an internal error" for a
/// session that was, in fact, coming up perfectly well under the other path.
/// What: holds the claim `resume_managed` now takes as its first statement,
/// then drives the real route into the same session. Deterministic — the claim
/// is in place before the route starts, never raced into position. Also asserts
/// the record is untouched: a refused resume must change nothing at all.
/// Test: this function IS the test.
#[tokio::test]
async fn a_second_operator_resume_is_refused_while_one_is_in_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create("claim-span".into(), Some(ws.clone()), None, None, None, None)
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, ws, ManagedSessionState::Active)
        .await
        .expect("set Active");
    mgr.stop(&id).await.expect("stop");
    let before = mgr.get(&id).await.expect("record");

    let claim = mgr.begin_resume(&id).expect("the first claim is granted");
    let refused = super::lifecycle::resume_managed(&state, &id).await;

    let err = refused.expect_err("a second operator resume must be refused");
    assert!(
        matches!(err, super::ResumeManagedError::AlreadyResuming(ref s) if s == &id.to_string()),
        "a refused claim must read as 'already resuming', not as a launch failure: {err}"
    );
    let after = mgr.get(&id).await.expect("record");
    assert_eq!(
        after.state, before.state,
        "a refused resume must change no state"
    );
    assert_eq!(
        after.task, before.task,
        "and append no error note to the record the other path is working on"
    );

    // Released, the route is admitted past the claim — a claim that outlived its
    // resume would make the session permanently unresumable.
    drop(claim);
    let later = super::lifecycle::resume_managed(&state, &id).await;
    assert!(
        !matches!(later, Err(super::ResumeManagedError::AlreadyResuming(_))),
        "a released claim must leave the session resumable again: {later:?}"
    );
}
