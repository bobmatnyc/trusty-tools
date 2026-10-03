//! Tests for `SessionManager::inject`/`observe`/`answer_decision` pane-scoped
//! targeting (#2468, the follow-up to #2467 that closes the same
//! sibling-window hijack risk for `inject`/`send_input`, `observe`, and the
//! `answer_decision` route added in the #2514 adversarial-review pass).
//!
//! Why: `session_manager/tests.rs` and `resume_reattach_tests.rs` already
//! cover `resume`'s pane-scoped respawn (#2467); this file is the parallel
//! coverage for the sites #2467 deliberately left out of scope — typing
//! into a live session (`inject`), reading its pane (`observe`), and
//! answering a pending decision (`answer_decision`) all addressed "the
//! session" rather than the record's own recorded pane, which tmux resolves
//! to whichever pane/window is currently ACTIVE.
//! What: for `inject` (across all three `Submit` dispatch variants),
//! `observe`, and `answer_decision`, proves (a) a known-and-alive `pane_id`
//! routes through the pane-scoped driver call, never the session-scoped one;
//! (b) a confirmed-gone `pane_id` refuses loudly with `ManagedError::PaneGone`
//! WITHOUT ever calling a session-scoped primitive; (c) #9101: a legacy
//! record with no captured `pane_id` refuses, where #2467 fell back to the
//! session-scoped call.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use crate::core::sm::control::Submit;

use super::manager::ManagedError;
use super::record::ManagedSessionState;
use super::tests::make_manager;

/// Creates an Active session whose record carries the given `pane_id` (or
/// none, for the legacy-record case), seeding the fake driver's
/// `pane_id_override` BEFORE `create()` so it is captured into the record —
/// mirrors `resume_reattach_tests.rs`'s seeding pattern.
async fn make_active_session(
    dir: &TempDir,
    pane_id: Option<&str>,
) -> (
    super::manager::SessionManager,
    std::sync::Arc<super::tests::FakeTmuxDriver>,
    super::record::SessionRecord,
) {
    let (mgr, fake) = make_manager(dir).await;
    *fake.pane_id_override.lock().unwrap() = pane_id.map(str::to_owned);

    let record = mgr
        .create(
            "task".into(),
            Some(dir.path().to_path_buf()),
            Some("pane-scoped-session".into()),
            Some(dir.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("create");
    assert_eq!(
        record.pane_id.as_deref(),
        pane_id,
        "sanity: pane_id captured"
    );

    mgr.set_workspace(
        &record.id,
        dir.path().to_path_buf(),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");

    (mgr, fake, record)
}

// ── inject: pane-scoped-when-known / refuse-when-gone / legacy-fallback ─────

#[tokio::test]
async fn inject_targets_pane_when_pane_id_known() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;

    mgr.inject(&record.id, "hello", Submit::Enter)
        .await
        .expect("inject enter");

    assert_eq!(
        fake.pane_send_calls.lock().unwrap().as_slice(),
        [(
            record.tmux_name.clone(),
            "%6015".to_string(),
            "hello".to_string()
        )]
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "a known-alive pane_id must never fall through to the session-scoped send"
    );
}

#[tokio::test]
async fn inject_nosubmit_targets_pane_when_pane_id_known() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;

    mgr.inject(&record.id, "partial", Submit::NoSubmit)
        .await
        .expect("inject nosubmit");

    assert_eq!(
        fake.pane_literal_calls.lock().unwrap().as_slice(),
        [(
            record.tmux_name.clone(),
            "%6015".to_string(),
            "partial".to_string()
        )]
    );
    assert!(fake.send_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn inject_interrupt_targets_pane_when_pane_id_known() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;

    mgr.inject(&record.id, "ignored", Submit::Interrupt)
        .await
        .expect("inject interrupt");

    assert_eq!(
        fake.pane_interrupt_calls.lock().unwrap().as_slice(),
        [(record.tmux_name.clone(), "%6015".to_string())]
    );
    assert!(fake.interrupt_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn inject_refuses_when_stored_pane_gone() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    let err = mgr
        .inject(&record.id, "hello", Submit::Enter)
        .await
        .expect_err("a confirmed-gone recorded pane must refuse, not fall back");

    assert!(
        matches!(&err, ManagedError::PaneGone(sid, pid) if sid == &record.id.to_string() && pid == "%6015"),
        "expected PaneGone(session_id, \"%6015\"), got: {err:?}"
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "the session-scoped target must NEVER be constructed on refusal"
    );
    assert!(
        fake.pane_send_calls.lock().unwrap().is_empty(),
        "the pane-scoped call must not fire either — the whole operation refuses"
    );
}

/// #9101: a record with no pane id cannot prove the session is its own, so
/// the old session-scoped fallback refuses and nothing is typed.
#[tokio::test]
async fn inject_refuses_a_legacy_record_without_pane_id() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, None).await;

    let err = mgr
        .inject(&record.id, "hello", Submit::Enter)
        .await
        .expect_err("a legacy record must refuse");

    assert!(matches!(err, ManagedError::InvalidState(..)), "{err:?}");
    assert!(fake.send_calls.lock().unwrap().is_empty());
    assert!(fake.pane_send_calls.lock().unwrap().is_empty());
}

// ── answer_decision: pane-scoped-when-known / refuse-when-gone / legacy-fallback ──

#[tokio::test]
async fn answer_decision_targets_pane_when_pane_id_known() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;

    mgr.answer_decision(&record.id, "rebase")
        .await
        .expect("answer_decision");

    assert_eq!(
        fake.pane_send_calls.lock().unwrap().as_slice(),
        [(
            record.tmux_name.clone(),
            "%6015".to_string(),
            "rebase".to_string()
        )]
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "a known-alive pane_id must never fall through to the session-scoped send"
    );
}

#[tokio::test]
async fn answer_decision_refuses_when_stored_pane_gone() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    let err = mgr
        .answer_decision(&record.id, "rebase")
        .await
        .expect_err("a confirmed-gone recorded pane must refuse, not fall back");

    assert!(
        matches!(&err, ManagedError::PaneGone(sid, pid) if sid == &record.id.to_string() && pid == "%6015"),
        "expected PaneGone(session_id, \"%6015\"), got: {err:?}"
    );
    assert!(
        fake.send_calls.lock().unwrap().is_empty(),
        "the session-scoped target must NEVER be constructed on refusal"
    );
    assert!(
        fake.pane_send_calls.lock().unwrap().is_empty(),
        "the pane-scoped call must not fire either — the whole operation refuses"
    );
}

/// #9101: as `inject_refuses_a_legacy_record_without_pane_id`, for answers.
#[tokio::test]
async fn answer_decision_refuses_a_legacy_record_without_pane_id() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, None).await;

    let err = mgr
        .answer_decision(&record.id, "rebase")
        .await
        .expect_err("a legacy record must refuse");

    assert!(matches!(err, ManagedError::InvalidState(..)), "{err:?}");
    assert!(fake.send_calls.lock().unwrap().is_empty());
    assert!(fake.pane_send_calls.lock().unwrap().is_empty());
}

// ── observe: pane-scoped-when-known / refuse-when-gone / legacy-fallback ────

#[tokio::test]
async fn observe_captures_pane_scoped_when_pane_id_known() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    // Keyed by PANE id (not session name) — proves observe used the
    // pane-scoped capture, not the session-scoped one.
    fake.capture_responses
        .lock()
        .unwrap()
        .insert("%6015".to_string(), "pane-owned output".to_string());

    let obs = mgr.observe(&record.id, 50).await.expect("observe");

    assert_eq!(obs.raw_pane, "pane-owned output");
    assert_eq!(
        fake.pane_capture_calls.lock().unwrap().as_slice(),
        [(record.tmux_name.clone(), "%6015".to_string())]
    );
}

#[tokio::test]
async fn observe_refuses_when_stored_pane_gone() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    let err = mgr
        .observe(&record.id, 50)
        .await
        .expect_err("a confirmed-gone recorded pane must refuse observe too");

    assert!(
        matches!(&err, ManagedError::PaneGone(sid, pid) if sid == &record.id.to_string() && pid == "%6015"),
        "expected PaneGone(session_id, \"%6015\"), got: {err:?}"
    );
    assert!(fake.pane_capture_calls.lock().unwrap().is_empty());
}

/// #9101: a legacy record's observe reads no pane at all.
#[tokio::test]
async fn observe_refuses_a_legacy_record_without_pane_id() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, None).await;

    let err = mgr
        .observe(&record.id, 50)
        .await
        .expect_err("a legacy record must refuse");

    assert!(matches!(err, ManagedError::InvalidState(..)), "{err:?}");
    assert!(fake.pane_capture_calls.lock().unwrap().is_empty());
}

// ── capture_pane (by id): the shared read behind activity/mcp/cli/supervisor ──
// (issue #2515 — same three-way contract as observe, applied to the read helper
// every non-observe consumer routes through).

#[tokio::test]
async fn capture_pane_targets_recorded_pane_when_pane_id_known() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    // Keyed by PANE id — proves the read used the pane-scoped capture, not the
    // session-scoped one (which the fake keys by session name).
    fake.capture_responses
        .lock()
        .unwrap()
        .insert("%6015".to_string(), "pane-owned output".to_string());

    let text = mgr
        .capture_pane(&record.id, 60)
        .await
        .expect("capture_pane");

    assert_eq!(text, "pane-owned output");
    assert_eq!(
        fake.pane_capture_calls.lock().unwrap().as_slice(),
        [(record.tmux_name.clone(), "%6015".to_string())]
    );
}

#[tokio::test]
async fn capture_pane_refuses_when_stored_pane_gone() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    let err = mgr
        .capture_pane(&record.id, 60)
        .await
        .expect_err("a confirmed-gone recorded pane must refuse the read too");

    assert!(
        matches!(&err, ManagedError::PaneGone(sid, pid) if sid == &record.id.to_string() && pid == "%6015"),
        "expected PaneGone(session_id, \"%6015\"), got: {err:?}"
    );
    assert!(
        fake.pane_capture_calls.lock().unwrap().is_empty(),
        "the pane-scoped capture must not fire on refusal — the whole read refuses"
    );
}

/// #9101: a legacy record's capture reads no pane at all.
#[tokio::test]
async fn capture_pane_refuses_a_legacy_record_without_pane_id() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, None).await;

    let err = mgr
        .capture_pane(&record.id, 60)
        .await
        .expect_err("a legacy record must refuse");

    assert!(matches!(err, ManagedError::InvalidState(..)), "{err:?}");
    assert!(fake.pane_capture_calls.lock().unwrap().is_empty());
}

// ── capture_pane_by_tmux_name: the idle-reaper read keyed by tmux name ───────
// (issue #2515 — the reaper knows a session only by tmux name; resolve the
// record, then apply the same pane-scoped-or-refuse contract).

#[tokio::test]
async fn capture_pane_by_tmux_name_targets_recorded_pane() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    fake.capture_responses
        .lock()
        .unwrap()
        .insert("%6015".to_string(), "pane-owned output".to_string());

    let text = mgr
        .capture_pane_by_tmux_name(&record.tmux_name, 300)
        .await
        .expect("live record with an alive pane yields pane content");

    assert_eq!(text, "pane-owned output");
    assert_eq!(
        fake.pane_capture_calls.lock().unwrap().as_slice(),
        [(record.tmux_name.clone(), "%6015".to_string())]
    );
}

#[tokio::test]
async fn capture_pane_by_tmux_name_refuses_when_pane_gone() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, Some("%6015")).await;
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    let out = mgr.capture_pane_by_tmux_name(&record.tmux_name, 300).await;

    assert!(
        out.is_none(),
        "a confirmed-gone recorded pane must yield None (no verdict), not a sibling read"
    );
    assert!(
        fake.pane_capture_calls.lock().unwrap().is_empty(),
        "the pane-scoped capture must not fire when the pane is gone"
    );
}

/// #9101: a legacy record yields no verdict; a name with no record at all
/// still falls back to the session-scoped capture.
#[tokio::test]
async fn capture_pane_by_tmux_name_without_a_record_falls_back_to_session() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake, record) = make_active_session(&dir, None).await;
    fake.capture_responses.lock().unwrap().insert(
        "tm-unmanaged".to_string(),
        "session-scoped output".to_string(),
    );

    let legacy = mgr.capture_pane_by_tmux_name(&record.tmux_name, 300).await;
    let unmanaged = mgr.capture_pane_by_tmux_name("tm-unmanaged", 300).await;

    assert_eq!(legacy, None, "a legacy record must yield no verdict");
    assert_eq!(unmanaged.as_deref(), Some("session-scoped output"));
    assert!(fake.pane_capture_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn capture_pane_by_tmux_name_none_for_unmanaged_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake, _record) = make_active_session(&dir, Some("%6015")).await;

    let out = mgr
        .capture_pane_by_tmux_name("tm-no-such-session", 300)
        .await;

    assert!(
        out.is_none(),
        "an unmanaged/unknown tmux name with no live record and no seeded capture yields None"
    );
}
