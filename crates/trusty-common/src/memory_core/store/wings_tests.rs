//! ADR-0027 T9 wing registry: seeding safety, idempotency, create, rename.
//!
//! Why: three safety properties carry the wing half of ADR-0027 and must be
//! proven, not asserted — (1) placing existing rooms in the default wing
//! changes ZERO drawer rows AND ZERO room rows, (2) seeding is idempotent so a
//! rename survives every reopen, and (3) a caller who never mentions a wing is
//! unaffected. Everything else round-trips create / rename / list.
//! What: unit tests over a real redb-backed `KnowledgeGraph` in a tempdir.
//! Test: this file.

use super::*;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::room_identity::room_to_uuid;
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::kg_store::{decode_value, encode_value};
use crate::memory_core::store::room_backfill::backfill_rooms;
use crate::memory_core::store::rooms::{RoomRecord, resolve_or_create_room_sync};
use crate::memory_core::wing_identity::mint_wing_id;
use tempfile::TempDir;

fn open_kg() -> (TempDir, KnowledgeGraph) {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("open kg");
    (dir, kg)
}

/// Write one drawer sitting in the LEGACY id for `room` — i.e. exactly what
/// every pre-ADR-0027 write produced — then register it the way palace open
/// does. This reproduces the real starting state a wing migration meets.
fn seed_legacy_room(kg: &KnowledgeGraph, room: &RoomType) -> Drawer {
    let drawer = Drawer::new(room_to_uuid(room), "legacy content");
    kg.upsert_drawer_sync(&drawer).expect("upsert drawer");
    drawer
}

// ── Record shape ─────────────────────────────────────────────────────────

/// Simulates the NEXT schema revision — the mechanism ADR-0027 D2 relies on
/// for hanging #3064's per-wing access configuration without a migration.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FutureWingRecord {
    label: String,
    created_at_ms: i64,
    description: Option<String>,
    /// The hypothetical new field.
    access: Option<String>,
}

#[test]
fn wing_record_round_trip() {
    let r = WingRecord::new("engineer", 1_700_000_000_000, Some("owns impl".into()));
    let bytes = encode_value(&r).expect("encode");
    let back: WingRecord = decode_value(&bytes).expect("decode");
    assert_eq!(r, back);
    assert_eq!(back.label, "engineer");
}

#[test]
fn wing_record_decodes_under_a_future_field() {
    // Postcard is positional, so the naive future decode MUST fail — the
    // fallback chain in `decode_wing_record` is what carries old rows forward,
    // exactly as `RoomRecord` and `DrawerRecord` already do.
    let bytes = encode_value(&WingRecord::new("pm", 1, None)).expect("encode");
    assert!(
        decode_value::<FutureWingRecord>(&bytes).is_err(),
        "postcard is positional: the naive future decode must fail"
    );
    let migrated: WingRecord = decode_value(&bytes).expect("fallback to current shape");
    assert_eq!(migrated.label, "pm");
}

// ── Safety property 1: seeding rewrites nothing ──────────────────────────

#[test]
fn seeding_the_default_wing_changes_no_room_or_drawer_rows() {
    // The headline migration guarantee, carried onto the wing axis: existing
    // rooms land in the default wing by NAMING, never reclassification.
    // Byte-level snapshots on BOTH tables — a decoded comparison could mask a
    // rewrite that round-trips to the same value.
    let (_d, kg) = open_kg();
    let rooms = [
        RoomType::General,
        RoomType::Planning,
        RoomType::Custom("status".to_string()),
        RoomType::Custom("decisions".to_string()),
    ];
    let drawers: Vec<Drawer> = rooms.iter().map(|r| seed_legacy_room(&kg, r)).collect();
    backfill_rooms(&kg, &drawers).expect("room backfill");

    let drawers_before = kg
        .store()
        .raw_drawer_rows()
        .expect("drawer snapshot before");
    let rooms_before = kg.store().raw_room_rows().expect("room snapshot before");
    assert_eq!(drawers_before.len(), rooms.len());
    assert!(!rooms_before.is_empty());

    assert!(ensure_default_wing(&kg).expect("seed"), "wrote a wing row");

    let drawers_after = kg.store().raw_drawer_rows().expect("drawer snapshot after");
    let rooms_after = kg.store().raw_room_rows().expect("room snapshot after");
    assert_eq!(
        drawers_before, drawers_after,
        "seeding must not touch a single DRAWERS byte"
    );
    assert_eq!(
        rooms_before, rooms_after,
        "seeding must not touch a single ROOMS byte — no room id is rewritten"
    );

    // And every pre-existing room is genuinely IN the default wing already.
    let scoped = rooms_in_wing(&kg, DEFAULT_WING_ID).expect("scope");
    for drawer in &drawers {
        assert!(
            scoped.contains(&drawer.room_id),
            "legacy room {} must already belong to the default wing",
            drawer.room_id
        );
    }
}

// ── Safety property 2: idempotent, and a rename survives ─────────────────

#[test]
fn default_wing_is_seeded_once() {
    let (_d, kg) = open_kg();
    assert_eq!(kg.store().wing_schema_version().expect("version"), None);
    assert!(ensure_default_wing(&kg).expect("first"), "first seeds");
    assert_eq!(
        kg.store().wing_schema_version().expect("version"),
        Some(WING_SCHEMA_VERSION)
    );
    assert!(
        !ensure_default_wing(&kg).expect("second"),
        "second is a no-op"
    );

    let wings = list_wings(&kg).expect("list");
    assert_eq!(wings.len(), 1);
    assert_eq!(wings[0].id, DEFAULT_WING_ID);
    assert_eq!(wings[0].label, "default");
    assert!(wings[0].is_default);
    // The marker row must never surface as a wing.
    assert!(wings.iter().all(|w| !w.id.is_nil()));
}

#[test]
fn wing_rename_survives_reseed() {
    // The prompt's idempotency bar: run twice, and a wing renamed between runs
    // keeps its new name. Seeding probes by ID, so it cannot resurrect either
    // the old label or the old key.
    let (_d, kg) = open_kg();
    ensure_default_wing(&kg).expect("seed");
    let renamed = rename_wing_sync(&kg.store(), DEFAULT_WING_ID, "platform").expect("rename");
    assert_eq!(renamed.label, "platform");

    assert!(!ensure_default_wing(&kg).expect("reseed"), "no second row");
    let wings = list_wings(&kg).expect("list");
    assert_eq!(wings.len(), 1, "still exactly one wing");
    assert_eq!(wings[0].label, "platform", "the rename survived");
    assert_eq!(wings[0].id, DEFAULT_WING_ID, "the id never moved");
    assert_eq!(
        resolve_wing_selector(&kg, "default").expect("selector"),
        None,
        "the retired label must not resurrect as an alias"
    );
}

#[test]
fn wing_insert_is_insert_only() {
    let (_d, kg) = open_kg();
    let id = Uuid::from_u128(42);
    let key = canonical_wing_key("engineer");
    let first = WingRecord::new("engineer", 1, None);
    assert!(kg.store().insert_wing_if_absent(id, &key, &first).unwrap());
    let second = WingRecord::new("SOMETHING ELSE", 2, None);
    assert!(
        !kg.store().insert_wing_if_absent(id, &key, &second).unwrap(),
        "a second insert under the same id reports no write"
    );
    assert_eq!(kg.store().get_wing(id).unwrap().unwrap().label, "engineer");
}

// ── Safety property 3: a caller who never names a wing is unaffected ─────

#[test]
fn seeding_leaves_room_resolution_byte_identical() {
    // "Wing is never a required concept for a caller" (ADR-0027 D2). A write
    // that names no wing must resolve to the SAME room id before and after the
    // wing registry exists — including for a legacy room, whose id must stay
    // the fold value its drawers already carry.
    let (_d, kg) = open_kg();
    let legacy = RoomType::Custom("work".to_string());
    let drawer = seed_legacy_room(&kg, &legacy);
    backfill_rooms(&kg, std::slice::from_ref(&drawer)).expect("room backfill");

    let legacy_before = resolve_or_create_room_sync(&kg.store(), &legacy).expect("before");
    let fresh_before = resolve_or_create_room_sync(&kg.store(), &RoomType::Planning).expect("b2");

    ensure_default_wing(&kg).expect("seed");

    assert_eq!(
        resolve_or_create_room_sync(&kg.store(), &legacy).expect("after"),
        legacy_before,
        "a legacy room's id must not move when wings appear"
    );
    assert_eq!(legacy_before, room_to_uuid(&legacy), "still the fold value");
    assert_eq!(
        resolve_or_create_room_sync(&kg.store(), &RoomType::Planning).expect("a2"),
        fresh_before,
        "a wing-less room resolution is unchanged by seeding"
    );
}

#[test]
fn fail_open_seeding_creates_the_default_wing() {
    // The fail-open wrapper the registry hook calls. A read-only (snapshot)
    // palace returns early inside it and has nothing to write to; a writable
    // one seeds exactly one wing.
    let (_d, kg) = open_kg();
    ensure_default_wing_fail_open("empty-palace", &kg);
    assert_eq!(list_wings(&kg).expect("list").len(), 1);
}

// ── wing_create ──────────────────────────────────────────────────────────

#[test]
fn wing_create_is_idempotent() {
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (a, created_a) = resolve_or_create_wing_sync(&store, "engineer").expect("first");
    let (b, created_b) = resolve_or_create_wing_sync(&store, "Engineer").expect("second");
    assert_eq!(a, b, "case variants are one wing");
    assert!(created_a, "the first call created it");
    assert!(!created_b, "the second did not");
    assert_eq!(a, mint_wing_id(&canonical_wing_key("engineer")));
    assert_eq!(store.get_wing(a).unwrap().unwrap().label, "engineer");
    assert_eq!(store.list_wings().unwrap().len(), 1);
}

#[test]
fn wing_create_returns_the_default_wing() {
    // Creating "default" must return DEFAULT_WING_ID — the id every room row
    // already carries — not a freshly minted one. This is why seeding runs at
    // palace open, before any caller can reach `wing_create`.
    let (_d, kg) = open_kg();
    ensure_default_wing(&kg).expect("seed");
    let (id, created) = resolve_or_create_wing_sync(&kg.store(), "default").expect("create");
    assert_eq!(id, DEFAULT_WING_ID);
    assert!(!created);
    assert_ne!(id, mint_wing_id(&canonical_wing_key("default")));
}

#[test]
fn wing_create_rejects_a_blank_label() {
    let (_d, kg) = open_kg();
    for blank in ["", "   ", "\t"] {
        assert!(
            resolve_or_create_wing_sync(&kg.store(), blank).is_err(),
            "blank label {blank:?} must be rejected"
        );
    }
    assert!(kg.store().list_wings().unwrap().is_empty());
}

// ── wing_rename ──────────────────────────────────────────────────────────

#[test]
fn wing_rename_changes_no_room_or_drawer_rows() {
    // A wing rename cannot move a room: rooms reference a wing by ID, and the
    // id is unchanged. Proven byte-for-byte on both tables.
    let (_d, kg) = open_kg();
    let drawer = seed_legacy_room(&kg, &RoomType::Planning);
    backfill_rooms(&kg, std::slice::from_ref(&drawer)).expect("room backfill");
    ensure_default_wing(&kg).expect("seed");

    let drawers_before = kg.store().raw_drawer_rows().expect("drawers before");
    let rooms_before = kg.store().raw_room_rows().expect("rooms before");

    rename_wing_sync(&kg.store(), DEFAULT_WING_ID, "platform").expect("rename");

    assert_eq!(drawers_before, kg.store().raw_drawer_rows().unwrap());
    assert_eq!(rooms_before, kg.store().raw_room_rows().unwrap());
    // The room is still in the (now renamed) wing.
    assert!(
        rooms_in_wing(&kg, DEFAULT_WING_ID)
            .unwrap()
            .contains(&drawer.room_id)
    );
}

#[test]
fn reseeding_does_not_restamp_the_schema_version() {
    // `set_wing_schema_version` is an unconditional overwrite, so seeding must
    // never reach it on an already-seeded palace: restamping a bumped version
    // over rows nothing migrated would erase the only evidence they still need
    // migrating. Byte-level snapshot, because a marker rewritten to the SAME
    // version is still a write and a decoded compare would not see it.
    let (_d, kg) = open_kg();
    assert!(ensure_default_wing(&kg).expect("first"));
    let before = kg.store().raw_wing_rows().expect("snapshot before");
    assert!(
        before.iter().any(|(k, _)| k.iter().all(|b| *b == 0)),
        "the marker row must be present after the first seed"
    );

    assert!(!ensure_default_wing(&kg).expect("second"), "no second row");
    assert_eq!(
        before,
        kg.store().raw_wing_rows().expect("snapshot after"),
        "re-seeding must not write a single WINGS byte"
    );
}

#[test]
fn wing_rename_applies_every_effect_together() {
    // The three effects of a rename — row label, new key, retired old key —
    // are applied in ONE redb write transaction (see `rename_wing_in_place`),
    // so no crash can land some and not others. This pins the combined
    // postcondition; the atomicity itself is a property of the single
    // begin_write/commit pair in that method.
    let (_d, kg) = open_kg();
    let (id, _) = resolve_or_create_wing_sync(&kg.store(), "engineer").expect("create");
    rename_wing_sync(&kg.store(), id, "platform").expect("rename");

    assert_eq!(kg.store().get_wing(id).unwrap().unwrap().label, "platform");
    assert_eq!(
        kg.store()
            .lookup_wing_id(&canonical_wing_key("platform"))
            .unwrap(),
        Some(id),
        "the new label must resolve"
    );
    assert_eq!(
        kg.store()
            .lookup_wing_id(&canonical_wing_key("engineer"))
            .unwrap(),
        None,
        "the old key must be gone — a rename, not an alias"
    );
}

#[test]
fn wing_rename_retires_the_old_label() {
    let (_d, kg) = open_kg();
    let (id, _) = resolve_or_create_wing_sync(&kg.store(), "engineer").expect("create");
    rename_wing_sync(&kg.store(), id, "Platform").expect("rename");

    assert_eq!(resolve_wing_selector(&kg, "engineer").unwrap(), None);
    assert_eq!(resolve_wing_selector(&kg, "platform").unwrap(), Some(id));
    assert_eq!(
        kg.store().get_wing(id).unwrap().unwrap().label,
        "Platform",
        "the chosen capitalisation is kept"
    );
    // A rename is not a create: still exactly one wing.
    assert_eq!(kg.store().list_wings().unwrap().len(), 1);
}

#[test]
fn wing_rename_rejects_a_taken_label() {
    let (_d, kg) = open_kg();
    let (a, _) = resolve_or_create_wing_sync(&kg.store(), "engineer").expect("a");
    let (b, _) = resolve_or_create_wing_sync(&kg.store(), "pm").expect("b");
    let err = rename_wing_sync(&kg.store(), a, "PM").expect_err("must refuse");
    // `{:#}` walks the anyhow chain — the refusal is raised inside the storage
    // transaction and wrapped by the caller's context.
    let msg = format!("{err:#}");
    assert!(msg.contains("already used"), "{msg}");
    // Nothing moved.
    assert_eq!(resolve_wing_selector(&kg, "pm").unwrap(), Some(b));
    assert_eq!(resolve_wing_selector(&kg, "engineer").unwrap(), Some(a));
}

#[test]
fn wing_rename_to_the_same_label_is_harmless() {
    let (_d, kg) = open_kg();
    let (id, _) = resolve_or_create_wing_sync(&kg.store(), "engineer").expect("create");
    let s = rename_wing_sync(&kg.store(), id, "Engineer").expect("recase");
    assert_eq!(s.label, "Engineer");
    assert_eq!(
        resolve_wing_selector(&kg, "engineer").unwrap(),
        Some(id),
        "the key is unchanged, so it must not have been retired"
    );
}

#[test]
fn wing_rename_rejects_an_unknown_wing() {
    let (_d, kg) = open_kg();
    assert!(rename_wing_sync(&kg.store(), Uuid::from_u128(7), "x").is_err());
}

#[test]
fn wing_rename_rejecting_a_taken_label_writes_nothing() {
    // The uniqueness probe lives INSIDE the write transaction, so a rejected
    // rename must abort it whole — no half-applied row, no orphaned key. Byte
    // snapshot, because a row rewritten to the same value is still a write.
    let (_d, kg) = open_kg();
    let (a, _) = resolve_or_create_wing_sync(&kg.store(), "engineer").expect("a");
    resolve_or_create_wing_sync(&kg.store(), "pm").expect("b");
    let before = kg.store().raw_wing_rows().expect("snapshot before");

    assert!(rename_wing_sync(&kg.store(), a, "PM").is_err());

    assert_eq!(
        before,
        kg.store().raw_wing_rows().expect("snapshot after"),
        "a rejected rename must leave WINGS byte-identical"
    );
    assert_eq!(
        kg.store()
            .lookup_wing_id(&canonical_wing_key("engineer"))
            .unwrap(),
        Some(a),
        "the original label must still resolve after a rejected rename"
    );
}

// ── Listing and scoping ──────────────────────────────────────────────────

#[test]
fn wing_list_reports_seeded_and_created_wings() {
    let (_d, kg) = open_kg();
    ensure_default_wing(&kg).expect("seed");
    resolve_or_create_wing_sync(&kg.store(), "engineer").expect("create");
    let wings = list_wings(&kg).expect("list");
    assert_eq!(wings.len(), 2);
    let labels: Vec<&str> = wings.iter().map(|w| w.label.as_str()).collect();
    assert!(labels.contains(&"default"));
    assert!(labels.contains(&"engineer"));
    assert_eq!(wings.iter().filter(|w| w.is_default).count(), 1);
}

#[test]
fn wing_list_counts_rooms_per_wing() {
    let (_d, kg) = open_kg();
    ensure_default_wing(&kg).expect("seed");
    // Two rooms in the default wing…
    resolve_or_create_room_sync(&kg.store(), &RoomType::Planning).expect("r1");
    resolve_or_create_room_sync(&kg.store(), &RoomType::Research).expect("r2");
    // …and a wing with none.
    resolve_or_create_wing_sync(&kg.store(), "engineer").expect("wing");

    let wings = list_wings(&kg).expect("list");
    let default = wings.iter().find(|w| w.is_default).expect("default wing");
    let engineer = wings.iter().find(|w| w.label == "engineer").expect("wing");
    assert_eq!(default.room_count, 2);
    assert_eq!(engineer.room_count, 0);
}

#[test]
fn wing_selector_accepts_id_or_label() {
    let (_d, kg) = open_kg();
    let (id, _) = resolve_or_create_wing_sync(&kg.store(), "engineer").expect("create");
    assert_eq!(
        resolve_wing_selector(&kg, &id.to_string()).unwrap(),
        Some(id)
    );
    assert_eq!(resolve_wing_selector(&kg, "ENGINEER").unwrap(), Some(id));
    assert_eq!(resolve_wing_selector(&kg, "nope").unwrap(), None);
    // A well-formed UUID with no row is "no such wing", not an error.
    assert_eq!(
        resolve_wing_selector(&kg, &Uuid::from_u128(3).to_string()).unwrap(),
        None
    );
}

#[test]
fn rooms_in_wing_separates_same_named_rooms() {
    // ADR-0027 D2 pattern 3: `engineer/Planning` and `pm/Planning` are two
    // distinct rooms, with no `Custom("engineer-planning")` name mangling.
    let (_d, kg) = open_kg();
    let store = kg.store();
    let (engineer, _) = resolve_or_create_wing_sync(&store, "engineer").expect("w1");
    let (pm, _) = resolve_or_create_wing_sync(&store, "pm").expect("w2");

    let key_e = crate::memory_core::room_identity::canonical_room_key(engineer, "Planning");
    let key_p = crate::memory_core::room_identity::canonical_room_key(pm, "Planning");
    assert_ne!(key_e, key_p, "the wing id is part of the room key");

    let id_e = crate::memory_core::room_identity::mint_room_id(&key_e);
    let id_p = crate::memory_core::room_identity::mint_room_id(&key_p);
    assert_ne!(id_e, id_p, "same label, different wings, different rooms");

    let mut rec_e = RoomRecord::new(&RoomType::Planning, 1, true);
    rec_e.wing_id = *engineer.as_bytes();
    let mut rec_p = RoomRecord::new(&RoomType::Planning, 1, true);
    rec_p.wing_id = *pm.as_bytes();
    store.insert_room_if_absent(id_e, &key_e, &rec_e).unwrap();
    store.insert_room_if_absent(id_p, &key_p, &rec_p).unwrap();

    assert_eq!(rooms_in_wing(&kg, engineer).unwrap(), HashSet::from([id_e]));
    assert_eq!(rooms_in_wing(&kg, pm).unwrap(), HashSet::from([id_p]));
}

#[test]
fn rooms_in_wing_is_empty_for_an_unknown_wing() {
    let (_d, kg) = open_kg();
    ensure_default_wing(&kg).expect("seed");
    resolve_or_create_room_sync(&kg.store(), &RoomType::Planning).expect("room");
    assert!(
        rooms_in_wing(&kg, Uuid::from_u128(99)).unwrap().is_empty(),
        "an unknown wing owns nothing — it must not fall back to everything"
    );
}
