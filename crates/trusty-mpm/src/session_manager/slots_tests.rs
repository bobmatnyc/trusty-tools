//! `numbered_snapshot` integration coverage — stable slot numbers survive a
//! delete, a deleted slot renders as a tombstone, and no slot is ever reused
//! within one `SessionManager` (issue #3034).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! #3034-specific coverage lives here so neither file grows past its limit,
//! mirroring the pattern established by `delete_tests.rs` / `dedup_tests.rs`.
//! [`super::slots`]'s own unit tests cover [`super::slots::SlotRegistry`] in
//! isolation; these tests exercise the integration through a real
//! `SessionManager` (create → list → numbered_snapshot → delete →
//! numbered_snapshot again) so the "deleted" concept — derived purely from
//! store membership, not a dedicated deletion hook — is proven against the
//! real `delete_record` path.
//! What: three tests: slot stability across a delete, tombstone
//! representation of the deleted slot, and no-reuse of a deleted number for a
//! session created afterward.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::tests::make_manager;

#[tokio::test]
async fn numbered_snapshot_keeps_slot_stable_across_delete() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let a = mgr
        .create(
            "task a".into(),
            Some(PathBuf::from("/tmp/wt-a")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create a");
    let b = mgr
        .create(
            "task b".into(),
            Some(PathBuf::from("/tmp/wt-b")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create b");
    let c = mgr
        .create(
            "task c".into(),
            Some(PathBuf::from("/tmp/wt-c")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create c");

    let before = mgr.numbered_snapshot(&mgr.list().await).await;
    let slot_of = |snap: &[super::slots::NumberedSlot], id: super::record::ManagedSessionId| {
        snap.iter()
            .find(|s| s.record.as_ref().map(|r| r.id) == Some(id))
            .expect("session present in snapshot")
            .slot
    };
    let slot_a = slot_of(&before, a.id);
    let slot_b = slot_of(&before, b.id);
    let slot_c = slot_of(&before, c.id);
    // The store does not guarantee `list()` returns records in creation
    // order, so only distinctness (never a shared/reused number) is
    // asserted here — the exact numbers are incidental.
    assert_ne!(slot_a, slot_b);
    assert_ne!(slot_b, slot_c);
    assert_ne!(slot_a, slot_c);

    // Deleting `b` must NOT shift `a` or `c`'s numbers — the exact
    // misdirection risk #3034 reports.
    mgr.delete_record(&b.id, true).await.expect("delete b");
    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    assert_eq!(slot_of(&after, a.id), slot_a);
    assert_eq!(
        slot_of(&after, c.id),
        slot_c,
        "c must keep its number after b is deleted"
    );
}

#[tokio::test]
async fn numbered_snapshot_tombstones_deleted_slot() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let a = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let before = mgr.numbered_snapshot(&mgr.list().await).await;
    assert_eq!(before.len(), 1);
    assert!(before[0].record.is_some());

    mgr.delete_record(&a.id, true).await.expect("delete");
    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    assert_eq!(after.len(), 1, "the tombstoned slot must still be reported");
    assert_eq!(after[0].slot, before[0].slot);
    assert!(
        after[0].record.is_none(),
        "a deleted session's slot must report no record"
    );
}

#[tokio::test]
async fn numbered_snapshot_never_reuses_a_deleted_slot() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let a = mgr
        .create(
            "task a".into(),
            Some(PathBuf::from("/tmp/wt-a")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create a");
    mgr.numbered_snapshot(&mgr.list().await).await; // assigns a's slot (1)
    mgr.delete_record(&a.id, true).await.expect("delete a");

    let d = mgr
        .create(
            "task d".into(),
            Some(PathBuf::from("/tmp/wt-d")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create d");
    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    let slot_d = after
        .iter()
        .find(|s| s.record.as_ref().map(|r| r.id) == Some(d.id))
        .expect("d present in snapshot")
        .slot;
    assert_eq!(
        slot_d, 2,
        "a fresh session must never receive a's tombstoned slot 1"
    );
    assert_eq!(
        after.len(),
        2,
        "slot 1 (tombstone) and slot 2 (d) must both be reported"
    );
}
