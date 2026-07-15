//! DOC-37 (issue #2611) tests for the `repo_identity` join key surfaced by
//! `GET /indexes` — split out of `tests_list.rs` to keep that file under the
//! 500-SLOC production cap (this file's `_tests.rs` basename classifies it as
//! a test file under the 1500-SLOC cap instead).
//!
//! Why: `resolve_identities` (in `indexes.rs`) reads ONLY the persisted
//! `PersistedIndex::repo_identity` value — no per-request live derive (that
//! would shell out to `git` once per handle lacking a stored identity,
//! unbounded work inside an async HTTP handler). These tests therefore seed
//! `indexes.toml` directly via the `TRUSTY_DATA_DIR` isolation pattern
//! established by `residency_sweep_tests.rs` / `tests_index_config.rs`, rather
//! than relying on a live git-remote derive.
//! What: `list_indexes_repo_identity_details_and_filter` covers the flat +
//! `?details=true` response shapes; `list_indexes_repo_identity_tree_filter`
//! covers the `format=tree` branch, which was previously untested for the
//! filter.
//! Test: this module IS the tests (`cargo test -p trusty-search -- repo_identity`).

use super::*;
use axum::extract::{Query, State};

/// Point `TRUSTY_DATA_DIR` at a fresh tempdir and write `entries` to its
/// `indexes.toml`. `TRUSTY_DATA_DIR` is shared process env state, so every
/// caller MUST be `#[serial_test::serial]`. Returns the tempdir; keep it alive
/// for the test's duration, then call [`clear_data_dir_env`].
fn isolate_and_seed_toml(
    entries: &[crate::service::persistence::PersistedIndex],
) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", tmp.path()) };
    let path = crate::service::persistence::indexes_toml_path().expect("indexes_toml_path");
    crate::service::persistence::save_index_registry_at(&path, entries).expect("seed indexes.toml");
    tmp
}

fn clear_data_dir_env() {
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// `GET /indexes?details=true` carries the PERSISTED `repo_identity` (never a
/// per-request live derive — see `resolve_identities` in `indexes.rs` for why:
/// deriving per handle would shell out to `git` once per handle lacking a
/// stored identity, unbounded work inside an async HTTP handler), and
/// `?repo_identity=` filters the flat list to that repo only (DOC-37, issue
/// #2611).
///
/// Why: the visibility layer — an operator must be able to see, and select, all
/// indexes belonging to one repo, from data the handler can read in O(1) file
/// I/O rather than N subprocess spawns.
/// What: seeds `indexes.toml` with two indexes carrying different
/// `repo_identity` values, registers matching bare handles, asserts the details
/// response surfaces each stored `repo_identity`, and that the
/// `?repo_identity=` flat filter (mixed-case, to exercise normalisation)
/// returns only the matching index.
/// Test: this function.
#[tokio::test]
#[serial_test::serial]
async fn list_indexes_repo_identity_details_and_filter() {
    use super::indexes::ListIndexesParams;
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use crate::service::persistence::PersistedIndex;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let _data_dir = isolate_and_seed_toml(&[
        PersistedIndex {
            id: "idx-a".into(),
            root_path: root_a.path().to_path_buf(),
            repo_identity: Some("acme/alpha".into()),
            ..Default::default()
        },
        PersistedIndex {
            id: "idx-b".into(),
            root_path: root_b.path().to_path_buf(),
            repo_identity: Some("acme/beta".into()),
            ..Default::default()
        },
    ]);

    let registry = IndexRegistry::new();
    for (name, dir) in [("idx-a", root_a.path()), ("idx-b", root_b.path())] {
        let indexer = CodeIndexer::new(name, dir.to_string_lossy().to_string());
        registry.register(IndexHandle::bare(
            IndexId::new(name),
            Arc::new(RwLock::new(indexer)),
            dir.to_path_buf(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));

    // details=true → each entry carries its PERSISTED repo_identity.
    let resp = list_indexes_handler(
        State(state.clone()),
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
    let entries = value["indexes"].as_array().unwrap();
    let identity_for = |id: &str| -> Option<String> {
        entries
            .iter()
            .find(|e| e["id"].as_str() == Some(id))
            .and_then(|e| e["repo_identity"].as_str())
            .map(str::to_string)
    };
    assert_eq!(identity_for("idx-a").as_deref(), Some("acme/alpha"));
    assert_eq!(identity_for("idx-b").as_deref(), Some("acme/beta"));

    // ?repo_identity=Acme/Alpha (mixed case) → flat list contains only idx-a,
    // proving the filter normalises to canonical form before comparing.
    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: None,
            details: false,
            repo_identity: Some("Acme/Alpha".to_string()),
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let ids: Vec<&str> = value["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["idx-a"],
        "filter must return only the matching repo (case-normalised)"
    );

    clear_data_dir_env();
}

/// `GET /indexes?format=tree&repo_identity=…` applies the identity filter
/// BEFORE the tree/hierarchy entries are built (DOC-37, issue #2611) — the
/// `format=tree` branch was previously untested for the filter.
///
/// Why: the filter is threaded through three separate response branches (flat,
/// details, tree); only flat + details had coverage. The tree branch retains
/// handles before calling `build_tree_entries`, so a regression there would
/// silently leak unrelated indexes into a tree-scoped view.
/// What: seeds three indexes — two sharing one repo identity, one on another —
/// requests `format=tree` with a `repo_identity` filter, and asserts the
/// object-array response contains exactly the two matching ids.
/// Test: this function.
#[tokio::test]
#[serial_test::serial]
async fn list_indexes_repo_identity_tree_filter() {
    use super::indexes::ListIndexesParams;
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use crate::service::persistence::PersistedIndex;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let root_c = tempfile::tempdir().unwrap();
    let _data_dir = isolate_and_seed_toml(&[
        PersistedIndex {
            id: "tree-a".into(),
            root_path: root_a.path().to_path_buf(),
            repo_identity: Some("acme/widget".into()),
            ..Default::default()
        },
        PersistedIndex {
            id: "tree-b".into(),
            root_path: root_b.path().to_path_buf(),
            repo_identity: Some("acme/widget".into()),
            ..Default::default()
        },
        PersistedIndex {
            id: "tree-c".into(),
            root_path: root_c.path().to_path_buf(),
            repo_identity: Some("acme/other".into()),
            ..Default::default()
        },
    ]);

    let registry = IndexRegistry::new();
    for (name, dir) in [
        ("tree-a", root_a.path()),
        ("tree-b", root_b.path()),
        ("tree-c", root_c.path()),
    ] {
        let indexer = CodeIndexer::new(name, dir.to_string_lossy().to_string());
        registry.register(IndexHandle::bare(
            IndexId::new(name),
            Arc::new(RwLock::new(indexer)),
            dir.to_path_buf(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));

    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: Some("tree".to_string()),
            details: false,
            repo_identity: Some("acme/widget".to_string()),
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let arr = value["indexes"].as_array().expect("indexes array");
    let mut ids: Vec<&str> = arr.iter().filter_map(|e| e["id"].as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec!["tree-a", "tree-b"],
        "tree format must apply the repo_identity filter before building entries"
    );

    clear_data_dir_env();
}
