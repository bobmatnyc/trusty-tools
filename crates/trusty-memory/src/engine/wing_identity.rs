//! Wing identity: canonical keys and id minting for the *scope* axis.
//!
//! Why (ADR-0027 D2): a Wing is the "who" axis (scope / ownership) and a Room
//! is the "what" axis (topic). Separating them is what lets `engineer/Planning`
//! and `pm/Planning` coexist without the name mangling that gave the live
//! `trusty-tools` palace twelve ad-hoc labels. This module supplies the two
//! pure ingredients the `WINGS` table needs — the canonical key a wing label
//! resolves through, and the UUIDv5 minted for a key that has no row yet.
//! What: `WING_NAMESPACE`, `DEFAULT_WING_LABEL`, `canonical_wing_key`,
//! `default_wing_key`, `mint_wing_id`. Deliberately mirrors
//! [`crate::memory_core::room_identity`] so the two axes have one shape, not
//! two. No I/O, no redb — every function here is pure and total.
//!
//! [`DEFAULT_WING_ID`] itself lives in `room_identity` because `RoomRecord`
//! reserved it first (ADR-0027 D1.2); it is re-exported here so wing-side call
//! sites have one import.
//!
//! Test: `wing_namespace_matches_its_documented_derivation`,
//! `canonical_wing_key_is_case_insensitive`, `mint_wing_id_is_stable`,
//! `default_wing_id_is_not_the_minted_id`.

use uuid::Uuid;

pub use crate::memory_core::room_identity::DEFAULT_WING_ID;

/// UUIDv5 namespace for wing ids.
///
/// Why: minting a wing id must be reproducible across processes and machines
/// without coordination. A namespace distinct from `ROOM_NAMESPACE` means a
/// wing and a room that happen to share a label can never mint the same id,
/// so a caller cannot accidentally pass one where the other is expected.
/// What: `uuid5(NAMESPACE_URL,
/// "https://github.com/bobmatnyc/trusty-tools/adr-0027/wing-namespace")`,
/// hardcoded so it needs no lazy global (this workspace forbids those).
/// Test: `wing_namespace_matches_its_documented_derivation`.
pub const WING_NAMESPACE: Uuid = Uuid::from_bytes([
    112, 61, 198, 9, 153, 103, 81, 110, 178, 2, 134, 208, 67, 211, 226, 182,
]);

/// Display label of the wing every room falls into when nobody names one.
///
/// Why (ADR-0027 D2): "Wing is never a required concept for a caller." Every
/// palace gets this wing, every room defaults into it, and a caller who never
/// heard of wings sees identical behaviour before and after T9.
pub const DEFAULT_WING_LABEL: &str = "default";

/// The `WING_KEYS` lookup key for `label`.
///
/// Why: same case-folding rule as rooms (ADR-0027 D1.3) — the *key* is
/// lowercased while the record keeps the first-seen *spelling*, so `Engineer`
/// and `engineer` are one wing without destroying the capitalisation a human
/// chose. Unlike a room key there is no parent id to prefix: a wing is
/// top-level within a palace, and `WING_KEYS` is its own table, so no two
/// namespaces can alias.
/// What: the trimmed, lowercased label.
/// Test: `canonical_wing_key_is_case_insensitive`.
pub fn canonical_wing_key(label: &str) -> String {
    label.trim().to_lowercase()
}

/// Canonical key of the default wing.
///
/// Test: `default_wing_id_is_not_the_minted_id`.
pub fn default_wing_key() -> String {
    canonical_wing_key(DEFAULT_WING_LABEL)
}

/// Mint the id for a wing that has no row yet.
///
/// Why: UUIDv5 over a canonical key is reproducible without coordination and
/// carries no fold, so the legacy `room_to_uuid` collision class (ADR-0027
/// C3.1) has no wing-side analogue. A random v4 would not be reproducible.
/// What: `Uuid::new_v5(WING_NAMESPACE, key.as_bytes())`.
///
/// Note that the DEFAULT wing is **not** minted with this — its id is the
/// pre-existing [`DEFAULT_WING_ID`] constant that every `RoomRecord` written
/// since ADR-0027 T1 already carries, and it is seeded verbatim exactly the
/// way a legacy room id is (ADR-0027 D1.3, "ids are read from the table,
/// never recomputed"). `default_wing_id_is_not_the_minted_id` pins that the
/// two genuinely differ, so the seeding is load-bearing rather than
/// incidental.
/// Test: `mint_wing_id_is_stable`, `default_wing_id_is_not_the_minted_id`.
pub fn mint_wing_id(key: &str) -> Uuid {
    Uuid::new_v5(&WING_NAMESPACE, key.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wing_namespace_matches_its_documented_derivation() {
        // The constant is hardcoded (this workspace forbids lazy globals), so
        // nothing but this test ties it to the derivation its doc claims. A
        // transcription slip would silently change every wing id minted from
        // here on and no other test would fail — `mint_wing_id_is_stable`
        // proves stability across calls, not that the seed is the documented
        // one.
        assert_eq!(
            WING_NAMESPACE,
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                b"https://github.com/bobmatnyc/trusty-tools/adr-0027/wing-namespace",
            ),
            "WING_NAMESPACE drifted from its doc"
        );
    }

    #[test]
    fn wing_namespace_differs_from_room_namespace() {
        assert_ne!(
            WING_NAMESPACE,
            crate::memory_core::room_identity::ROOM_NAMESPACE,
            "a wing and a room sharing a label must never mint the same id"
        );
    }

    #[test]
    fn canonical_wing_key_is_case_insensitive() {
        assert_eq!(
            canonical_wing_key("Engineer"),
            canonical_wing_key(" engineer ")
        );
        assert_eq!(canonical_wing_key("Engineer"), "engineer");
    }

    #[test]
    fn mint_wing_id_is_stable() {
        let key = canonical_wing_key("engineer");
        assert_eq!(mint_wing_id(&key), mint_wing_id(&key));
        assert_eq!(mint_wing_id(&key).get_version_num(), 5, "ids are UUIDv5");
        assert_ne!(mint_wing_id(&key), Uuid::nil());
        assert_ne!(mint_wing_id(&key), mint_wing_id(&canonical_wing_key("pm")));
    }

    #[test]
    fn default_wing_id_is_not_the_minted_id() {
        // If these ever coincided, seeding the default row verbatim would be a
        // no-op and this test would stop proving anything — so pin the
        // difference. Every room row written since T1 carries DEFAULT_WING_ID,
        // so that is the id the default wing MUST be stored under.
        assert_eq!(default_wing_key(), "default");
        assert_ne!(
            mint_wing_id(&default_wing_key()),
            DEFAULT_WING_ID,
            "the default wing is seeded verbatim, never minted"
        );
    }
}
