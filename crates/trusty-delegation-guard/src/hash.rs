//! Canonical hashing, hashed identifiers and the dedupe key (design §3).

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{DgError, Result};

/// A lowercase, 64-character sha256 hex digest.
///
/// Why: every hash field in `dg.event.v1` and `dg.verdict.v1` must reject a
/// non-digest at parse time, not at first comparison.
/// What: a `String` newtype whose `Deserialize` runs the shape check.
/// Test: `sha256_hex_rejects_non_digests`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Hex(String);

impl Sha256Hex {
    /// Hashes `bytes` and wraps the digest.
    pub fn digest(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    /// The digest as hex text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha256Hex {
    type Error = DgError;

    fn try_from(value: String) -> Result<Self> {
        let ok = value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok {
            Ok(Self(value))
        } else {
            Err(DgError::InvalidHash)
        }
    }
}

impl From<Sha256Hex> for String {
    fn from(value: Sha256Hex) -> Self {
        value.0
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Serializes `value` as canonical JSON: object keys sorted bytewise at every
/// depth, no whitespace.
///
/// Why: `serde_json`'s map order depends on the workspace-unified
/// `preserve_order` feature, so a hash over `to_string` output could change
/// with an unrelated crate's feature set.
/// What: converts to a `Value` and writes it with an explicit key sort.
/// Test: `canonical_hash_ignores_field_order`.
pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    let mut out = String::new();
    write_canonical(&value, &mut out)?;
    Ok(out)
}

/// sha256 of [`canonical_json`].
///
/// Test: `canonical_hash_ignores_field_order`.
pub fn canonical_sha256<T: Serialize + ?Sized>(value: &T) -> Result<Sha256Hex> {
    Ok(Sha256Hex::digest(canonical_json(value)?.as_bytes()))
}

fn write_canonical(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                if let Some(inner) = map.get(key) {
                    write_canonical(inner, out)?;
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar)?),
    }
    Ok(())
}

/// `sha256(session_id | tool_use_id)`: the only form in which a dispatch's raw
/// identifiers reach a stored event (§3).
///
/// Test: `stored_event_carries_no_raw_ids`.
pub fn action_id(session_id: &str, tool_use_id: &str) -> Sha256Hex {
    Sha256Hex::digest(format!("{session_id}|{tool_use_id}").as_bytes())
}

/// `sha256(session_id)`, stored as `actor.session_hash`.
///
/// Test: `stored_event_carries_no_raw_ids`.
pub fn session_hash(session_id: &str) -> Sha256Hex {
    Sha256Hex::digest(session_id.as_bytes())
}

/// The design's dedupe key:
/// `sha256(boundary | action_id | policy_rev | prompt_sha256 | tree_sha256)`.
///
/// Why: a forwarder retry or a repeated observation must map to the stored
/// verdict instead of a second inference call (§3 "Dedupe", §5 forwarder).
/// What: joins the five parts with `|`. No part can contain `|`: the boundary
/// is an enum word and the rest are hex digests. A contract-boundary event has
/// no worker snapshot, so its `tree_sha256` part is the empty string.
/// Test: `dedupe_key_changes_with_each_input`, `dedupe_set_detects_repeat_key`.
pub fn dedupe_key(
    boundary: &str,
    action_id: &Sha256Hex,
    policy_rev: &Sha256Hex,
    prompt_sha256: &Sha256Hex,
    tree_sha256: Option<&Sha256Hex>,
) -> Sha256Hex {
    let tree = tree_sha256.map(Sha256Hex::as_str).unwrap_or("");
    let joined = format!("{boundary}|{action_id}|{policy_rev}|{prompt_sha256}|{tree}");
    Sha256Hex::digest(joined.as_bytes())
}

/// Whether a dedupe key was seen before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// First observation: evaluate.
    First,
    /// Repeat: bump `seen_count`, make no inference call.
    Duplicate,
}

/// In-memory set of dedupe keys; the evaluator store (PR3) persists the same
/// relation.
///
/// Test: `dedupe_set_detects_repeat_key`.
#[derive(Debug, Default)]
pub struct DedupeSet {
    keys: HashSet<Sha256Hex>,
}

impl DedupeSet {
    /// Records `key` and reports whether it was already present.
    pub fn observe(&mut self, key: &Sha256Hex) -> Seen {
        if self.keys.insert(key.clone()) {
            Seen::First
        } else {
            Seen::Duplicate
        }
    }
}
