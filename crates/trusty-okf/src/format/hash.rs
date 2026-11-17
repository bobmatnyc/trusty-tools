//! The §2.10 content hash, the lifecycle key set and the byte hash
//! (SPEC-OKF-01 §2.10, §4.2).
//!
//! Why: a verification event attests one revision of a concept. trusty-okf
//! needs one deterministic name for "the claims this concept makes" so a
//! write can tell a content change, which clears verification, from a
//! lifecycle change, which keeps it.
//! What: [`content_hash`] is SHA-256 over the body (CRLF normalised to LF)
//! and the content frontmatter: every key outside [`LIFECYCLE_KEYS`],
//! key-sorted, with plain-string `sources` entries read as `{resource: …}`
//! (§2.8), so a migration never changes the hash. [`bytes_hash`] hashes raw
//! file bytes; the stale-write check compares it.
//! Test: `c32_description_change_clears_verification`,
//! `c33_status_or_stale_after_change_keeps_verification`,
//! `c34_migration_keeps_the_content_hash`,
//! `content_hash_ignores_lifecycle_keys_and_crlf`.

use std::fmt;

use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};

use crate::format::concept::Concept;
use crate::format::frontmatter::sort_value;

/// Keys that describe a concept's state, not its claims (§2.10). A change
/// to one of them alone is not a content change.
///
/// #10746: S3 writes no verification-record key into frontmatter. #10746
/// S10: the §2.16 input SHA-256 key is `extract_input_sha256`; like the
/// other extraction provenance keys it is not content (item 21 B).
pub const LIFECYCLE_KEYS: [&str; 22] = [
    "generated",
    "verified",
    "stale_after",
    "status",
    "supersedes",
    "superseded_by",
    "trust",
    "source_id",
    "source_kind",
    "source_item_id",
    "source_timestamp",
    "source_status",
    "tombstoned",
    "created",
    "updated",
    "legacy_verified",
    "legacy_generated",
    "extracted_by",
    "extract_protocol",
    "xberg_version",
    "extract_config",
    "extract_input_sha256",
];

/// A §2.10 content hash, `sha256:<hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(String);

impl ContentHash {
    /// Wrap a stored `sha256:<hex>` string. Returns `None` for any other
    /// shape.
    pub fn from_stored(s: &str) -> Option<ContentHash> {
        let hex = s.strip_prefix("sha256:")?;
        let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| ContentHash(s.to_string()))
    }

    /// The `sha256:<hex>` string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `key` is in the §2.10 lifecycle set.
pub fn is_lifecycle_key(key: &str) -> bool {
    LIFECYCLE_KEYS.contains(&key)
}

/// Lower-case hex of a SHA-256 digest.
fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `sha256:<hex>` of raw bytes.
pub fn bytes_hash(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

/// The content hash of a concept (SPEC-OKF-01 §4.3).
///
/// What: hashes a fixed version tag, the key-sorted YAML of the content
/// frontmatter and the LF-normalised body. A key that is not a string is
/// content. The render cannot fail for a parsed value; if it ever did, the
/// value's debug form is hashed instead, which is still deterministic.
/// Test: `content_hash_ignores_lifecycle_keys_and_crlf`,
/// `c34_migration_keeps_the_content_hash`.
pub fn content_hash(c: &Concept) -> ContentHash {
    let mut content = Mapping::new();
    for (k, v) in c.values() {
        if k.as_str().is_some_and(is_lifecycle_key) {
            continue;
        }
        let v = if k.as_str() == Some("sources") {
            normalized_sources(v)
        } else {
            v.clone()
        };
        content.insert(k.clone(), v);
    }
    let sorted = sort_value(&Value::Mapping(content));
    let yaml = serde_yaml::to_string(&sorted).unwrap_or_else(|_| format!("{sorted:?}"));
    let mut h = Sha256::new();
    h.update(b"okf-content-v1\n");
    h.update(yaml.as_bytes());
    h.update(b"---\n");
    h.update(c.body.replace("\r\n", "\n").as_bytes());
    ContentHash(format!("sha256:{}", hex(&h.finalize())))
}

/// `sources` with each plain-string entry read as `{resource: s}` (§2.8).
fn normalized_sources(v: &Value) -> Value {
    let Value::Sequence(seq) = v else {
        return v.clone();
    };
    Value::Sequence(
        seq.iter()
            .map(|e| match e {
                Value::String(s) => {
                    let mut m = Mapping::new();
                    m.insert("resource".into(), Value::String(s.clone()));
                    Value::Mapping(m)
                }
                other => other.clone(),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::concept::parse_concept;
    use crate::format::id::ConceptId;

    fn hash_of(text: &str) -> ContentHash {
        content_hash(
            &parse_concept(ConceptId::parse("a").unwrap(), text)
                .unwrap()
                .0,
        )
    }

    /// Why: only content keys and the body define a revision (§2.10).
    /// What: a lifecycle-only change and a CRLF body keep the hash; a
    /// description change and a body change do not.
    /// Test: self-contained.
    #[test]
    fn content_hash_ignores_lifecycle_keys_and_crlf() {
        let base = hash_of("---\ntype: A\ndescription: d\n---\nBody\n");
        assert_eq!(
            base,
            hash_of("---\ntype: A\nstatus: deprecated\ndescription: d\nverified: []\n---\nBody\n")
        );
        assert_eq!(
            base,
            hash_of("---\r\ntype: A\r\ndescription: d\r\n---\r\nBody\r\n")
        );
        assert_ne!(base, hash_of("---\ntype: A\ndescription: e\n---\nBody\n"));
        assert_ne!(base, hash_of("---\ntype: A\ndescription: d\n---\nBody!\n"));
        assert!(ContentHash::from_stored(base.as_str()).is_some());
        assert!(ContentHash::from_stored("sha256:xyz").is_none());
    }
}
