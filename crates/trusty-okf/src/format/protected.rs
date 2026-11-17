//! The protected key set at the ingest boundary (SPEC-OKF-01 §2.13.1).
//!
//! Why: a source file, a source adapter or the extraction worker must never
//! set a key that describes a concept's state rather than its claims. A
//! planted `verified`, `status` or verification-record hash would show a false
//! review to OKF readers, or a false lifecycle to trusty readers.
//! What: [`PROTECTED_KEYS`] is the one constant the ingest engine reads. It is
//! the §2.10 lifecycle set ([`LIFECYCLE_KEYS`]) followed by
//! [`VERIFICATION_RECORD_KEYS`] and [`SOURCE_FIELDS_KEY`], built at compile
//! time, so a change to the lifecycle set changes the protected set.
//! [`is_protected_key`] adds the `verified_` and `verification_` prefixes.
//! Test: `protected_set_holds_the_lifecycle_set`,
//! `protected_prefixes_cover_verification_keys`,
//! `c116_protected_keys_from_every_input_are_quarantined`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.13.1 (the protected set), §2.10 (lifecycle set).

use crate::format::hash::LIFECYCLE_KEYS;

/// The extension mapping that holds quarantined source values (§2.13.1).
/// It is itself protected, so no source can plant or overwrite it.
pub const SOURCE_FIELDS_KEY: &str = "source_fields";

/// Frontmatter keys that would carry verification-record state.
///
/// #10746: SPEC-OKF-01 names "verification-record keys" without fixing
/// their spelling (S3 writes no record key into frontmatter). These are the
/// record's own field names under a frontmatter-safe prefix, plus the two
/// invalidation fields; any other `verification_*` key is caught by
/// [`PROTECTED_PREFIXES`].
pub const VERIFICATION_RECORD_KEYS: [&str; 5] = [
    "verification",
    "verification_record",
    "verification_hash",
    "invalidated_at",
    "invalidated_by_hash",
];

/// Key prefixes that are protected whatever follows them (§2.13.1 names
/// `verified_`; #10746 adds `verification_` for the record keys).
pub const PROTECTED_PREFIXES: [&str; 2] = ["verified_", "verification_"];

const EXTRA_KEYS: [&str; VERIFICATION_RECORD_KEYS.len() + 1] = {
    let mut out = [SOURCE_FIELDS_KEY; VERIFICATION_RECORD_KEYS.len() + 1];
    let mut i = 0;
    while i < VERIFICATION_RECORD_KEYS.len() {
        out[i] = VERIFICATION_RECORD_KEYS[i];
        i += 1;
    }
    out
};

/// Every exactly-named protected key: the §2.10 lifecycle set, then the
/// verification-record keys, then [`SOURCE_FIELDS_KEY`].
pub const PROTECTED_KEYS: [&str; LIFECYCLE_KEYS.len() + EXTRA_KEYS.len()] = {
    let mut out = [""; LIFECYCLE_KEYS.len() + EXTRA_KEYS.len()];
    let mut i = 0;
    while i < LIFECYCLE_KEYS.len() {
        out[i] = LIFECYCLE_KEYS[i];
        i += 1;
    }
    let mut j = 0;
    while j < EXTRA_KEYS.len() {
        out[i + j] = EXTRA_KEYS[j];
        j += 1;
    }
    out
};

/// Whether no source value may set `key` at the top level (§2.13.1).
///
/// What: true for every [`PROTECTED_KEYS`] member and every key that starts
/// with a [`PROTECTED_PREFIXES`] member.
/// Test: `protected_set_holds_the_lifecycle_set`,
/// `protected_prefixes_cover_verification_keys`.
pub fn is_protected_key(key: &str) -> bool {
    PROTECTED_KEYS.contains(&key) || PROTECTED_PREFIXES.iter().any(|p| key.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: §2.13.1 — the lifecycle set is a subset of the protected set, so
    /// a key added to one is in the other.
    /// What: every lifecycle key, every record key and `source_fields` is
    /// protected; content keys are not.
    /// Test: self-contained.
    #[test]
    fn protected_set_holds_the_lifecycle_set() {
        for k in LIFECYCLE_KEYS.iter().chain(VERIFICATION_RECORD_KEYS.iter()) {
            assert!(is_protected_key(k), "{k} must be protected");
        }
        assert!(is_protected_key(SOURCE_FIELDS_KEY));
        for k in ["title", "description", "type", "tags", "sources", "from"] {
            assert!(!is_protected_key(k), "{k} is content");
        }
    }

    /// Why: §2.13.1 protects every `verified_` key, and a record key under
    /// any spelling must not slip through.
    /// What: prefixed keys are protected; a key that only contains the
    /// prefix later is not.
    /// Test: self-contained.
    #[test]
    fn protected_prefixes_cover_verification_keys() {
        for k in ["verified_by", "verified_hash", "verification_anything"] {
            assert!(is_protected_key(k), "{k}");
        }
        assert!(!is_protected_key("unverified_note"));
    }
}
