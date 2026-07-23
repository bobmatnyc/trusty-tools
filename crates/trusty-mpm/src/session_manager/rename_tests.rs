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

/// A store-write failure AFTER the live tmux session was renamed must roll
/// the tmux rename back (#3698 round-2 HIGH-B pattern applied to `rename`):
/// the live session and its record must never desync.
///
/// Why: since the round-2 HIGH-A restructure, the tmux rename happens with
/// the store guard RELEASED; the subsequent persist can fail independently.
/// Without compensation the session would answer to the new name while the
/// store still records the old one.
/// What: seeds an Active record (live tmux), makes the store directory
/// read-only so the next persist fails, renames, and asserts the call
/// errored, the driver saw BOTH the rename and its rollback, and the live
/// session answers to its ORIGINAL name again.
/// Test: this function IS the test.
#[cfg(unix)]
#[tokio::test]
async fn rename_rolls_back_tmux_when_store_write_fails() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;
    let old_name = mgr.get(&id).await.expect("get").tmux_name;

    // Make the store dir read-only: reads (the reload) still work, but the
    // save's `sessions.json.tmp` creation fails — a targeted upsert failure.
    let dir_perms = std::fs::metadata(dir.path()).expect("meta").permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
        .expect("make store dir read-only");

    let err = mgr
        .rename(&id, "tm-doomed-rename")
        .await
        .expect_err("rename must fail when the store write fails");

    // Restore permissions FIRST so TempDir cleanup works even on assert failure.
    std::fs::set_permissions(dir.path(), dir_perms).expect("restore perms");

    assert!(
        matches!(err, ManagedError::InvalidState(_, _)),
        "got {err:?}"
    );
    let renames = fake.rename_calls.lock().unwrap();
    assert!(
        renames
            .iter()
            .any(|(o, n)| o == &old_name && n == "tm-doomed-rename"),
        "the forward rename must have happened: {renames:?}"
    );
    assert!(
        renames
            .iter()
            .any(|(o, n)| o == "tm-doomed-rename" && n == &old_name),
        "the compensating rollback rename must have happened: {renames:?}"
    );
    drop(renames);
    assert!(
        fake.session_exists(&old_name),
        "the live session must answer to its ORIGINAL name after rollback"
    );
    assert!(
        !fake.session_exists("tm-doomed-rename"),
        "the failed new name must not linger on the live session"
    );
    // NOTE deliberately NOT asserted: the in-memory cached record's name.
    // `upsert` mutates the cache before the (failed) disk save — a
    // pre-existing store semantic outside this test's scope; the DURABLE
    // on-disk record still carries the old name, and the tmux rollback above
    // is what this test exists to prove.
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

/// A LEGACY record (pre-#2453, no captured `pane_id`) must keep the
/// pre-#3714 name-only tmux-liveness check for the rename MUTATION path —
/// there is no stronger per-pane signal available for it, so `rename` must
/// still physically rename the live tmux session exactly as before #3714.
///
/// Why: proves the legacy fallback explicitly, rather than incidentally —
/// `pane_exists_override` is forced to `Some(false)` (an incorrect gone
/// answer, if it were ever consulted): if the mutation path mistakenly
/// probed `pane_exists` for a record with no `pane_id`, the tmux rename
/// would be skipped and this assertion would fail. Observing the rename
/// call fire anyway is proof the pane check was never reached — the
/// `pane_id: None` branch short-circuits straight to the name-only check.
#[tokio::test]
async fn rename_legacy_record_without_pane_id_still_renames_live_tmux_session() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    // seed_record never sets `pane_id` (always `None`) — the legacy shape.
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;
    assert_eq!(
        mgr.get(&id).await.expect("get").pane_id,
        None,
        "sanity: seeded record is legacy (no pane_id)"
    );
    let old_name = mgr.get(&id).await.expect("get").tmux_name;
    // If the pane check were (wrongly) consulted for this legacy record, it
    // would report "gone" — proving below that the rename still happened
    // means it was never consulted.
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    mgr.rename(&id, "tm-legacy-renamed").await.expect("rename");

    let renames = fake.rename_calls.lock().unwrap();
    assert!(
        renames
            .iter()
            .any(|(o, n)| o == &old_name && n == "tm-legacy-renamed"),
        "a legacy record's rename must still physically rename its live tmux \
         session via the name-only check: {renames:?}"
    );
}

/// #3714 remediation: renaming a record whose OWN recorded pane is confirmed
/// GONE must NEVER physically rename a tmux session merely because a session
/// with the same NAME is live — that live session belongs to a DIFFERENT
/// record (the duplicate-name collision from #3692), and the rename must not
/// hijack it. Only the DB record changes; the live tmux entity is left
/// untouched.
///
/// Why: the pre-#3714 code trusted `live_names.contains(old_name)` alone —
/// this reproduces the remediation-comment incident where `tm sessions
/// rename` on a stale duplicate physically retitled the operator's OWN,
/// attached, unrelated live session.
/// What: creates an Active record with a captured `pane_id` (`%3`), then
/// forces `pane_exists` to report `false` (simulating that this record's own
/// pane is gone even though a tmux session still answers to `old_name` — the
/// exact shape of "a DIFFERENT record's live session happens to share this
/// name"). Asserts the rename still succeeds (DB-only), the driver's
/// `rename_session` is NEVER called, and the live session (still under
/// `old_name`) is left completely alone.
#[tokio::test]
async fn rename_never_renames_unrelated_live_session_sharing_a_stale_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    *fake.pane_id_override.lock().unwrap() = Some("%3".to_string());

    let record = mgr
        .create(
            "task".into(),
            Some(dir.path().to_path_buf()),
            Some("dup-name-session".into()),
            Some(dir.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("create");
    assert_eq!(
        record.pane_id.as_deref(),
        Some("%3"),
        "sanity: pane_id captured at create time"
    );
    mgr.set_workspace(
        &record.id,
        dir.path().to_path_buf(),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");
    let old_name = record.tmux_name.clone();

    // Simulate the #3714 duplicate-name condition: `old_name` is still a
    // LIVE tmux session (list_sessions reports it — `create()` registered it
    // above), but THIS record's own recorded pane is confirmed gone, as if
    // the live session actually belongs to an unrelated record.
    *fake.pane_exists_override.lock().unwrap() = Some(false);

    let updated = mgr
        .rename(&record.id, "tm-renamed-safely")
        .await
        .expect("rename must still succeed — DB-only when the tmux identity can't be confirmed");
    assert_eq!(updated.tmux_name, "tm-renamed-safely");

    assert!(
        fake.rename_calls.lock().unwrap().is_empty(),
        "must never physically rename a tmux session that isn't confirmed to be this record's own"
    );
    assert!(
        fake.session_exists(&old_name),
        "the unrelated live session (still answering to old_name) must remain untouched"
    );
    assert!(
        !fake.session_exists("tm-renamed-safely"),
        "no tmux session should have been created/renamed under the new name"
    );
}

/// Regression guard for the #3714 fix's happy path: when the record's OWN
/// `pane_id` IS confirmed alive (the ordinary case — no duplicate-name
/// collision), the live tmux session is still renamed exactly as before.
#[tokio::test]
async fn rename_renames_live_session_when_pane_confirmed_alive() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    *fake.pane_id_override.lock().unwrap() = Some("%9".to_string());

    let record = mgr
        .create(
            "task".into(),
            Some(dir.path().to_path_buf()),
            Some("own-pane-session".into()),
            Some(dir.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("create");
    mgr.set_workspace(
        &record.id,
        dir.path().to_path_buf(),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");
    let old_name = record.tmux_name.clone();
    // `pane_exists_override` stays at its default (`None` -> trait's
    // optimistic `true`), matching a real driver confirming the pane is
    // there.

    mgr.rename(&record.id, "tm-live-renamed-2")
        .await
        .expect("rename");

    let renames = fake.rename_calls.lock().unwrap();
    assert!(
        renames
            .iter()
            .any(|(o, n)| o == &old_name && n == "tm-live-renamed-2"),
        "a confirmed-alive pane must still rename the live tmux session: {renames:?}"
    );
}
