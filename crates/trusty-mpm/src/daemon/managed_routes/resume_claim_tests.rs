//! The resume claim `resume_managed` holds (#8233 item 1).
//!
//! Why: r5 held the claim only for the record transition — `SessionManager::
//! resume` took it and dropped it on return, so the self-heal, the prompt
//! refresh, the pane handshake, the spawn and the post-send check all ran
//! unclaimed over a record already written `Active` with a bare shell behind
//! it. The reaper then stopped that record and the next supervisor tick
//! launched it a second time.
//! What: the route's entry-side refusal, driven through the real
//! `resume_managed` against a claim a concurrent path already holds, AND the
//! claim's SPAN — `the_claim_is_still_held_when_the_route_types_into_the_pane`
//! observes the claim from INSIDE the injected tmux driver, so moving the claim
//! back into `SessionManager::resume` fails it. The two downstream consequences
//! of that span are pinned where they are observable —
//! `the_reaper_leaves_a_session_whose_resume_is_in_flight_alone` and
//! `a_supervisor_tick_does_nothing_to_a_session_being_resumed`.
//! Test: this file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::daemon::state::DaemonState;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionManager,
};

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
    let state = Arc::new(DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await);
    let mgr = state.session_manager().await;
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create(
            "claim-span".into(),
            Some(ws.clone()),
            None,
            None,
            None,
            None,
        )
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

    // Released, the session is claimable again — a claim that outlived its
    // resume would make the session permanently unresumable. #8233 r7: asserted
    // through `begin_resume` rather than by driving the whole route a second
    // time. The route's tail reaches the prompt refresh, the deployment repair
    // and the spawn; running all of that proves nothing about RELEASE and used
    // to write a usage fold into the operator's own `~/.trusty-mpm`.
    drop(claim);
    let regranted = mgr
        .begin_resume(&id)
        .expect("a released claim must leave the session claimable again");
    drop(regranted);
}

/// One driver call, and what was true of the record when it was made.
#[derive(Debug)]
struct Observation {
    call: &'static str,
    claim_held: bool,
    persisted_active: bool,
}

/// A tmux driver that RECORDS the claim at every call and never blocks.
///
/// Why (#8233): the claim's span is invisible from outside the route — every
/// earlier test installed the claim itself, so all three passed with the claim
/// taken and released inside `SessionManager::resume`, which IS the P0 defect.
/// The driver is the one seam that runs INSIDE the route, so it is where the
/// question "is the claim still held now?" can actually be asked.
/// What: behaves exactly like `FakeNoopTmuxDriver` (every call succeeds, no
/// session is ever reported live, so `resume_inner` takes its recreate branch)
/// and additionally appends an [`Observation`] per call. `persisted_active` is
/// read from `sessions.json` on disk, so it marks the exact calls that happen
/// AFTER `resume_inner` wrote `Active` — no production hook required.
///
/// It RECORDS and returns; it never parks. Two earlier attempts blocked inside
/// the driver waiting for the route to reach them and hung the suite instead.
/// Test: `the_claim_is_still_held_when_the_route_types_into_the_pane`.
struct ClaimRecordingDriver {
    manager: Arc<OnceLock<Arc<SessionManager>>>,
    session: Arc<OnceLock<ManagedSessionId>>,
    store_path: PathBuf,
    seen: Arc<Mutex<Vec<Observation>>>,
}

impl ClaimRecordingDriver {
    /// Is this session's record `Active` in `sessions.json` right now?
    fn persisted_active(&self, id: &ManagedSessionId) -> bool {
        let Ok(raw) = std::fs::read_to_string(&self.store_path) else {
            return false;
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return false;
        };
        doc.get("sessions")
            .and_then(|s| s.get(id.to_string()))
            .and_then(|r| r.get("state"))
            .and_then(serde_json::Value::as_str)
            == Some("active")
    }

    fn observe(&self, call: &'static str) {
        let (Some(mgr), Some(id)) = (self.manager.get(), self.session.get()) else {
            // Before the fixture finished wiring itself up there is nothing to
            // ask about; those calls are simply not observations.
            return;
        };
        let obs = Observation {
            call,
            claim_held: mgr.is_resume_in_flight(id),
            persisted_active: self.persisted_active(id),
        };
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(obs);
        }
    }
}

impl ManagedTmuxDriver for ClaimRecordingDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.observe("create_session");
        Ok(())
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        self.observe("kill_session");
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        self.observe("send_line");
        Ok(())
    }

    fn send_keys_literal(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        self.observe("send_keys_literal");
        Ok(())
    }

    fn send_interrupt(&self, _name: &str) -> Result<(), ManagedError> {
        self.observe("send_interrupt");
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        self.observe("capture");
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        self.observe("list_sessions");
        Ok(Vec::new())
    }
}

/// #8233 P0: the claim is STILL HELD at the first driver call the route makes
/// after the record reads `Active`.
///
/// Why: this is the one assertion the three pre-existing claim tests cannot
/// make. They install the claim externally, so they are blind to WHERE
/// `resume_managed` takes it — move it back inside `SessionManager::resume`,
/// which releases on return, and all three still pass while the reaper is free
/// to stop a half-resumed session again.
/// What: injects [`ClaimRecordingDriver`], drives the REAL `resume_managed`,
/// then asserts (a) the route reached at least one driver call whose on-disk
/// record already read `Active` — otherwise this test observes nothing — and
/// (b) every such call saw the claim held. Bounded by `tokio::time::timeout`
/// so a route that stalls FAILS rather than hanging the suite.
/// Test: this function IS the test.
#[cfg(unix)]
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_claim_is_still_held_when_the_route_types_into_the_pane() {
    // #7862: the route's last step before it types is the adapter's `claude`
    // lookup, and a machine without Claude Code fails it — leaving the driver
    // with the three calls `resume_inner` made and nothing after the record
    // read `Active`. That is the fixture gap this test's own message names.
    let _claude = crate::test_support::fake_claude_on_path();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("framework");
    let seen: Arc<Mutex<Vec<Observation>>> = Arc::new(Mutex::new(Vec::new()));
    let manager_slot: Arc<OnceLock<Arc<SessionManager>>> = Arc::new(OnceLock::new());
    let session_slot: Arc<OnceLock<ManagedSessionId>> = Arc::new(OnceLock::new());

    let driver = Arc::new(ClaimRecordingDriver {
        manager: Arc::clone(&manager_slot),
        session: Arc::clone(&session_slot),
        store_path: root.join("session-manager").join("sessions.json"),
        seen: Arc::clone(&seen),
    });
    let state =
        Arc::new(DaemonState::with_root_isolated_managed_and_driver(root.clone(), driver).await);
    let mgr = state.session_manager().await;
    let _ = manager_slot.set(Arc::clone(&mgr));

    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create(
            "claim-span-driver".into(),
            Some(ws.clone()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, ws, ManagedSessionState::Active)
        .await
        .expect("set Active");
    mgr.stop(&id).await.expect("stop");
    let _ = session_slot.set(id);

    // The route may legitimately end in an error (no real tmux, no real
    // runtime); what is under test is the claim, not the outcome.
    let _ = tokio::time::timeout(
        Duration::from_secs(60),
        super::lifecycle::resume_managed(&state, &id),
    )
    .await
    .expect("resume_managed must finish well inside the budget, never hang");

    let seen = seen.lock().expect("observations");
    let every_call: Vec<&str> = seen.iter().map(|o| o.call).collect();
    let post_transition: Vec<&Observation> = seen.iter().filter(|o| o.persisted_active).collect();
    assert!(
        !post_transition.is_empty(),
        "the route made no driver call after the record read `Active`, so this test \
         can observe nothing about the claim's span — fix the fixture, do not \
         weaken the assertion. Calls seen: {every_call:?}"
    );
    let post_calls: Vec<&str> = post_transition.iter().map(|o| o.call).collect();
    assert!(
        post_transition.iter().all(|o| o.claim_held),
        "#8233 P0: `resume_managed` must still hold the resume claim at every \
         driver call it makes after the record reads `Active`. Unclaimed calls \
         let the reaper stop a half-resumed session and the supervisor launch it \
         a second time. Post-transition calls: {post_calls:?}; full observations: \
         {post_transition:?}"
    );
}
