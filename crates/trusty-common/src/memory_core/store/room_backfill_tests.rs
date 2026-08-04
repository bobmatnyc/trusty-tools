//! ADR-0027 room registry: storage ops, backfill, and write-path resolution.
//!
//! Why: two safety properties carry this whole change and must be proven, not
//! asserted — (1) the backfill changes ZERO `DRAWERS` rows, and (2) it is
//! idempotent, so a human rename survives every reopen. Everything else here
//! round-trips the four label-resolution steps.
//! What: unit tests over a real redb-backed `KnowledgeGraph` in a tempdir.
//! Test: this file.

use super::*;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::room_identity::{
    DEFAULT_WING_ID, canonical_room_key, default_wing_key, mint_room_id, room_label, room_to_uuid,
};
use crate::memory_core::store::kg::{KnowledgeGraph, Triple};
use crate::memory_core::store::rooms::{RoomRecord, resolve_or_create_room_sync};
use chrono::Utc;
use tempfile::TempDir;
use uuid::Uuid;

fn open_kg() -> (TempDir, KnowledgeGraph) {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("open kg");
    (dir, kg)
}

/// Write one drawer sitting in the LEGACY id for `room` — i.e. exactly what
/// every pre-ADR-0027 write produced.
fn seed_legacy_drawer(kg: &KnowledgeGraph, room: &RoomType, content: &str) -> Drawer {
    let drawer = Drawer::new(room_to_uuid(room), content);
    kg.upsert_drawer_sync(&drawer).expect("upsert drawer");
    drawer
}

fn assert_triple(kg: &KnowledgeGraph, subject: &str) {
    kg.assert_sync(&Triple {
        subject: subject.to_string(),
        predicate: "contains".to_string(),
        object: "drawer:x".to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .expect("assert triple");
}

fn room_for(kg: &KnowledgeGraph, room: &RoomType) -> RoomRecord {
    kg.store()
        .get_room(room_to_uuid(room))
        .expect("get_room")
        .unwrap_or_else(|| panic!("no room row for {room:?}"))
}

// ── Safety property 1: not one drawer row changes ────────────────────────

#[test]
fn backfill_changes_no_drawer_rows() {
    let (_d, kg) = open_kg();
    let rooms = [
        RoomType::General,
        RoomType::Planning,
        RoomType::Custom("work".to_string()),
        RoomType::Custom("checkpoint".to_string()),
    ];
    let mut drawers = Vec::new();
    for (i, room) in rooms.iter().enumerate() {
        drawers.push(seed_legacy_drawer(&kg, room, &format!("legacy drawer {i}")));
    }

    // Byte-level snapshot: a decoded comparison could mask a rewrite that
    // round-trips to the same value.
    let before = kg.store().raw_drawer_rows().expect("snapshot before");
    assert_eq!(before.len(), rooms.len());

    let report = backfill_rooms(&kg, &drawers).expect("backfill");
    assert_eq!(report.observed, rooms.len());
    assert_eq!(report.inserted, rooms.len());

    let after = kg.store().raw_drawer_rows().expect("snapshot after");
    assert_eq!(
        before, after,
        "backfill must not touch a single DRAWERS byte"
    );

    // And the ids the drawers carry are exactly the ids now registered.
    for (room, drawer) in rooms.iter().zip(&drawers) {
        assert_eq!(drawer.room_id, room_to_uuid(room));
        assert!(kg.store().get_room(drawer.room_id).unwrap().is_some());
    }
}

// ── Safety property 2: idempotent, and a rename survives ─────────────────

#[test]
fn backfill_is_idempotent() {
    let (_d, kg) = open_kg();
    let drawers = vec![
        seed_legacy_drawer(&kg, &RoomType::General, "a"),
        seed_legacy_drawer(&kg, &RoomType::Custom("work".to_string()), "b"),
    ];

    let first = backfill_rooms(&kg, &drawers).expect("first pass");
    assert_eq!(first.inserted, 2);
    let second = backfill_rooms(&kg, &drawers).expect("second pass");
    assert_eq!(second.inserted, 0, "second pass inserts nothing");
    assert_eq!(second.skipped, 2);
    assert_eq!(kg.store().list_rooms().unwrap().len(), 2);
}

#[test]
fn backfill_rename_survives_second_run() {
    let (_d, kg) = open_kg();
    let unnamed = RoomType::Custom("qqqqqqqqqqqqqqqqqq".to_string());
    let drawers = vec![seed_legacy_drawer(&kg, &unnamed, "orphan")];

    backfill_rooms(&kg, &drawers).expect("first pass");
    let id = room_to_uuid(&unnamed);
    let before = kg.store().get_room(id).unwrap().expect("row exists");
    assert!(before.label.starts_with("unresolved-"));
    assert!(!before.resolved, "synthesised labels are marked unresolved");

    // Simulate the human rename that `room_rename` (ticket T6) will perform.
    let renamed = RoomRecord {
        label: "sprint-notes".to_string(),
        resolved: true,
        ..before
    };
    kg.store().force_put_room(id, &renamed).expect("rename");

    backfill_rooms(&kg, &drawers).expect("second pass");
    let after = kg.store().get_room(id).unwrap().expect("row survives");
    assert_eq!(
        after.label, "sprint-notes",
        "insert-only preserves a rename"
    );
    assert!(after.resolved);
}

// ── Label resolution: the four steps ─────────────────────────────────────

#[test]
fn backfill_resolves_builtin_rooms() {
    // Step 1 — the rainbow table. A match is proof: it is the exact function
    // that produced the id.
    let (_d, kg) = open_kg();
    let builtins = [
        RoomType::Frontend,
        RoomType::Backend,
        RoomType::Testing,
        RoomType::Planning,
        RoomType::Documentation,
        RoomType::Research,
        RoomType::Configuration,
        RoomType::Meetings,
        RoomType::General,
    ];
    let drawers: Vec<Drawer> = builtins
        .iter()
        .map(|r| seed_legacy_drawer(&kg, r, "x"))
        .collect();
    let report = backfill_rooms(&kg, &drawers).expect("backfill");
    assert_eq!(report.unresolved, 0, "every built-in resolves exactly");

    for room in &builtins {
        let record = room_for(&kg, room);
        assert_eq!(record.label, room_label(room), "{room:?}");
        assert_eq!(record.room_type(), *room);
        assert!(record.resolved);
        assert_eq!(Uuid::from_bytes(record.wing_id), DEFAULT_WING_ID);
    }
}

#[test]
fn backfill_inverts_short_custom_labels() {
    // Step 2 — direct fold inversion. `Custom("work")` is 14 bytes of `Debug`
    // repr and `Custom("status")` is exactly 16, the longest still-injective
    // input; both must invert without any dictionary.
    let (_d, kg) = open_kg();
    let rooms = [
        RoomType::Custom("work".to_string()),
        RoomType::Custom("status".to_string()),
    ];
    let drawers: Vec<Drawer> = rooms
        .iter()
        .map(|r| seed_legacy_drawer(&kg, r, "x"))
        .collect();
    let report = backfill_rooms(&kg, &drawers).expect("backfill");
    assert_eq!(report.unresolved, 0);

    for room in &rooms {
        let record = room_for(&kg, room);
        assert_eq!(record.room_type(), *room);
        assert_eq!(record.room_type, "Custom");
        assert!(record.resolved);
    }
    // The ADR measured these ids live; pin the mapping end to end.
    assert_eq!(
        room_to_uuid(&RoomType::Custom("work".to_string())).to_string(),
        "43767577-7372-2e29-7f78-7c762e360000"
    );
}

#[test]
fn backfill_uses_kg_dictionary() {
    // Step 3 — a wrapped label (`Custom("decisions")` is 19 bytes of `Debug`
    // repr, past the 16-byte fold) is not invertible, but the palace's own KG
    // wrote a `room:decisions` subject we can hash and check.
    let (_d, kg) = open_kg();
    let room = RoomType::Custom("decisions".to_string());
    assert!(
        format!("{room:?}").len() > 16,
        "this test is only meaningful in the fold's wrap zone"
    );
    assert_triple(&kg, "room:decisions");
    let drawers = vec![seed_legacy_drawer(&kg, &room, "adr")];

    let report = backfill_rooms(&kg, &drawers).expect("backfill");
    assert_eq!(report.unresolved, 0, "the dictionary must name it");
    let record = room_for(&kg, &room);
    assert_eq!(record.label, "decisions");
    assert!(record.resolved);
}

#[test]
fn backfill_falls_back_to_unresolved() {
    // Step 4 — nothing matched. Nothing is lost: the drawer keeps its
    // room_id, the room becomes visible, and a rename can fix the name.
    let (_d, kg) = open_kg();
    let room = RoomType::Custom("zzzzzzzzzzzzzzzzzzzz".to_string());
    let drawer = seed_legacy_drawer(&kg, &room, "mystery");
    let drawers = vec![drawer.clone()];

    let report = backfill_rooms(&kg, &drawers).expect("backfill");
    assert_eq!(report.inserted, 1);
    assert_eq!(report.unresolved, 1);

    let id = room_to_uuid(&room);
    let record = kg.store().get_room(id).unwrap().expect("row exists");
    assert_eq!(record.label, format!("unresolved-{}", &id.to_string()[..8]));
    assert!(!record.resolved);
    assert_eq!(record.room_type, "Custom");
    // The drawer was NOT reassigned.
    assert_eq!(drawer.room_id, id);
    assert_eq!(kg.store().raw_drawer_rows().unwrap().len(), 1);
}

#[test]
fn backfill_stamps_schema_version_and_skips_marker_row() {
    let (_d, kg) = open_kg();
    assert_eq!(kg.store().room_schema_version().unwrap(), None);
    let drawers = vec![seed_legacy_drawer(&kg, &RoomType::General, "x")];
    backfill_rooms(&kg, &drawers).expect("backfill");
    assert_eq!(
        kg.store().room_schema_version().unwrap(),
        Some(crate::memory_core::store::rooms::ROOM_SCHEMA_VERSION)
    );
    // The marker must never appear as a room.
    let rooms = kg.store().list_rooms().unwrap();
    assert_eq!(rooms.len(), 1);
    assert!(rooms.iter().all(|(id, _)| !id.is_nil()));
}

#[test]
fn backfill_fail_open_is_noop_without_drawers() {
    let (_d, kg) = open_kg();
    backfill_rooms_fail_open("empty-palace", &kg, &[]);
    assert!(kg.store().list_rooms().unwrap().is_empty());
    assert_eq!(kg.store().room_schema_version().unwrap(), None);
}

// ── Store-level insert-only semantics ────────────────────────────────────

#[test]
fn room_insert_is_insert_only() {
    let (_d, kg) = open_kg();
    let id = Uuid::from_u128(42);
    let key = canonical_room_key(DEFAULT_WING_ID, "planning");
    let first = RoomRecord::new(&RoomType::Planning, 1, true);
    assert!(kg.store().insert_room_if_absent(id, &key, &first).unwrap());

    let second = RoomRecord::new(&RoomType::Custom("other".to_string()), 2, false);
    assert!(
        !kg.store().insert_room_if_absent(id, &key, &second).unwrap(),
        "a second insert under the same id reports no write"
    );
    assert_eq!(kg.store().get_room(id).unwrap().unwrap().label, "Planning");
}

#[test]
fn room_key_collision_keeps_both_rows() {
    // ADR-0027 D4.1: `Backend` and a legacy `Custom("backend")` normalise onto
    // one canonical key. Both keep a row (ids are unique); the first one seen
    // owns the name, and the backfill's sorted visit order makes that
    // deterministic.
    let (_d, kg) = open_kg();
    let key = default_wing_key(&RoomType::Backend);
    assert_eq!(key, default_wing_key(&RoomType::Custom("backend".into())));

    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    kg.store()
        .insert_room_if_absent(first, &key, &RoomRecord::new(&RoomType::Backend, 1, true))
        .unwrap();
    kg.store()
        .insert_room_if_absent(
            second,
            &key,
            &RoomRecord::new(&RoomType::Custom("backend".into()), 2, true),
        )
        .unwrap();

    assert_eq!(kg.store().list_rooms().unwrap().len(), 2, "both enumerable");
    assert_eq!(kg.store().lookup_room_id(&key).unwrap(), Some(first));
}

#[test]
fn room_key_lookup_returns_registered_id() {
    let (_d, kg) = open_kg();
    let key = default_wing_key(&RoomType::Research);
    assert_eq!(kg.store().lookup_room_id(&key).unwrap(), None);
    let id = Uuid::from_u128(9);
    kg.store()
        .insert_room_if_absent(id, &key, &RoomRecord::new(&RoomType::Research, 1, true))
        .unwrap();
    assert_eq!(kg.store().lookup_room_id(&key).unwrap(), Some(id));
}

// ── Write-path resolution (T4) ───────────────────────────────────────────

#[test]
fn resolve_or_create_is_idempotent() {
    let (_d, kg) = open_kg();
    let room = RoomType::Custom("newroom".to_string());
    let store = kg.store();
    let a = resolve_or_create_room_sync(&store, &room).expect("first");
    let b = resolve_or_create_room_sync(&store, &room).expect("second");
    assert_eq!(a, b);
    assert_eq!(
        a,
        mint_room_id(&default_wing_key(&room)),
        "a brand-new room mints UUIDv5, not the legacy fold"
    );
    assert_ne!(a, room_to_uuid(&room));
    assert_eq!(store.list_rooms().unwrap().len(), 1);
}

#[test]
fn resolve_or_create_reuses_backfilled_legacy_id() {
    // The continuity guarantee: a write into a room that already exists lands
    // on the id its existing drawers carry, so nothing has to be rewritten.
    let (_d, kg) = open_kg();
    let room = RoomType::Custom("work".to_string());
    let drawers = vec![seed_legacy_drawer(&kg, &room, "legacy")];
    backfill_rooms(&kg, &drawers).expect("backfill");

    let resolved = resolve_or_create_room_sync(&kg.store(), &room).expect("resolve");
    assert_eq!(resolved, room_to_uuid(&room), "legacy id kept verbatim");
    assert_eq!(kg.store().list_rooms().unwrap().len(), 1, "no new room");
}

#[test]
fn resolve_or_create_unifies_case_variants() {
    // ADR-0027 D1.3: `Decisions` and `decisions` are one room, and the
    // first-seen spelling is what the label keeps.
    let (_d, kg) = open_kg();
    let store = kg.store();
    let a = resolve_or_create_room_sync(&store, &RoomType::Custom("Decisions".into())).unwrap();
    let b = resolve_or_create_room_sync(&store, &RoomType::Custom("decisions".into())).unwrap();
    assert_eq!(a, b);
    assert_eq!(store.get_room(a).unwrap().unwrap().label, "Decisions");
}

// ── Read-side filter resolution ──────────────────────────────────────────

#[test]
fn filter_id_falls_back_to_legacy_fold() {
    // An un-backfilled palace (or a filter naming a room that does not exist)
    // must filter exactly as it did before ADR-0027.
    let (_d, kg) = open_kg();
    for room in [RoomType::General, RoomType::Custom("nope".into())] {
        assert_eq!(
            crate::memory_core::store::rooms::resolve_room_filter_id(&kg, &room),
            room_to_uuid(&room),
            "{room:?}"
        );
    }
}

#[test]
fn filter_id_prefers_registered_room() {
    // Without this, every room minted after ADR-0027 would be unfilterable —
    // an invisible failure, since a filter that matches nothing returns an
    // empty list rather than an error.
    let (_d, kg) = open_kg();
    let room = RoomType::Custom("brandnew".to_string());
    let minted = resolve_or_create_room_sync(&kg.store(), &room).expect("resolve");
    assert_ne!(minted, room_to_uuid(&room));
    assert_eq!(
        crate::memory_core::store::rooms::resolve_room_filter_id(&kg, &room),
        minted
    );
}

#[test]
fn filter_id_matches_write_path_after_backfill() {
    // End-to-end: legacy drawer -> backfill -> a filter for that room resolves
    // to the id the drawer already carries.
    let (_d, kg) = open_kg();
    let room = RoomType::Custom("status".to_string());
    let drawer = seed_legacy_drawer(&kg, &room, "203 of these live in trusty-tools");
    backfill_rooms(&kg, std::slice::from_ref(&drawer)).expect("backfill");
    assert_eq!(
        crate::memory_core::store::rooms::resolve_room_filter_id(&kg, &room),
        drawer.room_id
    );
}
