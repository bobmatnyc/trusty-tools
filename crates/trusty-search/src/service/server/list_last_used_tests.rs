//! Issue #6424 tests for `last_used_unix` on `GET /indexes?details=true`.
//!
//! Why: the trusty-console index roster shows a Last Used column and sorts by
//! it. The daemon already persisted `last_queried_unix` (throttled to one write
//! per `LAST_QUERIED_WRITE_INTERVAL_SECS` per index) and `last_indexed_unix` in
//! `indexes.toml` for selective warm boot, but no API read them back, so the
//! console had nothing to show. These tests seed `indexes.toml` directly — the
//! same no-env-mutation isolation `list_repo_identity_tests.rs` documents at
//! length (#2717) and this file does not restate.
//! What: three cases — both halves present picks the later one, neither half
//! present omits the field entirely, and a persisted stamp survives a fresh
//! `SearchAppState` (the restart round-trip).
//! Test: this module IS the tests
//! (`cargo test -p trusty-search -- last_used`).

use super::*;
use axum::extract::{Query, State};

/// Build a registry + state over `entries`, registering one bare handle per
/// seeded row. Returns the tempdirs so the caller can keep them alive.
fn state_over(
    entries: Vec<(&str, Option<u64>, Option<u64>)>,
) -> (
    Vec<tempfile::TempDir>,
    tempfile::TempDir,
    std::sync::Arc<SearchAppState>,
) {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use crate::service::persistence::PersistedIndex;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let roots: Vec<tempfile::TempDir> = entries
        .iter()
        .map(|_| tempfile::tempdir().expect("root tempdir"))
        .collect();

    let persisted: Vec<PersistedIndex> = entries
        .iter()
        .zip(roots.iter())
        .map(|((id, queried, indexed), root)| PersistedIndex {
            id: (*id).to_string(),
            root_path: root.path().to_path_buf(),
            last_queried_unix: *queried,
            last_indexed_unix: *indexed,
            ..Default::default()
        })
        .collect();

    let data_dir = tempfile::tempdir().expect("data tempdir");
    let toml_path = data_dir.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(&toml_path, &persisted)
        .expect("seed indexes.toml");

    let registry = IndexRegistry::new();
    for ((id, _, _), root) in entries.iter().zip(roots.iter()) {
        let indexer = CodeIndexer::new(*id, root.path().to_string_lossy().to_string());
        registry.register(IndexHandle::bare(
            IndexId::new(*id),
            Arc::new(RwLock::new(indexer)),
            root.path().to_path_buf(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry).with_registry_path(toml_path));
    (roots, data_dir, state)
}

/// Ask `GET /indexes?details=true` for the `last_used_unix` of every entry.
async fn last_used_map(
    state: std::sync::Arc<SearchAppState>,
) -> std::collections::HashMap<String, Option<u64>> {
    use super::indexes::ListIndexesParams;
    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: None,
            details: true,
            repo_identity: None,
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_string(),
                e.get("last_used_unix").and_then(serde_json::Value::as_u64),
            )
        })
        .collect()
}

/// The reported stamp is the LATER of the query and index timestamps (#6424).
///
/// Why: "last used" is one column, and either kind of touch counts as use. An
/// index reindexed this morning and last searched a month ago was used this
/// morning; reporting only `last_queried_unix` would call it a month stale.
/// What: seeds one index whose index stamp is newer, one whose query stamp is
/// newer, and asserts each reports its own maximum.
/// Test: this function.
#[tokio::test]
async fn list_indexes_details_includes_last_used_unix() {
    let (_roots, _data, state) = state_over(vec![
        ("indexed-later", Some(1_700_000_000), Some(1_800_000_000)),
        ("queried-later", Some(1_900_000_000), Some(1_750_000_000)),
    ]);

    let map = last_used_map(state).await;
    assert_eq!(
        map.get("indexed-later"),
        Some(&Some(1_800_000_000)),
        "a reindex is use: the later last_indexed_unix must win"
    );
    assert_eq!(
        map.get("queried-later"),
        Some(&Some(1_900_000_000)),
        "a search is use: the later last_queried_unix must win"
    );
}

/// A never-used index reports NO stamp, not a zero (#6424).
///
/// Why: `warmboot_sort_key` collapses both absent halves to `0`, which as a
/// timestamp reads as 1970 — a date the console would render and sort as a real
/// one. Absent has to stay absent so the tab can render "never" and sort it
/// last, which is what the pre-feature rows on every existing daemon look like.
/// What: seeds a row with neither half set and asserts the field is omitted
/// from the JSON entirely (`skip_serializing_if`).
/// Test: this function.
#[tokio::test]
async fn list_indexes_details_last_used_absent_when_never_used() {
    let (_roots, _data, state) = state_over(vec![("never-touched", None, None)]);

    use super::indexes::ListIndexesParams;
    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: None,
            details: true,
            repo_identity: None,
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let entry = &value["indexes"][0];
    assert_eq!(entry["id"].as_str(), Some("never-touched"));
    assert!(
        entry.get("last_used_unix").is_none(),
        "a never-used index must omit the field, not report the epoch: {entry}"
    );
}

/// The stamp survives a daemon restart (#6424).
///
/// Why: the column is worthless if it resets to "never" every time the daemon
/// is bounced. The durability comes from `indexes.toml`, which outlives the
/// process; this pins that the read path takes it from there and not from any
/// in-memory state built during the run.
/// What: writes a stamp through `patch_index_registry_entry_at` — the exact
/// primitive `update_last_queried_unix` uses, differing only in taking the path
/// instead of resolving it from global env — then builds a SECOND, entirely
/// fresh `SearchAppState` over that same file and asserts the value comes back.
/// Test: this function.
#[tokio::test]
async fn last_used_unix_survives_a_fresh_daemon_state() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use crate::service::persistence::PersistedIndex;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let root = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let toml_path = data_dir.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(
        &toml_path,
        &[PersistedIndex {
            id: "survivor".into(),
            root_path: root.path().to_path_buf(),
            ..Default::default()
        }],
    )
    .expect("seed");

    // The write the search handler performs, against this registry file.
    crate::service::persistence::patch_index_registry_entry_at(&toml_path, "survivor", |entry| {
        entry.last_queried_unix = Some(1_950_000_000);
    })
    .expect("stamp");

    // A brand-new state — nothing carried over from the write above.
    let registry = IndexRegistry::new();
    let indexer = CodeIndexer::new("survivor", root.path().to_string_lossy().to_string());
    registry.register(IndexHandle::bare(
        IndexId::new("survivor"),
        Arc::new(RwLock::new(indexer)),
        root.path().to_path_buf(),
    ));
    let state = Arc::new(SearchAppState::new(registry).with_registry_path(toml_path));

    let map = last_used_map(state).await;
    assert_eq!(
        map.get("survivor"),
        Some(&Some(1_950_000_000)),
        "the stamp must be read back off indexes.toml, not from process state"
    );
}
