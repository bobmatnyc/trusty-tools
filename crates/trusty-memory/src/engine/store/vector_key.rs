//! Vector-store row keys: one drawer, one row per chunk (#9275).
//!
//! Why: a drawer longer than the embedder's input window is stored as several
//! vectors. Each needs its own `VECTOR_KEYS` row, and every drawer-level
//! surface must map those rows back to one drawer. The grammar lives here so
//! search, remove, compaction and alias repair all read keys the same way.
//! What: `vector-key = drawer [ "#" chunk-n ]`, where `chunk-n` is decimal,
//! 1..=`u32::MAX`, with no sign and no leading zero. The bare key IS chunk 0,
//! so every row written before #9275 is a valid one-chunk drawer and nothing
//! migrates. `#0`, `#01`, an empty or non-decimal suffix, and a second `#` are
//! malformed: a key that does not parse names no drawer.
//! Test: `vector_key_tests.rs`.

use uuid::Uuid;

/// Most chunk rows one drawer may hold.
///
/// Why: ~1300 chunks covers a 1 MB drawer at ~200 tokens per chunk; the cap
/// stops a runaway chunker, it does not bound normal use (Architect ruling Q4:
/// a hard error, never truncation).
pub(crate) const MAX_CHUNKS_PER_DRAWER: usize = 4096;

/// Why a vector key does not name a drawer chunk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum KeyError {
    /// The text after `#` is not a chunk number in 1..=`u32::MAX`.
    #[error("vector key {0:?} has a malformed chunk suffix")]
    BadSuffix(String),
    /// The drawer part is not a uuid.
    #[error("vector key {key:?} does not start with a drawer uuid: {reason}")]
    BadDrawer { key: String, reason: String },
}

/// Split `key` into its drawer part and chunk index, without reading the
/// drawer part as a uuid.
///
/// Why: `HnswStore` keys are opaque strings; it groups rows by drawer without
/// knowing what a drawer id looks like.
/// What: `Some((key, 0))` for a key with no `#`; `Some((drawer, n))` for
/// `drawer#n` with a valid `n`; `None` for a malformed suffix.
/// Test: `bare_uuid_is_chunk_zero`,
/// `rejects_zero_suffix_leading_zero_empty_and_nested`.
pub(crate) fn split(key: &str) -> Option<(&str, u32)> {
    let Some((drawer, suffix)) = key.split_once('#') else {
        return Some((key, 0));
    };
    let decimal = suffix.bytes().all(|b| b.is_ascii_digit());
    if !decimal || suffix.starts_with('0') {
        return None;
    }
    // Empty and > u32::MAX both fail here.
    suffix.parse::<u32>().ok().map(|n| (drawer, n))
}

/// The drawer a row belongs to, for grouping: the drawer part of a valid key,
/// or the whole key when its suffix is malformed, so a bad row is a group of
/// its own and never joins a real drawer.
/// Test: `rejects_zero_suffix_leading_zero_empty_and_nested`.
pub(crate) fn group_key(key: &str) -> &str {
    split(key).map_or(key, |(drawer, _)| drawer)
}

/// Read `key` as `(drawer uuid, chunk index)`.
/// Test: `suffix_n_parses_to_chunk_n`,
/// `rejects_zero_suffix_leading_zero_empty_and_nested`.
pub(crate) fn parse(key: &str) -> Result<(Uuid, u32), KeyError> {
    let (drawer, chunk) = split(key).ok_or_else(|| KeyError::BadSuffix(key.to_string()))?;
    let id = Uuid::parse_str(drawer).map_err(|e| KeyError::BadDrawer {
        key: key.to_string(),
        reason: e.to_string(),
    })?;
    Ok((id, chunk))
}

/// The key of chunk `chunk` of `drawer`: bare for chunk 0.
/// Test: `format_parse_round_trip_for_0_1_and_u32_max`.
pub(crate) fn format(drawer: &str, chunk: u32) -> String {
    if chunk == 0 {
        drawer.to_string()
    } else {
        format!("{drawer}#{chunk}")
    }
}

/// Half-open key range `[lo, hi)` holding every `#`-suffixed row of `drawer`.
///
/// Why: redb orders `&str` keys bytewise, and `#` (0x23) is followed by `$`
/// (0x24), so `["{drawer}#", "{drawer}$")` holds exactly the keys that start
/// with `{drawer}#`. Drawer uuids are fixed-length, so no other drawer's key
/// falls inside. The bare key is outside the range; callers add it.
/// Test: `group_range_bounds_contain_only_this_drawers_suffixed_keys`.
pub(crate) fn group_range(drawer: &str) -> (String, String) {
    (format!("{drawer}#"), format!("{drawer}$"))
}

#[cfg(test)]
#[path = "vector_key_tests.rs"]
mod tests;
