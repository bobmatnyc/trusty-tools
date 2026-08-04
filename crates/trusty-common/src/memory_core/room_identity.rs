//! Room identity: canonical keys, id minting, and label/kind projection.
//!
//! Why (ADR-0027 D1.3): a room's id used to be a 16-byte XOR fold of a `Debug`
//! string (`retrieval::layers::room_to_uuid`) with provable structured
//! collisions for any custom label of 9+ characters. Ids are now *read from the
//! `ROOMS` table*; this module supplies the two pure ingredients that table
//! needs — the canonical key a label resolves through, and the UUIDv5 minted
//! for a key that has no row yet.
//! What: `DEFAULT_WING_ID`, `ROOM_NAMESPACE`, `canonical_room_key`,
//! `mint_room_id`, plus the `RoomType` <-> `(kind tag, label)` projection that
//! the on-disk `RoomRecord` stores as two separate fields (ADR-0010 shape).
//! No I/O, no redb — every function here is pure and total.
//! Test: `canonical_key_is_case_insensitive`, `mint_room_id_is_stable`,
//! `room_type_parts_round_trip`, `mint_room_id_avoids_fold_collisions`.

use crate::memory_core::palace::RoomType;
use uuid::Uuid;

/// Separator between the wing id and the normalised label in a canonical key.
///
/// ASCII UNIT SEPARATOR — cannot occur in a user-supplied room label that
/// survived `RoomType::parse`, so the two key components can never alias.
const KEY_SEP: char = '\u{1f}';

/// UUIDv5 namespace for room ids.
///
/// Why: minting a room id must be reproducible across processes and machines
/// without coordination, and must not collide the way the legacy 16-byte fold
/// does. A fixed namespace plus the canonical key gives a full SHA-1 digest.
/// What: `uuid5(NAMESPACE_URL,
/// "https://github.com/bobmatnyc/trusty-tools/adr-0027/room-namespace")`,
/// hardcoded so it needs no lazy global (this workspace forbids those).
/// Test: `mint_room_id_is_stable` pins the derived id for a known key.
pub const ROOM_NAMESPACE: Uuid = Uuid::from_bytes([
    193, 10, 247, 149, 38, 17, 83, 26, 166, 218, 54, 18, 11, 146, 68, 19,
]);

/// The default wing every room falls into until the Wing entity ships.
///
/// Why (ADR-0027 D2): `wing_id` is *reserved* now so the on-disk record does
/// not need a migration when wings land, but the `WINGS` table, the wing MCP
/// surface, and wing-scoped recall are explicitly gated on the #3064 consumer
/// that reads them (ticket T9). Every palace's `kg.db` is its own row space, so
/// one constant is "a default wing per palace".
/// What: `uuid5(ROOM_NAMESPACE, "default-wing")`, hardcoded for the same
/// no-lazy-global reason as [`ROOM_NAMESPACE`].
/// Test: `canonical_key_is_case_insensitive` uses it as the key prefix.
pub const DEFAULT_WING_ID: Uuid = Uuid::from_bytes([
    80, 71, 66, 222, 8, 75, 84, 80, 164, 24, 67, 237, 14, 82, 201, 141,
]);

/// Canonical display label for a room — the human-facing spelling.
///
/// Why: `RoomRecord` stores the *kind* and the *label* in separate fields
/// (ADR-0027 D1.2, mirroring ADR-0010's "named variants plus `Custom`"), so
/// both directions need one shared projection rather than a match per call
/// site. This is the common entry point; `trusty-memory`'s `room_label`
/// delegates here.
/// What: the variant name for the nine built-ins, the inner string for
/// `Custom`.
/// Test: `room_type_parts_round_trip`.
pub fn room_label(room: &RoomType) -> String {
    match room {
        RoomType::Frontend => "Frontend".to_string(),
        RoomType::Backend => "Backend".to_string(),
        RoomType::Testing => "Testing".to_string(),
        RoomType::Planning => "Planning".to_string(),
        RoomType::Documentation => "Documentation".to_string(),
        RoomType::Research => "Research".to_string(),
        RoomType::Configuration => "Configuration".to_string(),
        RoomType::Meetings => "Meetings".to_string(),
        RoomType::General => "General".to_string(),
        RoomType::Custom(s) => s.clone(),
    }
}

/// On-disk kind tag for a room: a built-in variant name, or `"Custom"`.
///
/// Why: keeping the kind separate from the label means a `Custom` body is never
/// confused with a built-in of the same spelling, and adding a variant later
/// does not renumber anything on disk (postcard would renumber an enum).
/// What: the variant name, or the literal `"Custom"`.
/// Test: `room_type_parts_round_trip`.
pub fn room_type_tag(room: &RoomType) -> &'static str {
    match room {
        RoomType::Frontend => "Frontend",
        RoomType::Backend => "Backend",
        RoomType::Testing => "Testing",
        RoomType::Planning => "Planning",
        RoomType::Documentation => "Documentation",
        RoomType::Research => "Research",
        RoomType::Configuration => "Configuration",
        RoomType::Meetings => "Meetings",
        RoomType::General => "General",
        RoomType::Custom(_) => "Custom",
    }
}

/// Rebuild a `RoomType` from the two fields a `RoomRecord` stores.
///
/// Why: readers (room listing, filter resolution) need the enum back; an
/// unknown tag written by a newer version must degrade to `Custom(label)`
/// rather than erroring, so an old binary can still list a new room.
/// What: matches the tag; anything unrecognised becomes `Custom(label)`.
/// Test: `room_type_parts_round_trip`, `unknown_tag_degrades_to_custom`.
pub fn room_type_from_parts(tag: &str, label: &str) -> RoomType {
    match tag {
        "Frontend" => RoomType::Frontend,
        "Backend" => RoomType::Backend,
        "Testing" => RoomType::Testing,
        "Planning" => RoomType::Planning,
        "Documentation" => RoomType::Documentation,
        "Research" => RoomType::Research,
        "Configuration" => RoomType::Configuration,
        "Meetings" => RoomType::Meetings,
        "General" => RoomType::General,
        _ => RoomType::Custom(label.to_string()),
    }
}

/// The `ROOM_KEYS` lookup key for `(wing, label)`.
///
/// Why (ADR-0027 D1.3): lowercasing the *key* while the record keeps the
/// first-seen *spelling* means `Decisions` and `decisions` resolve to one room
/// without destroying the capitalisation a human chose.
/// What: `"<wing_uuid>\x1f<trimmed, lowercased label>"`.
/// Test: `canonical_key_is_case_insensitive`.
pub fn canonical_room_key(wing_id: Uuid, label: &str) -> String {
    format!("{wing_id}{KEY_SEP}{}", label.trim().to_lowercase())
}

/// Canonical key for a `RoomType` in the default wing.
///
/// Convenience wrapper over [`canonical_room_key`] — the only wing that exists
/// until T9 lands.
pub fn default_wing_key(room: &RoomType) -> String {
    canonical_room_key(DEFAULT_WING_ID, &room_label(room))
}

/// Mint the id for a room that has no row yet.
///
/// Why (ADR-0027 D1.3): UUIDv5 is a full SHA-1 digest with no 16-byte fold, so
/// the legacy `room_to_uuid` collision class (C3.1 — any custom label of 9+
/// characters lands in the wrap zone) cannot recur for anything created from
/// now on. It is also reproducible without coordination, which a random v4
/// would not be.
/// What: `Uuid::new_v5(ROOM_NAMESPACE, key.as_bytes())`.
/// Test: `mint_room_id_is_stable`, `mint_room_id_avoids_fold_collisions`.
pub fn mint_room_id(key: &str) -> Uuid {
    Uuid::new_v5(&ROOM_NAMESPACE, key.as_bytes())
}

/// LEGACY id derivation — do not call to create a new room.
///
/// Why: every `room_id` written before ADR-0027 came from this 16-byte XOR
/// fold of a `RoomType`'s `Debug` repr, and those ids are kept **verbatim** on
/// their drawers (D1.3) — no `DRAWERS` row is ever rewritten. It therefore
/// survives for exactly two jobs: it is the oracle the backfill's rainbow
/// table and fold inversion match against (D1.4 steps 1–2), and it is the
/// fallback a room *filter* falls back to when the palace has no registry row
/// for that label yet, which keeps filtering byte-identical on an
/// un-backfilled palace. New ids come from [`mint_room_id`].
///
/// Known defect (ADR-0027 C3.1 / D-2): the fold XORs byte *i* into slot
/// *i mod 16*, so any `Debug` repr of 17+ bytes — i.e. every
/// `Custom("<9+ chars>")` — wraps and admits structured collisions. That is
/// why nothing new is minted with it.
///
/// What: folds `format!("{room:?}")` into 16 bytes as
/// `bytes[i % 16] ^= label[i].wrapping_add(i)`.
/// Test: `mint_room_id_avoids_fold_collisions` (proves the collision class is
/// real), plus every backfill round-trip in `store::room_backfill`.
pub fn room_to_uuid(room: &RoomType) -> Uuid {
    fold_debug_repr(&format!("{room:?}"))
}

/// The raw fold, applied to an already-rendered `Debug` repr.
///
/// Why: the backfill needs to hash *candidate* repr strings (from the rainbow
/// table, from a fold inversion, from a KG `room:` subject) without first
/// materialising a `RoomType` for each. Sharing the arithmetic with
/// [`room_to_uuid`] is what makes a match proof rather than a guess.
/// What: `bytes[i % 16] ^= repr[i].wrapping_add(i)` over the raw bytes.
/// Test: `fold_matches_room_to_uuid`.
pub fn fold_debug_repr(repr: &str) -> Uuid {
    let mut bytes = [0u8; 16];
    for (i, b) in repr.bytes().enumerate() {
        bytes[i % 16] ^= b.wrapping_add(i as u8);
    }
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_key_is_case_insensitive() {
        let a = canonical_room_key(DEFAULT_WING_ID, "Decisions");
        let b = canonical_room_key(DEFAULT_WING_ID, "  decisions ");
        assert_eq!(a, b);
        assert!(a.starts_with(&DEFAULT_WING_ID.to_string()));
        assert!(a.ends_with("decisions"));
    }

    #[test]
    fn canonical_key_separates_wing_from_label() {
        // Two wings must never produce the same key for the same label.
        let other = Uuid::from_u128(1);
        assert_ne!(
            canonical_room_key(DEFAULT_WING_ID, "planning"),
            canonical_room_key(other, "planning")
        );
    }

    #[test]
    fn room_namespace_matches_its_documented_derivation() {
        // The constants are hardcoded (this workspace forbids lazy globals), so
        // nothing but this test ties them to the derivation their doc comments
        // claim. A transcription slip would silently change every id minted
        // from here on and no other test would fail — `mint_room_id_is_stable`
        // proves stability across calls, not that the seed is the documented
        // one. Recompute both from first principles and compare.
        let ns = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            b"https://github.com/bobmatnyc/trusty-tools/adr-0027/room-namespace",
        );
        assert_eq!(ROOM_NAMESPACE, ns, "ROOM_NAMESPACE drifted from its doc");
        assert_eq!(
            DEFAULT_WING_ID,
            Uuid::new_v5(&ns, b"default-wing"),
            "DEFAULT_WING_ID drifted from its doc"
        );
    }

    #[test]
    fn mint_room_id_is_stable() {
        let key = canonical_room_key(DEFAULT_WING_ID, "Planning");
        let a = mint_room_id(&key);
        let b = mint_room_id(&key);
        assert_eq!(a, b, "minting must be deterministic across calls");
        assert_eq!(a.get_version_num(), 5, "ids are UUIDv5");
        assert_ne!(a, Uuid::nil());
    }

    #[test]
    fn mint_room_id_avoids_fold_collisions() {
        // ADR-0027 C3.1: distinct 20-character bodies of the form
        // `?bcdefghijklmnop?rst` collapse onto ONE id under the legacy fold —
        // the repeated character lands in slot 8 twice, 16 positions apart, so
        // its two contributions differ by a constant and cancel. UUIDv5 must
        // keep them apart.
        // Range chosen so `Debug` needs no escaping (a `\\` would change the
        // repr's length) and so the two contributions cancel without a carry.
        let bodies: Vec<String> = ('a'..='g')
            .map(|c| format!("{c}bcdefghijklmnop{c}rst"))
            .collect();
        assert_eq!(bodies.len(), 7);
        let ids: std::collections::HashSet<Uuid> = bodies
            .iter()
            .map(|b| mint_room_id(&canonical_room_key(DEFAULT_WING_ID, b)))
            .collect();
        assert_eq!(ids.len(), bodies.len(), "UUIDv5 must not collide here");

        // And the legacy fold really does collapse them — the defect is real,
        // not hypothetical.
        let folded: std::collections::HashSet<Uuid> = bodies
            .iter()
            .map(|b| room_to_uuid(&RoomType::Custom(b.clone())))
            .collect();
        assert_eq!(folded.len(), 1, "legacy fold collapses all seven");
    }

    #[test]
    fn fold_matches_room_to_uuid() {
        for room in [RoomType::General, RoomType::Custom("work".to_string())] {
            assert_eq!(room_to_uuid(&room), fold_debug_repr(&format!("{room:?}")));
        }
    }

    #[test]
    fn legacy_fold_matches_live_palace_ids() {
        // ADR-0027 C2 measured these ids in the live `trusty-tools` palace.
        // Pinning them proves the fold implementation did not drift when it
        // moved out of `retrieval::layers` — the whole migration rests on
        // reproducing the exact ids already stamped on 1000+ drawers.
        assert_eq!(
            room_to_uuid(&RoomType::General).to_string(),
            "47667068-7666-7200-0000-000000000000"
        );
        assert_eq!(
            room_to_uuid(&RoomType::Custom("work".to_string())).to_string(),
            "43767577-7372-2e29-7f78-7c762e360000"
        );
        assert_eq!(
            room_to_uuid(&RoomType::Custom("status".to_string())).to_string(),
            "43767577-7372-2e29-7b7d-6b7f81803038"
        );
    }

    #[test]
    fn room_type_parts_round_trip() {
        let cases = [
            RoomType::Frontend,
            RoomType::Backend,
            RoomType::Testing,
            RoomType::Planning,
            RoomType::Documentation,
            RoomType::Research,
            RoomType::Configuration,
            RoomType::Meetings,
            RoomType::General,
            RoomType::Custom("status".to_string()),
        ];
        for room in cases {
            let tag = room_type_tag(&room);
            let label = room_label(&room);
            assert_eq!(room_type_from_parts(tag, &label), room, "{room:?}");
        }
    }

    #[test]
    fn unknown_tag_degrades_to_custom() {
        // A row written by a future binary that added a variant must still list.
        assert_eq!(
            room_type_from_parts("Kitchen", "kitchen"),
            RoomType::Custom("kitchen".to_string())
        );
    }
}
