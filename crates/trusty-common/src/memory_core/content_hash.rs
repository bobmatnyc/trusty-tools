//! Content-addressed identity for a memory body — the ONE hashing entry point
//! `memory_core` owns (#5902).
//!
//! Why: the same fact written on two machines for one project must converge to
//! one memory once it is exported, committed to git, pulled, and imported
//! elsewhere. `Drawer.id` is a v4 UUID minted at write time, so nothing about it
//! derives from what the drawer says and two independent writes can never be
//! recognised as the same fact. A digest over the body is the only identity that
//! two machines can compute without talking to each other.
//!
//! Why this module rather than a fourth ad-hoc `Sha256::new()`: this crate
//! already has four independent sha256 call sites
//! (`error_capture::fingerprint`, `memory_core::semantic_consolidation::types`,
//! `symgraph::registry`, `symgraph::contracts`), none of which is a shared entry
//! point. Adding a fifth would put the normalization contract below in a place
//! no other caller can find. CLAUDE.md's common-entry-point rule applies: the
//! capability lands once.
//!
//! Why NOT `symgraph::SymbolRegistry::content_hash`: that is code-symbol
//! identity — a raw sha256 over source text with no normalization, whose whole
//! purpose is to change when the source bytes change, including on a re-indent
//! or a line-ending flip. Memory identity needs the opposite: two clients that
//! typed the same sentence must agree even when their editors disagree about
//! trailing newlines. Same primitive, opposite contract, so they must not share
//! a function. The domain separator in [`memory_content_hash`] makes the two
//! digest spaces provably disjoint rather than merely conventionally distinct.
//!
//! What: [`normalize_for_hash`] (the stable, versioned normalization contract),
//! [`ContentHash`] (a 32-byte digest that prints and parses as lowercase hex),
//! and [`memory_content_hash`] (the entry point).
//! Test: `normalize_*` and `hash_*` in this module's `tests`; the cross-machine
//! properties live in `memory_core::share::tests`.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Version of the normalization + digest contract in this module.
///
/// Why: the digest IS the identity of a memory, so the normalization below is a
/// wire contract, not an implementation detail. Changing any rule re-mints every
/// id in every exported file in every repo that ever ran this code, and two
/// clients on different versions would stop converging — the exact failure this
/// module exists to prevent. Bumping this constant is therefore a BREAKING
/// change, and the version is folded into the digest preimage so a v2 hash can
/// never be mistaken for a v1 hash of different text.
/// What: `1`. Read by [`memory_content_hash`] as part of the domain separator.
/// Test: `domain_separator_pins_the_version`.
pub const CONTENT_HASH_VERSION: u32 = 1;

/// Domain separator prefix. The version is appended by [`memory_content_hash`].
const DOMAIN_PREFIX: &str = "trusty-memory/content-hash/v";

/// A memory body's content-addressed identity: the SHA-256 of its normalized
/// text under [`CONTENT_HASH_VERSION`].
///
/// Why: a `[u8; 32]` newtype rather than a `String` because every `Drawer`
/// carries one and the drawer table is cloned wholesale on every `list_drawers`
/// and every L1 refresh — a hex `String` would add an allocation per drawer per
/// clone for no gain. `Copy` keeps call sites free of borrow noise.
/// What: serializes and deserializes as a 64-character lowercase hex string, so
/// the JSONL export format is human-readable and greppable. [`Self::UNSET`] is
/// the all-zero sentinel a legacy record with no hash field decodes to; it is
/// never a real digest (SHA-256 of anything is all-zero with probability 2^-256)
/// and [`Self::is_unset`] is what the hydration paths branch on.
/// Test: `hash_hex_round_trips`, `unset_is_distinguishable_from_a_real_digest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// The all-zero sentinel meaning "no digest recorded".
    pub const UNSET: Self = Self([0u8; 32]);

    /// Whether this is the [`Self::UNSET`] sentinel rather than a real digest.
    pub fn is_unset(&self) -> bool {
        self.0 == [0u8; 32]
    }

    /// The raw 32 digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, 64 characters.
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-character lowercase-or-uppercase hex digest.
    ///
    /// Why: import reads the digest a producer wrote, so the parse has to reject
    /// anything that is not exactly 32 bytes rather than silently zero-padding —
    /// a short digest that parsed would compare unequal to every real one and
    /// turn every import into an insert.
    /// What: `Err` on any length other than 64 hex characters, or any non-hex
    /// character.
    /// Test: `hash_hex_round_trips`, `parse_rejects_a_short_or_non_hex_digest`.
    pub fn from_hex(s: &str) -> Result<Self, ContentHashParseError> {
        let bytes = hex::decode(s).map_err(|_| ContentHashParseError {
            got: s.chars().take(72).collect(),
        })?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| ContentHashParseError {
            got: s.chars().take(72).collect(),
        })?;
        Ok(Self(arr))
    }
}

/// A digest string that is not 32 bytes of hex.
#[derive(Debug, Clone, thiserror::Error)]
#[error("not a 64-character hex SHA-256 digest: {got:?}")]
pub struct ContentHashParseError {
    /// The offending input, truncated for the message.
    pub got: String,
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Default for ContentHash {
    fn default() -> Self {
        Self::UNSET
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).map_err(de::Error::custom)
    }
}

/// Normalize `content` for hashing. STABLE CONTRACT — see
/// [`CONTENT_HASH_VERSION`] before changing a single rule.
///
/// Why: nothing normalizes memory text today — `PalaceHandle::remember_with_options`
/// stores `content: String` exactly as received, with no trim, no Unicode form,
/// and no line-ending policy. Two clients that recorded the same sentence
/// therefore hold bytes that differ in ways no reader would call a difference: a
/// Windows editor's `\r\n`, one heredoc's trailing blank line, a macOS
/// filesystem's decomposed `é`. Hashing raw bytes would fork identity on every
/// one of those, which defeats convergence entirely.
///
/// What, in order:
/// 1. Line endings: `\r\n` and lone `\r` both become `\n`.
/// 2. Unicode: NFC (canonical composition), so `e` + U+0301 and U+00E9 agree.
/// 3. Per line: trailing whitespace removed (`str::trim_end`).
/// 4. Whole string: all trailing newlines and whitespace removed, so zero, one,
///    and five trailing blank lines are one identity.
///
/// What it deliberately does NOT do: leading whitespace is PRESERVED, per line
/// and for the string as a whole. Indentation carries meaning in a memory that
/// holds a code block or a nested list, and a rule that collapsed it would merge
/// two facts that a reader can tell apart. Interior blank lines are preserved for
/// the same reason. Case is preserved — this is not a fuzzy-match key.
///
/// Normalization is for HASHING ONLY. Nothing here touches what gets stored;
/// `Drawer.content` keeps the caller's bytes verbatim. Rewriting stored content
/// would be a silent data migration of every palace on disk.
/// Test: `normalize_collapses_line_endings`, `normalize_trims_trailing_space_per_line`,
/// `normalize_collapses_trailing_newlines`, `normalize_applies_nfc`,
/// `normalize_preserves_leading_whitespace_and_interior_blanks`.
pub fn normalize_for_hash(content: &str) -> String {
    // Step 1: line endings. Done before NFC so the `\r` removal cannot be
    // perturbed by a composition that spans the boundary.
    let unix: String = content.replace("\r\n", "\n").replace('\r', "\n");
    // Step 2: NFC over the whole string.
    let composed: String = unix.nfc().collect();
    // Step 3: per-line trailing whitespace.
    let mut out = String::with_capacity(composed.len());
    for (i, line) in composed.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    // Step 4: trailing newlines / whitespace for the string as a whole.
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    out
}

/// The content-addressed identity of a memory body (#5902).
///
/// Why: this is the join key that makes two machines' copies of one fact the
/// same memory. Decision: the digest covers the BODY ONLY — not tags, not
/// `room_id`, not `drawer_type`, not importance. The dream cycle rewrites both
/// content and tags during routine housekeeping
/// (`dream::helpers::merge_into` appends the loser's text and unions its tags;
/// `dream::cycle::apply_consolidation_result` writes an LLM-authored canonical
/// body with a rewritten tag set), so a metadata-inclusive digest would fork
/// identity on a consolidation pass that changed nothing a reader would call a
/// new fact.
///
/// What: `SHA-256("trusty-memory/content-hash/v<VERSION>" || 0x00 ||
/// normalize_for_hash(content))`. The domain separator makes this digest space
/// disjoint from every other sha256 in the workspace — notably
/// `symgraph::SymbolRegistry::content_hash`, which is a bare digest of raw
/// source — so no code-symbol digest can ever be mistaken for a memory id, and a
/// future [`CONTENT_HASH_VERSION`] bump lands in a different space rather than
/// silently overlapping v1.
/// Test: `hash_is_stable_for_a_known_body`, `hash_ignores_line_ending_and_trailing_newline`,
/// `hash_ignores_unicode_composition_form`, `hash_distinguishes_different_bodies`,
/// `hash_is_not_a_bare_sha256_of_the_body`.
pub fn memory_content_hash(content: &str) -> ContentHash {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_PREFIX.as_bytes());
    hasher.update(CONTENT_HASH_VERSION.to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(normalize_for_hash(content).as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    ContentHash(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_line_endings() {
        assert_eq!(normalize_for_hash("a\r\nb"), "a\nb");
        assert_eq!(normalize_for_hash("a\rb"), "a\nb");
        assert_eq!(normalize_for_hash("a\nb"), "a\nb");
    }

    #[test]
    fn normalize_trims_trailing_space_per_line() {
        assert_eq!(normalize_for_hash("a   \nb\t\nc"), "a\nb\nc");
    }

    #[test]
    fn normalize_collapses_trailing_newlines() {
        assert_eq!(normalize_for_hash("fact"), "fact");
        assert_eq!(normalize_for_hash("fact\n"), "fact");
        assert_eq!(normalize_for_hash("fact\n\n\n"), "fact");
        assert_eq!(normalize_for_hash("fact\n  \n\t\n"), "fact");
    }

    #[test]
    fn normalize_applies_nfc() {
        // U+0065 U+0301 (decomposed) vs U+00E9 (composed).
        let decomposed = "caf\u{0065}\u{0301}";
        let composed = "caf\u{00e9}";
        assert_ne!(decomposed, composed, "the inputs must differ as bytes");
        assert_eq!(normalize_for_hash(decomposed), normalize_for_hash(composed));
    }

    /// Why: the contract's negative half is as load-bearing as its positive
    /// half. A rule that collapsed indentation would merge a memory holding a
    /// nested code block with one holding the same text flush-left, which are
    /// different facts.
    /// Test: This test.
    #[test]
    fn normalize_preserves_leading_whitespace_and_interior_blanks() {
        assert_eq!(normalize_for_hash("    indented"), "    indented");
        assert_eq!(normalize_for_hash("a\n\nb"), "a\n\nb");
        assert_ne!(normalize_for_hash("  a"), normalize_for_hash("a"));
    }

    #[test]
    fn normalize_of_only_whitespace_is_empty() {
        assert_eq!(normalize_for_hash("   \n\t\r\n  "), "");
        assert_eq!(normalize_for_hash(""), "");
    }

    #[test]
    fn hash_hex_round_trips() {
        let h = memory_content_hash("a fact");
        let hex = h.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(ContentHash::from_hex(&hex).unwrap(), h);
        assert_eq!(h.to_string(), hex);
    }

    #[test]
    fn parse_rejects_a_short_or_non_hex_digest() {
        assert!(ContentHash::from_hex("deadbeef").is_err());
        assert!(ContentHash::from_hex("").is_err());
        assert!(ContentHash::from_hex(&"z".repeat(64)).is_err());
    }

    #[test]
    fn unset_is_distinguishable_from_a_real_digest() {
        assert!(ContentHash::UNSET.is_unset());
        assert!(ContentHash::default().is_unset());
        assert!(!memory_content_hash("").is_unset());
        assert!(!memory_content_hash("x").is_unset());
    }

    /// Why: the digest is a wire contract, so a pinned expected value is what
    /// catches an accidental change to the preimage — a reordered `update`, a
    /// dropped separator, a normalization tweak — that every other test in this
    /// module would still pass.
    /// Test: This test.
    #[test]
    fn hash_is_stable_for_a_known_body() {
        assert_eq!(
            memory_content_hash("the daemon binds loopback only").to_hex(),
            "5b00cdfb7e5932bd483cdb66a70fbc680693a056d6bc708ef3182cfdff0f31da"
        );
    }

    #[test]
    fn hash_ignores_line_ending_and_trailing_newline() {
        let base = memory_content_hash("line one\nline two");
        assert_eq!(memory_content_hash("line one\r\nline two"), base);
        assert_eq!(memory_content_hash("line one\nline two\n"), base);
        assert_eq!(memory_content_hash("line one\r\nline two\r\n\r\n"), base);
        assert_eq!(memory_content_hash("line one   \nline two\t"), base);
    }

    #[test]
    fn hash_ignores_unicode_composition_form() {
        assert_eq!(
            memory_content_hash("caf\u{0065}\u{0301} rules"),
            memory_content_hash("caf\u{00e9} rules")
        );
    }

    #[test]
    fn hash_distinguishes_different_bodies() {
        assert_ne!(memory_content_hash("a"), memory_content_hash("b"));
        // Case is significant — this is identity, not a fuzzy match key.
        assert_ne!(memory_content_hash("Fact"), memory_content_hash("fact"));
        // Leading indentation is significant.
        assert_ne!(memory_content_hash("  fact"), memory_content_hash("fact"));
    }

    /// Why: the domain separator is what keeps this digest space disjoint from
    /// every other sha256 of the same text in the workspace —
    /// `symgraph::SymbolRegistry::content_hash` is exactly the bare digest
    /// computed below. If the separator were ever dropped, the two spaces would
    /// silently merge and nothing else here would fail.
    /// Test: This test.
    #[test]
    fn hash_is_not_a_bare_sha256_of_the_body() {
        let bare = {
            let mut h = Sha256::new();
            h.update(b"a fact");
            hex::encode(h.finalize())
        };
        assert_ne!(memory_content_hash("a fact").to_hex(), bare);
    }

    /// Why: [`CONTENT_HASH_VERSION`] is folded into the preimage precisely so a
    /// future bump lands in a different digest space. A test that only asserted
    /// the constant's value would not catch it being dropped from the preimage.
    /// Test: This test.
    #[test]
    fn domain_separator_pins_the_version() {
        assert_eq!(CONTENT_HASH_VERSION, 1);
        let expected = {
            let mut h = Sha256::new();
            h.update(b"trusty-memory/content-hash/v1");
            h.update([0u8]);
            h.update(b"a fact");
            hex::encode(h.finalize())
        };
        assert_eq!(memory_content_hash("a fact").to_hex(), expected);
    }
}
