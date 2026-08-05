//! `trusty-memory rooms backfill` audit-path tests (ADR-0027 T10).
//!
//! Why: the one property this command exists to guarantee is that `--dry-run`
//! writes NOTHING — it is the pre-flight an operator runs against a live palace
//! before letting a migration touch it. That is proven here with a byte-level
//! comparison of the palace's on-disk state, not by inspecting the returned
//! plan (which could be right while the command still wrote).
//! What: seeds a palace's `kg.db` with legacy-id drawers directly (bypassing
//! the registry, whose open path runs the backfill), then audits it.
//! Test: this file.

use super::*;
use std::collections::BTreeMap;
use std::path::Path;
use trusty_common::memory_core::palace::{Drawer, PalaceId, RoomType};
use trusty_common::memory_core::room_identity::room_to_uuid;
use trusty_common::memory_core::store::PalaceStore;

/// Create a palace on disk holding one legacy-id drawer per room.
///
/// Deliberately does NOT go through `PalaceRegistry`: every registry open path
/// runs the at-open backfill, so a registry-seeded fixture would already have
/// its `ROOMS` rows and could not prove anything about a first run.
fn seed_palace(root: &Path, id: &str, rooms: &[RoomType]) -> Palace {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).expect("create palace dir");
    PalaceStore::save_palace(&palace).expect("save palace");
    let handle =
        PalaceHandle::open_with_intent(&palace, OpenIntent::Writer).expect("open for seeding");
    for (i, room) in rooms.iter().enumerate() {
        let drawer = Drawer::new(room_to_uuid(room), format!("legacy drawer {i}"));
        handle
            .kg
            .upsert_drawer_sync(&drawer)
            .expect("seed legacy drawer");
    }
    drop(handle);
    palace
}

/// `(room_id, label, resolved)` for every registered room.
type RoomRows = Vec<(String, String, bool)>;
/// `drawer_id -> room_id` for every drawer on disk.
type DrawerRooms = BTreeMap<String, String>;

/// The palace's room registry and drawer set, as a comparable snapshot.
///
/// Why NOT a byte-for-byte comparison of the database files: redb rewrites its
/// own header when a database is opened, even for a pure read, so every one of
/// this palace's three `.redb` files differs after any open regardless of
/// whether a row was written. That instrument cannot distinguish "we opened it"
/// from "we wrote to it". The byte-level proof therefore lives in the crate
/// that owns the tables — `store::room_backfill::tests::dry_run_plan_writes_nothing`
/// compares the raw `ROOMS` / `ROOM_KEYS` / `DRAWERS` bytes — and what this
/// command-level test proves is the complementary half: driving the CLI over a
/// real palace directory leaves the registry and the drawers exactly as they
/// were.
/// What: `(registered rooms, drawer id -> room id)` read back off disk through
/// a fresh handle, so nothing in-process is being trusted.
fn registry_snapshot(palace: &Palace) -> (RoomRows, DrawerRooms) {
    let handle = PalaceHandle::open_with_intent(palace, OpenIntent::ReadOnlyClient)
        .expect("reopen for snapshot");
    let rooms = handle
        .kg
        .store()
        .list_rooms()
        .expect("list rooms")
        .into_iter()
        .map(|(id, r)| (id.to_string(), r.label, r.resolved))
        .collect();
    let drawers = handle
        .drawers
        .read()
        .iter()
        .map(|d| (d.id.to_string(), d.room_id.to_string()))
        .collect();
    (rooms, drawers)
}

#[test]
fn dry_run_reports_without_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rooms = [
        RoomType::General,
        RoomType::Custom("status".to_string()),
        RoomType::Custom("qqqqqqqqqqqqqqqqqq".to_string()),
    ];
    let palace = seed_palace(tmp.path(), "audit", &rooms);

    let before = registry_snapshot(&palace);
    assert!(
        before.0.is_empty(),
        "no rooms registered before the dry run"
    );
    assert_eq!(before.1.len(), rooms.len(), "one drawer per room seeded");

    let audits = audit_palaces(tmp.path(), None, false).expect("dry run");

    assert_eq!(
        before,
        registry_snapshot(&palace),
        "a --dry-run audit must leave the room registry and every drawer untouched"
    );

    assert_eq!(audits.len(), 1);
    let audit = &audits[0];
    assert!(audit.error.is_none(), "{:?}", audit.error);
    assert_eq!(audit.inserted, None, "a dry run never reports a write");
    assert_eq!(audit.entries.len(), rooms.len());
    assert_eq!(audit.pending(), rooms.len(), "every room is still pending");
    // And it reports what the labels WOULD be, not placeholders.
    let labels: Vec<String> = audit.entries.iter().map(|e| e.label()).collect();
    assert!(labels.contains(&"General".to_string()));
    assert!(labels.contains(&"status".to_string()));
    assert!(
        labels.iter().any(|l| l.starts_with("unresolved-")),
        "the un-invertible room is named, not dropped: {labels:?}"
    );
}

#[test]
fn apply_writes_the_planned_rooms() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rooms = [RoomType::Planning, RoomType::Custom("work".to_string())];
    let palace = seed_palace(tmp.path(), "applied", &rooms);

    let planned = audit_palaces(tmp.path(), None, false).expect("dry run");
    let planned_labels: Vec<String> = planned[0].entries.iter().map(|e| e.label()).collect();

    let applied = audit_palaces(tmp.path(), None, true).expect("apply");
    assert_eq!(applied[0].inserted, Some(rooms.len()));
    assert_eq!(
        applied[0]
            .entries
            .iter()
            .map(|e| e.label())
            .collect::<Vec<_>>(),
        planned_labels,
        "--apply must register exactly what --dry-run previewed"
    );

    // Re-auditing shows nothing left to do: the write is idempotent.
    let again = audit_palaces(tmp.path(), None, false).expect("second dry run");
    assert_eq!(again[0].pending(), 0);
    assert!(again[0].entries.iter().all(|e| !e.would_insert()));
    let _ = palace;
}

#[test]
fn audit_can_be_scoped_to_one_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_palace(tmp.path(), "alpha", &[RoomType::General]);
    seed_palace(tmp.path(), "beta", &[RoomType::Planning]);

    let all = audit_palaces(tmp.path(), None, false).expect("all");
    assert_eq!(all.len(), 2);

    let one = audit_palaces(tmp.path(), Some("beta"), false).expect("one");
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].palace_id, "beta");
}
