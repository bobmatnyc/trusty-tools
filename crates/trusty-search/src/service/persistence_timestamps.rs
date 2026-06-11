//! LRU timestamp helpers for selective/lazy warm-boot (issue #993).
//!
//! Why: extracted from `persistence.rs` to keep that file under its allowlist
//! budget. These helpers are the only write path that touches `indexes.toml`
//! during normal query operation (rate-limited, fire-and-forget).
//! What: `warmboot_sort_key`, `read_last_queried_unix`,
//! `update_last_queried_unix`, `update_last_indexed_unix`.
//! Test: `last_queried_and_indexed_round_trips` and
//! `warmboot_sort_key_prefers_most_recent_activity` below.

use anyhow::Result;

use super::persistence::{
    indexes_toml_path, load_index_registry_at, upsert_index_registry_entry_at, PersistedIndex,
};

/// Compute the LRU sort key for lazy warm-boot ordering (issue #993).
///
/// Why: `TRUSTY_WARMBOOT_MAX_INDEXES` ranks indexes by recency before deciding
/// which N to warm-boot eagerly. The key is the most recent activity timestamp
/// across both querying and indexing, so indexes that are freshly reindexed but
/// not yet queried are ranked as recent.
/// What: returns `max(last_queried_unix, last_indexed_unix)` as a `u64`.
/// Indexes with both fields `None` (first boot or pre-upgrade) return `0`,
/// which places them last in descending sort — stable alpha sort by id breaks ties.
/// Test: `warmboot_sort_key_prefers_most_recent_activity` in this module.
pub fn warmboot_sort_key(entry: &PersistedIndex) -> u64 {
    let q = entry.last_queried_unix.unwrap_or(0);
    let i = entry.last_indexed_unix.unwrap_or(0);
    q.max(i)
}

/// Read the `last_queried_unix` field for `index_id` from `indexes.toml`.
///
/// Why (issue #993): the search handler needs to know when the field was last
/// written so it can apply the 60-second rate-limit before writing again.
/// What: loads `indexes.toml`, finds the entry by id, returns `last_queried_unix`.
/// Returns `None` when the registry cannot be loaded, the entry is absent, or
/// the field is not yet populated.
/// Test: exercised transitively by `update_last_queried_unix` round-trip.
pub fn read_last_queried_unix(index_id: &str) -> Option<u64> {
    let Ok(path) = indexes_toml_path() else {
        return None;
    };
    let entries = load_index_registry_at(&path).ok()?;
    entries
        .into_iter()
        .find(|e| e.id == index_id)
        .and_then(|e| e.last_queried_unix)
}

/// Write `now_unix` into the `last_queried_unix` field of an existing entry.
///
/// Why (issue #993): the search handler updates this field after each query so
/// future selective warm-boots pick the most-recently-used indexes for eager
/// loading. Rate-limited by the caller (≤ once per 60 s) to avoid constant I/O.
/// What: upserts the entry in `indexes.toml` with the updated timestamp.
/// No-op when the index_id is not found (the entry may have been deleted).
/// Test: `update_last_queried_unix_roundtrip` in persistence::tests.
pub fn update_last_queried_unix(index_id: &str, now_unix: u64) -> Result<()> {
    let path = indexes_toml_path()?;
    let mut entries = load_index_registry_at(&path).unwrap_or_default();
    let Some(entry) = entries.iter_mut().find(|e| e.id == index_id) else {
        return Ok(()); // Deleted between query and write — harmless.
    };
    entry.last_queried_unix = Some(now_unix);
    let updated = entry.clone();
    upsert_index_registry_entry_at(&path, updated)
}

/// Write `now_unix` into the `last_indexed_unix` field of an existing entry.
///
/// Why (issue #993): the reindex completion path updates this field so future
/// selective warm-boots include recently reindexed indexes in the eager set.
/// What: upserts the entry in `indexes.toml` with the updated timestamp.
/// No-op when the index_id is not found.
/// Test: `update_last_indexed_unix_roundtrip` in persistence::tests.
pub fn update_last_indexed_unix(index_id: &str, now_unix: u64) -> Result<()> {
    let path = indexes_toml_path()?;
    let mut entries = load_index_registry_at(&path).unwrap_or_default();
    let Some(entry) = entries.iter_mut().find(|e| e.id == index_id) else {
        return Ok(());
    };
    entry.last_indexed_unix = Some(now_unix);
    let updated = entry.clone();
    upsert_index_registry_entry_at(&path, updated)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::service::persistence::{load_index_registry_at, save_index_registry_at};

    /// Issue #993: `last_queried_unix` and `last_indexed_unix` default to `None`
    /// and survive a save/load round-trip. Legacy TOML without the fields loads
    /// as `None` (back-compat: existing files are not rewritten until queried).
    ///
    /// Why: pins the back-compat contract so upgrading the daemon does not
    /// rewrite every `indexes.toml` on first boot.
    /// What: default constructor, missing-field deserialization, explicit-value
    /// round-trip, and both fields independently set.
    /// Test: this test.
    #[test]
    fn last_queried_and_indexed_round_trips() {
        use crate::service::persistence::PersistedIndex;

        // Default constructor returns None.
        assert!(PersistedIndex::default().last_queried_unix.is_none());
        assert!(PersistedIndex::default().last_indexed_unix.is_none());

        // Loading legacy TOML without the fields gives None.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(
            &path,
            r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy_ts"
"#,
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].last_queried_unix.is_none(),
            "missing field must default to None (issue #993 back-compat)"
        );
        assert!(entries[0].last_indexed_unix.is_none());

        // Explicit values survive a save/load cycle.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        save_index_registry_at(
            &path,
            &[PersistedIndex {
                id: "queried".into(),
                root_path: PathBuf::from("/tmp/q"),
                last_queried_unix: Some(1_700_000_000),
                last_indexed_unix: Some(1_699_000_000),
                ..Default::default()
            }],
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].last_queried_unix, Some(1_700_000_000));
        assert_eq!(entries[0].last_indexed_unix, Some(1_699_000_000));
    }

    /// Issue #993: `warmboot_sort_key` returns max of queried/indexed timestamps,
    /// or 0 when both are None.
    ///
    /// Why: pins the sort-key logic used by `select_warmboot_entries`.
    /// What: a few representative cases covering all code paths.
    /// Test: this test.
    #[test]
    fn warmboot_sort_key_prefers_most_recent_activity() {
        use crate::service::persistence::PersistedIndex;

        let mk = |q: Option<u64>, i: Option<u64>| PersistedIndex {
            id: "x".into(),
            root_path: PathBuf::from("/x"),
            last_queried_unix: q,
            last_indexed_unix: i,
            ..Default::default()
        };
        // Both None → 0.
        assert_eq!(warmboot_sort_key(&mk(None, None)), 0);
        // Only queried.
        assert_eq!(warmboot_sort_key(&mk(Some(100), None)), 100);
        // Only indexed.
        assert_eq!(warmboot_sort_key(&mk(None, Some(200))), 200);
        // Queried wins.
        assert_eq!(warmboot_sort_key(&mk(Some(300), Some(200))), 300);
        // Indexed wins.
        assert_eq!(warmboot_sort_key(&mk(Some(100), Some(400))), 400);
    }
}
