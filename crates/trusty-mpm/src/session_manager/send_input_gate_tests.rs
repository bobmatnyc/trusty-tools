//! Tests for `SessionManager::send_input`'s readiness/modal gate (#3591).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! `send_input`-gate-specific coverage lives here so neither file grows past
//! its limit. Mirrors the pattern established by `restart_tests.rs` /
//! `decommission_worktree_tests.rs` (`FakeTmuxDriver` + `make_manager` from
//! the sibling `tests` module).
//! What: proves the state guard refuses `Provisioning`/`Errored` sessions
//! (the shell-execution defect this issue exists to close), proves the
//! ClaudeCode-only modal probe refuses a session whose pane shows a known
//! blocking onboarding/trust dialog, and proves a `Tcode`-runtime session is
//! exempt from that modal probe (the healthy-path regression guard on the
//! other runtime).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use super::record::ManagedSessionState;
use super::tests::make_manager;

/// send_input must be rejected for Provisioning and Errored sessions (#3591).
///
/// Why: this is the shell-execution defect the issue is about — before the
/// fix, `Provisioning` (pane exists, Claude Code hasn't finished booting) and
/// `Errored` (runtime crashed back to a bare shell) sessions were NOT refused
/// by `send_input`'s state guard, so `Submit::Enter`'s literal-then-Enter
/// dispatch would type the message into a live pane and press Enter — which,
/// against a bare shell, executes it as a shell command. Against PRE-FIX code
/// (guard only checked Stopped/Decommissioned) this test FAILS: a freshly
/// created record is `Provisioning` and the send proceeds, recording a call
/// in `fake.send_calls`. Asserting BOTH the `Err` result AND the empty
/// `send_calls` makes the causality explicit — a caller could otherwise
/// misread a false-negative error as protection even if the text still got
/// typed into the pane.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_send_input_rejected_for_provisioning_and_errored() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    // A freshly created record is Provisioning (create.rs) — no state
    // manipulation needed to exercise this branch.
    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    assert_eq!(record.state, ManagedSessionState::Provisioning);

    let result = mgr.send_input(&record.id, "rm -rf /").await;
    assert!(
        result.is_err(),
        "send_input must fail for Provisioning sessions (pre-fix: this proceeds)"
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "no send-line may fire for a Provisioning session — this is the shell-execution risk"
    );

    // Test Errored rejection.
    mgr.mark_errored(&record.id, "spawn failed")
        .await
        .expect("mark_errored");
    let result = mgr.send_input(&record.id, "rm -rf /").await;
    assert!(
        result.is_err(),
        "send_input must fail for Errored sessions (pre-fix: this proceeds)"
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "no send-line may fire for an Errored session — this is the shell-execution risk"
    );
}

/// send_input must refuse a ClaudeCode-runtime session whose pane is showing
/// a blocking onboarding/trust modal, rather than silently letting the text
/// be swallowed by the dialog (#3591).
///
/// Why: an `Active` session is NOT necessarily accepting typed input — a
/// first-run onboarding wizard or trust dialog can be occupying the pane's
/// input focus. Against PRE-FIX code (no readiness/modal probe on
/// `send_input` at all) this test FAILS: the send proceeds and is recorded in
/// `fake.send_calls` even though the text would have been eaten by the modal
/// on a real pane. Asserting the empty `send_calls` alongside the `Err`
/// confirms the gate actually stops the dispatch rather than merely
/// misreporting a send that still happened.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_send_input_rejected_when_pane_shows_blocking_modal() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).await.unwrap();
        r.state = ManagedSessionState::Active;
        store.upsert(r).await.unwrap();
    }
    fake.capture_responses.lock().unwrap().insert(
        record.tmux_name.clone(),
        "Do you trust the files in this folder?".into(),
    );

    let result = mgr.send_input(&record.id, "hello").await;
    assert!(
        result.is_err(),
        "send_input must fail when the pane shows a blocking modal (pre-fix: this proceeds)"
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "no send-line may fire while a blocking modal is showing"
    );
}

/// A `RuntimeKind::Tcode` session must NOT be gated by the ClaudeCode-only
/// modal probe (#3591).
///
/// Why: the blocking-modal markers (`BLOCKING_MODAL_MARKERS` in
/// `task_inject.rs`) are Claude-Code-specific onboarding/trust-dialog copy
/// (mirrors `should_inject_task`'s existing ClaudeCode-only scoping) — there
/// is nothing meaningful to probe for on a tcode pane. To PROVE the
/// exemption (not just that a send with no marker present happens to
/// succeed), this test seeds the fake pane's captured text with the SAME
/// ClaudeCode marker used elsewhere in this suite to trigger a rejection,
/// on a Tcode-runtime record — and asserts the send still succeeds. If the
/// modal probe were mistakenly applied regardless of runtime, this send
/// would be rejected exactly like
/// `manager_send_input_rejected_when_pane_shows_blocking_modal`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_send_input_skips_modal_probe_for_tcode() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).await.unwrap();
        r.state = ManagedSessionState::Active;
        r.runtime = crate::runtime::RuntimeKind::Tcode;
        store.upsert(r).await.unwrap();
    }
    // Same marker that DOES block a ClaudeCode session in
    // `manager_send_input_rejected_when_pane_shows_blocking_modal`.
    fake.capture_responses.lock().unwrap().insert(
        record.tmux_name.clone(),
        "Do you trust the files in this folder?".into(),
    );

    mgr.send_input(&record.id, "hello tcode")
        .await
        .expect("send_input must succeed for an Active tcode session even with a ClaudeCode-shaped marker in its pane");
    let calls = fake.send_calls.lock().unwrap();
    assert!(calls.iter().any(|(_, text)| text == "hello tcode"));
}

/// `send_input_observed` must report the UNSUBMITTED state when the pane still
/// shows a collapsed bracketed paste after the send (#5699).
///
/// Why: this is the defect. `send_input` succeeds the moment the two tmux
/// subprocesses exit 0, and the `session_send` MCP tool turned that into
/// `{"sent": true}` — while the message sat in Claude Code's input buffer as a
/// `[Pasted text #1]` placeholder that the trailing Enter never committed.
/// Against PRE-FIX code there is no `send_input_observed` at all and `sent` is
/// a hardcoded literal, so no post-send pane read happens and this assertion
/// cannot hold. Asserting the send ALSO fired proves the verdict describes a
/// real dispatch rather than a refusal.
/// Test: this function IS the test.
#[tokio::test]
async fn send_input_observed_reports_unsubmitted_paste() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).await.unwrap();
        r.state = ManagedSessionState::Active;
        store.upsert(r).await.unwrap();
    }
    // The exact prompt line issue #5699 recorded on the live session.
    fake.capture_responses.lock().unwrap().insert(
        record.tmux_name.clone(),
        "❯ [Pasted text #1][Pasted text #2]".into(),
    );

    let state = mgr
        .send_input_observed(&record.id, "a very long coordination message")
        .await
        .expect("the send itself succeeds — only the SUBMIT is in question");
    assert_eq!(
        state,
        crate::session_manager::SubmitState::UnsubmittedPaste,
        "a pane still showing a collapsed paste proves the Enter did not commit"
    );
    assert!(
        !state.delivered(),
        "`sent` must be false for an observed unsubmitted paste (pre-fix: hardcoded true)"
    );
    assert!(
        fake.send_calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, text)| text == "a very long coordination message"),
        "the text was typed — the verdict is about submission, not dispatch"
    );
}

/// A pane back at its ready prompt after the send reports `Submitted` (#5699).
///
/// Why: the healthy-path guard on the same probe — the fix must not start
/// reporting a delivery problem for every ordinary send. Paired with
/// `send_input_observed_reports_unsubmitted_paste`, this proves the verdict
/// tracks the pane content rather than being constant in either direction.
/// Test: this function IS the test.
#[tokio::test]
async fn send_input_observed_reports_submitted_on_a_clean_prompt() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).await.unwrap();
        r.state = ManagedSessionState::Active;
        store.upsert(r).await.unwrap();
    }
    fake.capture_responses
        .lock()
        .unwrap()
        .insert(record.tmux_name.clone(), "● Working…\n❯ ".into());

    let state = mgr
        .send_input_observed(&record.id, "short message")
        .await
        .expect("send");
    assert_eq!(state, crate::session_manager::SubmitState::Submitted);
    assert!(state.delivered());
}
