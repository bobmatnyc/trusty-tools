//! Tests for `SessionManager::adopt_existing` (#1433).
//!
//! Why: extracted from `tests.rs` (issue #2468) to keep that file under its
//! own 1500-SLOC test cap after the pane-scoped inject/observe `FakeTmuxDriver`
//! additions — mirrors why `reload_error_tests.rs` was split out earlier.
//! What: the explicit-adopt happy path (registers `Active`, never calls
//! `create_session`), the `TmuxSessionMissing`/`AlreadyAdopted` typed-error
//! guards, the non-`tmpm-`-prefix allowance (unlike reconcile's automatic
//! adoption), the persisted `ephemeral` flag, and (#3692) the
//! auto-suffix-a-recycled-name / reuse-a-terminal-tombstone's-name collision
//! handling.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use chrono::Utc;
use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::tests::make_manager;

#[tokio::test]
async fn manager_adopt_existing_registers_active() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    // The pane already exists (operator started it outside trusty-mpm).
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-hand-started".into());

    let record = mgr
        .adopt_existing(
            "tmpm-hand-started",
            PathBuf::from("/Users/op/work/proj"),
            "drive my hand-started session".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("adopt existing");

    assert_eq!(record.tmux_name, "tmpm-hand-started");
    assert_eq!(record.state, ManagedSessionState::Active);
    assert_eq!(record.cwd, PathBuf::from("/Users/op/work/proj"));
    assert_eq!(record.task, "drive my hand-started session");

    // Adoption must NOT spawn a new tmux session — the pane already exists.
    assert!(
        fake.create_cwd_calls.lock().unwrap().is_empty(),
        "adopt_existing must NOT call create_session; calls: {:?}",
        fake.create_cwd_calls.lock().unwrap()
    );

    // The record is durably queryable.
    let got = mgr.get(&record.id).await.expect("get adopted");
    assert_eq!(got.id, record.id);
    assert_eq!(got.state, ManagedSessionState::Active);
}

/// Adopting a tmux name that does NOT exist on the host must error — you cannot
/// adopt a pane that is not there (#1433).
///
/// Why: this is the inverse of `create`'s NameCollision guard. The error must be
/// the dedicated `TmuxSessionMissing` variant so the HTTP layer maps it to a 404.
/// What: adopts a name the driver does not report and asserts the typed error.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_missing_tmux_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let err = mgr
        .adopt_existing(
            "tmpm-not-here",
            PathBuf::from("/tmp/x"),
            String::new(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect_err("adopting a nonexistent pane must fail");

    assert!(
        matches!(err, ManagedError::TmuxSessionMissing(ref n) if n == "tmpm-not-here"),
        "expected TmuxSessionMissing, got {err:?}"
    );
}

/// Adopting a tmux name the store ALREADY tracks must error — no double records
/// for one pane (#1433).
///
/// Why: a second record for the same pane would split ownership and confuse every
/// downstream verb. The dedicated `AlreadyAdopted` variant lets the HTTP layer map
/// it to a 409 Conflict.
/// What: adopts once (succeeds), then adopts the same live name again and asserts
/// the second call returns `AlreadyAdopted`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_double_adopt_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    fake.seeded_names.lock().unwrap().push("tmpm-once".into());

    mgr.adopt_existing(
        "tmpm-once",
        PathBuf::from("/tmp/once"),
        String::new(),
        crate::runtime::RuntimeKind::default(),
        false,
    )
    .await
    .expect("first adopt succeeds");

    let err = mgr
        .adopt_existing(
            "tmpm-once",
            PathBuf::from("/tmp/once"),
            String::new(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect_err("second adopt of the same pane must fail");

    assert!(
        matches!(err, ManagedError::AlreadyAdopted(ref n) if n == "tmpm-once"),
        "expected AlreadyAdopted, got {err:?}"
    );
}

/// The explicit adopt path must allow NON-`tmpm-` names (unlike reconcile, which
/// filters to the `tmpm-` prefix for safe automatic adoption) (#1433).
///
/// Why: an operator naming a pane explicitly knows what they are adopting; the
/// `tmpm-` prefix filter exists only to make AUTOMATIC boot adoption safe. The
/// explicit path must not reject a session just because it lacks the prefix.
/// What: seeds a non-`tmpm-` live name, adopts it, and asserts success.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_allows_non_tmpm_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    fake.seeded_names
        .lock()
        .unwrap()
        .push("my-cli-session".into());

    let record = mgr
        .adopt_existing(
            "my-cli-session",
            PathBuf::from("/Users/op/repo"),
            "adopt non-prefixed".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("non-tmpm names are adoptable on the explicit path");

    assert_eq!(record.tmux_name, "my-cli-session");
    assert_eq!(record.state, ManagedSessionState::Active);
}

/// `adopt_existing` persists the caller-supplied `ephemeral` flag (#1508).
///
/// Why: the e2e harness adopts panes as ephemeral; the flag must reach the record.
/// What: seeds a live pane, adopts it with `ephemeral=true`, asserts it persisted.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_persists_ephemeral_flag() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-eph-adopt".into());

    let record = mgr
        .adopt_existing(
            "tmpm-eph-adopt",
            PathBuf::from("/tmp/adopt"),
            "throwaway adopt".into(),
            crate::runtime::RuntimeKind::default(),
            true,
        )
        .await
        .expect("adopt ephemeral");

    assert!(
        mgr.get(&record.id).await.unwrap().ephemeral,
        "adopted ephemeral flag persisted"
    );
}

/// Build a bare `SessionRecord` for direct store seeding — used by the
/// collision tests below, which need control over `state`/`tmux_name`/
/// `pane_id` that `super::tests::seed_record` does not expose.
fn bare_record(
    tmux_name: &str,
    state: ManagedSessionState,
    pane_id: Option<&str>,
) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.into(),
        cwd: PathBuf::from("/tmp/bare"),
        task: "seed".into(),
        state,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: pane_id.map(str::to_string),
        injection_status: Default::default(),
        worktree_owner: None,
    }
}

/// A name held ONLY by a TERMINAL (Decommissioned) tombstone is reusable
/// as-is, unsuffixed — the over-broad ANY-state check this replaced would
/// have wrongly returned `AlreadyAdopted` here (issue #3692, requirement 2).
///
/// Why: an operator hand-renaming a stale session out of the way (the exact
/// workaround the #3692 evidence recorded) should never have been necessary —
/// decommissioning it should have been enough to free the name.
/// What: seeds a Decommissioned record holding `tm-freed`, then adopts a live
/// pane that also answers to `tm-freed` and asserts the name is NOT suffixed.
/// Test: itself.
#[tokio::test]
async fn manager_adopt_existing_reuses_name_freed_by_decommissioned_record() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let gone = bare_record(
        "tm-freed",
        ManagedSessionState::Decommissioned,
        Some("old-pane"),
    );
    mgr.store
        .write()
        .await
        .upsert(gone)
        .await
        .expect("seed decommissioned tombstone");

    // A brand new, unrelated live pane now answers to the SAME name.
    fake.seeded_names.lock().unwrap().push("tm-freed".into());

    let record = mgr
        .adopt_existing(
            "tm-freed",
            PathBuf::from("/tmp/new"),
            "fresh adoption".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("a decommissioned tombstone's name must be reusable, unsuffixed");

    assert_eq!(
        record.tmux_name, "tm-freed",
        "a terminal record's name must not block reuse or force a suffix"
    );
}

/// A name held by a NON-terminal record whose live pane has genuinely
/// changed (a RECYCLED name — the tracked pane died, tmux gave the name to a
/// different pane) must auto-suffix, never reject (issue #3692).
///
/// Why: this is the mechanism the #3692 evidence pointed at — two distinct
/// sessions ending up sharing one name. Distinguishing "recycled" from
/// "identical pane, double-adopted" via `pane_id` lets the system resolve it
/// automatically instead of forcing a manual rename workaround.
/// What: adopts a pane under `tm-recycled` (captures `pane-old`), then adopts
/// the SAME name again after the driver reports a DIFFERENT live `pane_id`
/// (`pane-new`) — simulating the first pane dying and a new one taking its
/// name. Asserts the second adoption succeeds as `tm-recycled-2` and the live
/// tmux session was physically renamed to match.
/// Test: itself.
#[tokio::test]
async fn manager_adopt_existing_suffixes_recycled_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    *fake.pane_id_override.lock().unwrap() = Some("pane-old".into());
    fake.seeded_names.lock().unwrap().push("tm-recycled".into());
    let first = mgr
        .adopt_existing(
            "tm-recycled",
            PathBuf::from("/tmp/first"),
            "first pane".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("first adopt succeeds");
    assert_eq!(first.tmux_name, "tm-recycled");

    // The name is recycled: tmux now answers to it with a DIFFERENT pane,
    // while `first`'s record is still non-terminal (Active) — a genuine
    // collision, not a double-adopt of the same pane.
    *fake.pane_id_override.lock().unwrap() = Some("pane-new".into());

    let second = mgr
        .adopt_existing(
            "tm-recycled",
            PathBuf::from("/tmp/second"),
            "recycled pane".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("a recycled name must auto-suffix, never reject");

    assert_eq!(second.tmux_name, "tm-recycled-2");
    assert_ne!(first.id, second.id);
    assert!(
        fake.rename_calls
            .lock()
            .unwrap()
            .iter()
            .any(|(o, n)| o == "tm-recycled" && n == "tm-recycled-2"),
        "expected rename_session(tm-recycled -> tm-recycled-2), got {:?}",
        fake.rename_calls.lock().unwrap()
    );
}

/// A THIRD adoption attempt for the same recycled name must skip the
/// now-taken `-2` and land on `-3` — the smallest free ordinal, not a loop
/// back to `-2` or a failure (issue #3692).
/// Test: itself.
#[tokio::test]
async fn manager_adopt_existing_recycled_name_skips_to_next_free_ordinal() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    *fake.pane_id_override.lock().unwrap() = Some("pane-1".into());
    fake.seeded_names.lock().unwrap().push("tm-recycled".into());
    mgr.adopt_existing(
        "tm-recycled",
        PathBuf::from("/tmp/1"),
        "pane 1".into(),
        crate::runtime::RuntimeKind::default(),
        false,
    )
    .await
    .expect("first adopt succeeds");

    *fake.pane_id_override.lock().unwrap() = Some("pane-2".into());
    let second = mgr
        .adopt_existing(
            "tm-recycled",
            PathBuf::from("/tmp/2"),
            "pane 2".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("second adopt auto-suffixes to -2");
    assert_eq!(second.tmux_name, "tm-recycled-2");

    *fake.pane_id_override.lock().unwrap() = Some("pane-3".into());
    let third = mgr
        .adopt_existing(
            "tm-recycled",
            PathBuf::from("/tmp/3"),
            "pane 3".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("third adopt must skip the taken -2 and land on -3");
    assert_eq!(third.tmux_name, "tm-recycled-3");
}
