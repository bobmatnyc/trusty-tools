//! `SessionManager::rename` + `validate_session_name` coverage.
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; the rename
//! coverage lives here (mirroring `delete_tests.rs`) so neither file grows past
//! its limit. Reuses the sibling `tests` module's `make_manager`/`seed_record`
//! helpers rather than duplicating the scaffolding.
//! What: name-validation unit tests plus success/no-op/collision/invalid/
//! terminal/live-tmux-rename cases for [`super::manager::SessionManager::rename`].
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver};
use super::record::{ManagedSessionId, ManagedSessionState};
use super::rename::validate_session_name;
use super::tests::{make_manager, seed_record};

#[test]
fn validate_session_name_accepts_valid_and_trims() {
    assert_eq!(
        validate_session_name("  tm-quiet-falcon ").unwrap(),
        "tm-quiet-falcon"
    );
    assert_eq!(
        validate_session_name("tm_worker_01").unwrap(),
        "tm_worker_01"
    );
}

#[test]
fn validate_session_name_rejects_empty_and_bad_chars() {
    assert!(
        validate_session_name("   ").is_err(),
        "empty/whitespace must reject"
    );
    assert!(
        validate_session_name("has space").is_err(),
        "whitespace must reject"
    );
    assert!(
        validate_session_name("has.dot").is_err(),
        "tmux-reserved '.' must reject"
    );
    assert!(
        validate_session_name("has:colon").is_err(),
        "tmux-reserved ':' must reject"
    );
    assert!(
        validate_session_name(&"x".repeat(65)).is_err(),
        "over-64-char name must reject"
    );
}

/// `rename` updates a stopped record's name and persists it.
#[tokio::test]
async fn rename_updates_name_and_persists() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    let updated = mgr.rename(&id, "tm-renamed-01").await.expect("rename");
    assert_eq!(updated.tmux_name, "tm-renamed-01");
    // The change is persisted — a fresh read sees the new name.
    let reread = mgr.get(&id).await.expect("get");
    assert_eq!(reread.tmux_name, "tm-renamed-01");
}

/// Renaming to the SAME name is a no-op that returns the record unchanged.
#[tokio::test]
async fn rename_same_name_is_noop() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;
    let current = mgr.get(&id).await.expect("get").tmux_name;

    let same = mgr.rename(&id, &current).await.expect("rename same");
    assert_eq!(same.tmux_name, current);
}

/// `rename` auto-suffixes — never rejects (issue #3692) — a name already held
/// by ANOTHER managed record, appending the smallest free `-N` ordinal.
#[tokio::test]
async fn rename_suffixes_collision_with_record() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let a = ManagedSessionId::new();
    let b = ManagedSessionId::new();
    seed_record(&mgr, &dir, a, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, b, ManagedSessionState::Stopped, false).await;
    let b_name = mgr.get(&b).await.expect("get b").tmux_name;

    let updated = mgr
        .rename(&a, &b_name)
        .await
        .expect("collision must auto-suffix, never reject");
    assert_eq!(updated.tmux_name, format!("{b_name}-2"));
    // `b`'s own record is untouched.
    assert_eq!(mgr.get(&b).await.expect("get b").tmux_name, b_name);
}

/// `rename` auto-suffixes — never rejects (issue #3692) — a name held by a
/// LIVE tmux session (even a foreign one not backed by any managed record).
#[tokio::test]
async fn rename_suffixes_collision_with_live_tmux() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;
    // A live tmux session not backed by any managed record.
    fake.create_session("tm-foreign-live", "/tmp")
        .expect("register foreign tmux");

    let updated = mgr
        .rename(&id, "tm-foreign-live")
        .await
        .expect("collision with a live foreign session must auto-suffix, never reject");
    assert_eq!(updated.tmux_name, "tm-foreign-live-2");
}

/// `rename` picks the smallest FREE ordinal — a second collision on top of an
/// already-taken `-2` must skip to `-3`, not fail or loop back.
#[tokio::test]
async fn rename_suffix_skips_to_next_free_ordinal() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let a = ManagedSessionId::new();
    let b = ManagedSessionId::new();
    let c = ManagedSessionId::new();
    seed_record(&mgr, &dir, a, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, b, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, c, ManagedSessionState::Stopped, false).await;
    let b_name = mgr.get(&b).await.expect("get b").tmux_name;

    let first = mgr
        .rename(&a, &b_name)
        .await
        .expect("first collision suffixes to -2");
    assert_eq!(first.tmux_name, format!("{b_name}-2"));

    let second = mgr
        .rename(&c, &b_name)
        .await
        .expect("second collision must skip the now-taken -2 and land on -3");
    assert_eq!(second.tmux_name, format!("{b_name}-3"));
}

/// Two CONCURRENT renames of two different STOPPED sessions to the same
/// target name must land on distinct names (#3692 review HIGH-3).
///
/// Why: a stopped record has no live tmux session, so nothing outside the
/// store lock serializes its rename — with a lock-free dedupe snapshot, both
/// tasks could observe the target as free, both pick it (or the same
/// ordinal), and both persist: two non-terminal records sharing one
/// `tmux_name`, the literal #3692 defect reintroduced by the fix's own
/// rename path. `rename` therefore holds the store write guard across its
/// whole check/dedupe/persist sequence.
/// What: seeds two Stopped records, fires both renames to `tm-contested`
/// concurrently via `tokio::join!`, and asserts the persisted names are
/// distinct — one bare, one `-2`-suffixed.
/// Test: this function IS the test.
#[tokio::test]
async fn rename_concurrent_stopped_renames_to_same_target_never_collide() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let a = ManagedSessionId::new();
    let b = ManagedSessionId::new();
    seed_record(&mgr, &dir, a, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, b, ManagedSessionState::Stopped, false).await;

    let (ra, rb) = tokio::join!(
        mgr.rename(&a, "tm-contested"),
        mgr.rename(&b, "tm-contested")
    );
    let ra = ra.expect("rename a");
    let rb = rb.expect("rename b");

    assert_ne!(
        ra.tmux_name, rb.tmux_name,
        "concurrent renames to one target must never both claim the same name"
    );
    let mut names = [ra.tmux_name.as_str(), rb.tmux_name.as_str()];
    names.sort_unstable();
    assert_eq!(
        names,
        ["tm-contested", "tm-contested-2"],
        "one wins the bare name, the other takes the -2 suffix"
    );
}

/// `rename` rejects an invalid new name with `InvalidState`.
#[tokio::test]
async fn rename_rejects_invalid_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    let err = mgr
        .rename(&id, "bad name!")
        .await
        .expect_err("invalid name must be rejected");
    assert!(
        matches!(err, ManagedError::InvalidState(_, _)),
        "got {err:?}"
    );
}

/// `rename` refuses a terminal (Decommissioned) record.
#[tokio::test]
async fn rename_rejects_terminal_record() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Decommissioned, false).await;

    let err = mgr
        .rename(&id, "tm-new-name")
        .await
        .expect_err("a terminal record must not be renamed");
    assert!(
        matches!(err, ManagedError::InvalidState(_, _)),
        "got {err:?}"
    );
}

/// A name held only by a TERMINAL (Deleted) tombstone is reusable — the
/// tombstone's `tmux_name` must NOT block a live session from taking it (MEDIUM).
#[tokio::test]
async fn rename_reuses_name_freed_by_a_deleted_record() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    // A live session we will rename, and a Deleted tombstone whose name we reuse.
    let live = ManagedSessionId::new();
    let gone = ManagedSessionId::new();
    seed_record(&mgr, &dir, live, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, gone, ManagedSessionState::Deleted, false).await;
    let freed = mgr.get(&gone).await.expect("get gone").tmux_name;

    let updated = mgr
        .rename(&live, &freed)
        .await
        .expect("a deleted tombstone's name must be reusable");
    assert_eq!(updated.tmux_name, freed);
}

/// `rename` renames the LIVE tmux session when one backs the record.
#[tokio::test]
async fn rename_renames_live_tmux_session() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    // Active seed registers a live tmux session in the fake driver.
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;
    let old_name = mgr.get(&id).await.expect("get").tmux_name;

    mgr.rename(&id, "tm-live-renamed").await.expect("rename");

    // The driver was told to rename old -> new.
    let renames = fake.rename_calls.lock().unwrap();
    assert!(
        renames
            .iter()
            .any(|(o, n)| o == &old_name && n == "tm-live-renamed"),
        "expected rename_session({old_name} -> tm-live-renamed), got {renames:?}"
    );
    drop(renames);
    // And the live tmux session now answers to the new name, not the old.
    assert!(
        fake.session_exists("tm-live-renamed"),
        "new name must be live"
    );
    assert!(!fake.session_exists(&old_name), "old name must be gone");
}
