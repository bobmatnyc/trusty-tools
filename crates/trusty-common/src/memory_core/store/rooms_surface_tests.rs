//! ADR-0027 T6 room surface: create, rename, and selector resolution.
//!
//! Why: `room_rename` is the repair path for the `unresolved-*` labels the
//! backfill synthesises, and it is the ONLY code in the room registry that
//! rewrites a row. Two properties carry it and must be proven rather than
//! asserted — (1) a rename changes ZERO `DRAWERS` bytes, and (2) it refuses to
//! silently merge two rooms. `room_create`'s documented idempotency gets the
//! same treatment, including under a concurrent second create.
//! What: unit tests over a real redb-backed `KnowledgeGraph` in a tempdir.
//! Test: this file.

use super::*;
use crate::memory_core::palace::Drawer;
use crate::memory_core::room_identity::parse_room_preserving_case;
use crate::memory_core::store::kg::KnowledgeGraph;
use tempfile::TempDir;

fn open_kg() -> (TempDir, KnowledgeGraph) {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("open kg");
    (dir, kg)
}

fn seed_legacy_drawer(kg: &KnowledgeGraph, room: &RoomType, content: &str) -> Drawer {
    let drawer = Drawer::new(room_to_uuid(room), content);
    kg.upsert_drawer_sync(&drawer).expect("upsert drawer");
    drawer
}

// ── room_create: idempotent, including under a race ──────────────────────

#[test]
fn create_room_is_idempotent() {
    let (_d, kg) = open_kg();
    let store = kg.store();
    let room = parse_room_preserving_case("Sprint Notes");

    let (first, created) =
        create_room(&store, &room, Some("planning notes".into())).expect("create");
    assert!(created, "first create writes the row");
    assert_eq!(
        first.label, "Sprint Notes",
        "the caller's spelling survives"
    );
    assert_eq!(first.description.as_deref(), Some("planning notes"));

    // Same name, different capitalisation: the canonical key lowercases, so
    // this must resolve to the SAME room rather than minting a second id.
    let (second, created) =
        create_room(&store, &parse_room_preserving_case("sprint notes"), None).expect("create");
    assert!(!created, "second create writes nothing");
    assert_eq!(second.id, first.id);
    assert_eq!(second.label, "Sprint Notes", "the stored spelling is kept");
    assert_eq!(
        second.description.as_deref(),
        Some("planning notes"),
        "a re-create must not blank the stored description"
    );
    assert_eq!(list_room_summaries(&store).unwrap().len(), 1);
}

#[test]
fn create_room_returns_the_winner_under_a_race() {
    // Idempotency must come from the insert-only write path, not from a
    // check-then-write that two threads can both pass.
    let (_d, kg) = open_kg();
    let store = kg.store();
    let room = RoomType::Custom("concurrent".to_string());

    let (a, b) = std::thread::scope(|s| {
        let ha = s.spawn(|| create_room(&store, &room, None).expect("create a"));
        let hb = s.spawn(|| create_room(&store, &room, None).expect("create b"));
        (ha.join().expect("join a"), hb.join().expect("join b"))
    });

    assert_eq!(a.0.id, b.0.id, "both callers converge on one room id");
    assert_eq!(
        usize::from(a.1) + usize::from(b.1),
        1,
        "exactly one caller reports having created the room"
    );
    assert_eq!(list_room_summaries(&store).unwrap().len(), 1);
}

#[test]
fn create_room_reuses_a_backfilled_legacy_id() {
    // A room that already exists in the data (legacy fold id) must not gain a
    // second, UUIDv5-keyed row the moment someone "creates" it by name.
    let (_d, kg) = open_kg();
    let legacy = RoomType::Custom("status".to_string());
    let drawers = vec![seed_legacy_drawer(&kg, &legacy, "legacy row")];
    crate::memory_core::store::room_backfill::backfill_rooms(&kg, &drawers).expect("backfill");

    let store = kg.store();
    let (summary, created) = create_room(&store, &legacy, None).expect("create");
    assert!(!created);
    assert_eq!(
        summary.id,
        room_to_uuid(&legacy),
        "the legacy id the drawers carry is reused verbatim"
    );
    assert_eq!(list_room_summaries(&store).unwrap().len(), 1);
}

// ── room_rename: the repair path, and what it must never touch ───────────

#[test]
fn rename_changes_no_drawer_rows() {
    // ADR-0027's headline safety property applied to the one update path:
    // renaming a room changes a NAME, never a membership. Proven with a
    // byte-level snapshot — a decoded comparison could mask a rewrite that
    // round-trips to the same value.
    let (_d, kg) = open_kg();
    let unnamed = RoomType::Custom("qqqqqqqqqqqqqqqqqq".to_string());
    let drawers = vec![
        seed_legacy_drawer(&kg, &unnamed, "orphan one"),
        seed_legacy_drawer(&kg, &unnamed, "orphan two"),
        seed_legacy_drawer(&kg, &RoomType::General, "general one"),
    ];
    crate::memory_core::store::room_backfill::backfill_rooms(&kg, &drawers).expect("backfill");

    let store = kg.store();
    let id = room_to_uuid(&unnamed);
    let before_row = store.get_room(id).unwrap().expect("row exists");
    assert!(before_row.label.starts_with("unresolved-"));
    assert!(!before_row.resolved);

    let before = store.raw_drawer_rows().expect("snapshot before");
    assert_eq!(before.len(), 3);

    let renamed = rename_room(&store, id, "  Sprint Notes  ").expect("rename");

    let after = store.raw_drawer_rows().expect("snapshot after");
    assert_eq!(
        before, after,
        "room_rename must not touch a single DRAWERS byte"
    );

    assert_eq!(
        renamed.label, "Sprint Notes",
        "the label is trimmed, not lowercased"
    );
    assert!(renamed.resolved, "a human-supplied name is resolved");
    // And every drawer still carries the id it always carried.
    for d in &drawers[..2] {
        assert_eq!(d.room_id, id);
    }
}

#[test]
fn rename_updates_label_and_key() {
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (created, _) = create_room(&store, &RoomType::Custom("old-name".into()), None).unwrap();

    rename_room(&store, created.id, "new-name").expect("rename");

    assert_eq!(
        resolve_room_selector(&store, "new-name").expect("new key resolves"),
        created.id
    );
    assert!(
        resolve_room_selector(&store, "old-name").is_err(),
        "the old canonical key is released"
    );
    assert_eq!(
        list_room_summaries(&store).unwrap().len(),
        1,
        "a rename never adds a row"
    );
}

#[test]
fn rename_to_a_builtin_name_recovers_the_builtin_kind() {
    // The room_type tag is re-derived through the ONE parser (ADR-0027 D4.1),
    // so repairing a room to `Backend` yields the built-in kind rather than
    // leaving a Custom body that a `Backend` filter would never match.
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (created, _) =
        create_room(&store, &RoomType::Custom("unresolved-ab12".into()), None).unwrap();

    let renamed = rename_room(&store, created.id, "Backend").expect("rename");
    assert_eq!(renamed.room_type, RoomType::Backend);
    assert_eq!(
        store.get_room(created.id).unwrap().unwrap().room_type,
        "Backend"
    );
}

#[test]
fn rename_rejects_a_key_owned_by_another_room() {
    // Merging two rooms is ADR-0027 D5 and is deliberately not built here —
    // silently folding one into the other would change which drawers a filter
    // returns without any caller asking for it.
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (a, _) = create_room(&store, &RoomType::Custom("alpha".into()), None).unwrap();
    let (b, _) = create_room(&store, &RoomType::Custom("beta".into()), None).unwrap();

    let err = rename_room(&store, a.id, "beta").expect_err("must refuse");
    assert!(
        format!("{err:#}").contains("already belongs to another room"),
        "unexpected error: {err:#}"
    );
    // Nothing moved.
    assert_eq!(resolve_room_selector(&store, "alpha").unwrap(), a.id);
    assert_eq!(resolve_room_selector(&store, "beta").unwrap(), b.id);
    assert_eq!(store.get_room(a.id).unwrap().unwrap().label, "alpha");
}

#[test]
fn rename_accepts_a_case_only_change() {
    // The canonical key is unchanged, so there is no key to move — only the
    // display spelling. This must not trip the "already owned" guard.
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (created, _) = create_room(&store, &RoomType::Custom("decisions".into()), None).unwrap();

    let renamed = rename_room(&store, created.id, "Decisions").expect("rename");
    assert_eq!(renamed.label, "Decisions");
    assert_eq!(
        resolve_room_selector(&store, "decisions").unwrap(),
        created.id
    );
}

#[test]
fn rename_requires_a_non_empty_name_and_an_existing_room() {
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (created, _) = create_room(&store, &RoomType::Custom("alpha".into()), None).unwrap();

    assert!(rename_room(&store, created.id, "   ").is_err());
    assert!(rename_room(&store, Uuid::from_u128(999), "anything").is_err());
}

// ── selector resolution ──────────────────────────────────────────────────

#[test]
fn selector_resolves_by_id_and_by_label() {
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (created, _) = create_room(&store, &RoomType::Custom("Sprint Notes".into()), None).unwrap();

    assert_eq!(
        resolve_room_selector(&store, &created.id.to_string()).unwrap(),
        created.id
    );
    assert_eq!(
        resolve_room_selector(&store, "sprint notes").unwrap(),
        created.id,
        "label lookup is case-insensitive"
    );
    assert!(
        resolve_room_selector(&store, "no-such-room").is_err(),
        "a typo must never silently create a room"
    );
    // A well-formed UUID with no row falls through to the label lookup and
    // then errors, rather than being accepted as a room that does not exist.
    assert!(resolve_room_selector(&store, &Uuid::from_u128(42).to_string()).is_err());
}
