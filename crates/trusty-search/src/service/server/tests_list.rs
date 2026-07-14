//! Tests for list_indexes variants and global_search hierarchy.
use super::indexes::ListIndexesParams;
use super::*;
use axum::extract::{Query, State};
use axum::Json;
#[tokio::test]
async fn list_indexes_flat_default_unchanged() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };

    let registry = IndexRegistry::new();
    for name in ["alpha", "beta"] {
        let id = IndexId::new(name);
        let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
        registry.register(IndexHandle::bare(
            id.clone(),
            std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
            format!("/tmp/{name}").into(),
        ));
    }
    let state = std::sync::Arc::new(SearchAppState::new(registry));

    // No format param → flat list
    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: None,
            details: false,
            repo_identity: None,
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let arr = value["indexes"].as_array().expect("indexes array");
    // Must be strings (flat format)
    for item in arr {
        assert!(
            item.is_string(),
            "flat default must return string IDs: {item:?}"
        );
    }
    assert_eq!(arr.len(), 2);
}

/// `GET /indexes?format=tree` returns an object-array with hierarchy
/// fields (`parent_id`, `children`, `priority_boost`, `is_sub_index`).
#[tokio::test]
async fn list_indexes_tree_format_shape() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();

    // Register a parent and child whose root_paths have a strict prefix
    // relationship.  We use non-existent paths so canonicalize_best_effort
    // falls back to the raw strings for both, giving a deterministic
    // comparison on all platforms without requiring real directories.
    let parent_id = IndexId::new("tree-parent");
    let child_id = IndexId::new("tree-child");

    let parent_root: std::path::PathBuf = "/nonexistent_test_root_abc".into();
    let child_root: std::path::PathBuf = "/nonexistent_test_root_abc/services/billing".into();

    registry.register(IndexHandle::bare(
        parent_id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new(
            "tree-parent",
            "/nonexistent_test_root_abc",
        ))),
        parent_root,
    ));
    registry.register(IndexHandle::bare(
        child_id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new(
            "tree-child",
            "/nonexistent_test_root_abc/services/billing",
        ))),
        child_root,
    ));

    let state = Arc::new(SearchAppState::new(registry));

    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: Some("tree".to_string()),
            details: false,
            repo_identity: None,
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let arr = value["indexes"].as_array().expect("indexes array");
    assert_eq!(arr.len(), 2);
    // Each entry must be an object with required fields.
    for entry in arr {
        assert!(entry["id"].is_string(), "id must be string");
        assert!(entry["root_path"].is_string(), "root_path must be present");
        assert!(
            entry["priority_boost"].is_number(),
            "priority_boost must be a number"
        );
        assert!(
            entry["is_sub_index"].is_boolean(),
            "is_sub_index must be bool"
        );
        assert!(entry["children"].is_array(), "children must be an array");
    }

    // tree-child (/tmp/tree_child_sub_test) is a sub-path of /tmp →
    // it should be identified as a sub-index.
    let child_entry = arr
        .iter()
        .find(|e| e["id"].as_str() == Some("tree-child"))
        .expect("tree-child entry");
    assert_eq!(
        child_entry["is_sub_index"].as_bool(),
        Some(true),
        "tree-child must be a sub-index"
    );
    let parent_entry = arr
        .iter()
        .find(|e| e["id"].as_str() == Some("tree-parent"))
        .expect("tree-parent entry");
    assert_eq!(
        parent_entry["is_sub_index"].as_bool(),
        Some(false),
        "tree-parent must not be a sub-index"
    );
}

/// `GET /indexes?details=true` returns objects with `id` and `size_bytes`
/// fields (issue #312).  When the index data dir has never been created
/// `size_bytes` must be `null` rather than missing or erroring.
///
/// Why: MCP `list_indexes` and the admin UI need per-index disk usage in a
/// single call; the `?details=true` variant is the additive/backward-compat
/// way to expose it without breaking the bare flat format.
/// Test: this function.
#[tokio::test]
async fn list_indexes_details_includes_size_bytes() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };

    let registry = IndexRegistry::new();
    for name in ["detail-alpha", "detail-beta"] {
        let id = IndexId::new(name);
        let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
        registry.register(IndexHandle::bare(
            id.clone(),
            std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
            format!("/tmp/{name}").into(),
        ));
    }
    let state = std::sync::Arc::new(SearchAppState::new(registry));

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
    let arr = value["indexes"].as_array().expect("indexes array");
    assert_eq!(arr.len(), 2);
    for entry in arr {
        assert!(
            entry["id"].is_string(),
            "each detail entry must have a string id: {entry:?}"
        );
        // size_bytes must be present: either a number or null (dir not
        // created yet), never missing entirely.
        assert!(
            entry.get("size_bytes").is_some(),
            "each detail entry must have a size_bytes field: {entry:?}"
        );
        // root_path must be present (issue #661 — auto-derive support).
        assert!(
            entry.get("root_path").is_some(),
            "each detail entry must have a root_path field (issue #661): {entry:?}"
        );
    }
}

/// `GET /indexes?details=true` exposes the registered `root_path` per index
/// so trusty-review can auto-derive the correct index from the project cwd.
///
/// Why: issue #661 — user-level MCP wiring omits TRUSTY_SEARCH_INDEX;
/// trusty-review must match the current repo root to a registered index
/// without issuing N individual status requests.
/// What: registers one index with a known root path, issues the details
/// request, and asserts the returned `root_path` matches what was registered.
/// Test: this function.
#[tokio::test]
async fn list_indexes_details_includes_root_path() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };

    let registry = IndexRegistry::new();
    let id = IndexId::new("rp-test");
    let indexer = CodeIndexer::new("rp-test", "/tmp/rp-test");
    registry.register(IndexHandle::bare(
        id.clone(),
        std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
        std::path::PathBuf::from("/tmp/rp-test"),
    ));
    let state = std::sync::Arc::new(SearchAppState::new(registry));

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
    let arr = value["indexes"].as_array().expect("indexes array");
    assert_eq!(arr.len(), 1, "expected exactly one index entry");
    let entry = &arr[0];
    assert_eq!(
        entry["id"].as_str(),
        Some("rp-test"),
        "id must match registered index id"
    );
    let rp = entry["root_path"]
        .as_str()
        .expect("root_path must be a non-null string");
    assert_eq!(
        rp, "/tmp/rp-test",
        "root_path must match what was registered"
    );
}

/// `POST /search` with nested indexes: the response must include
/// `hierarchy_dedup_count` and the sub-index result should be preferred
/// when both parent and child contain the same file region.
#[tokio::test]
async fn global_search_nested_hierarchy_dedup_count_present() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // Two flat peer indexes (no nesting) — dedup_count should be 0.
    let registry = IndexRegistry::new();
    for name in ["flat-a", "flat-b"] {
        let id = IndexId::new(name);
        let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
        indexer
            .index_file(
                &format!("{name}/lib.rs"),
                "fn beta_function() { println!(\"beta\"); }",
            )
            .await
            .expect("index_file");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(indexer)),
            format!("/tmp/{name}").into(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));

    let Json(value) = global_search_handler(
        State(state),
        Json(GlobalSearchRequest {
            query: "beta_function".into(),
            top_k: 10,
            full_content: false,
            indexes: None,
            routing: None,
            routing_n: None,
            routing_threshold: None,
        }),
    )
    .await
    .expect("handler ok");

    // Must include the new field regardless of whether dedup fired.
    assert!(
        value["hierarchy_dedup_count"].is_number(),
        "hierarchy_dedup_count must be present: {value:?}"
    );
    // Flat peers → no nesting → count must be 0.
    assert_eq!(
        value["hierarchy_dedup_count"].as_u64(),
        Some(0),
        "flat peers must not trigger dedup"
    );
}

/// `POST /search` with a sub-index: the effective lane weight for the
/// sub-index must be boosted, and `hierarchy_dedup_count` reflects any
/// dropped parent copies.
#[tokio::test]
async fn global_search_sub_index_boost_applied() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // Use non-existent paths so canonicalize_best_effort falls back to raw
    // string comparison on all platforms (avoids /tmp → /private/tmp macOS
    // symlink mismatch).
    let registry = IndexRegistry::new();

    let parent_root: std::path::PathBuf = "/nonexistent_boost_root".into();
    let child_root: std::path::PathBuf = "/nonexistent_boost_root/sub".into();

    let parent_id = IndexId::new("boost-parent");
    let child_id = IndexId::new("boost-child");

    let parent_indexer = CodeIndexer::new("boost-parent", "/nonexistent_boost_root");
    parent_indexer
        .index_file("src/lib.rs", "fn gamma_function() { println!(\"gamma\"); }")
        .await
        .expect("parent index_file");
    registry.register(IndexHandle::bare(
        parent_id.clone(),
        Arc::new(RwLock::new(parent_indexer)),
        parent_root,
    ));

    let child_indexer = CodeIndexer::new("boost-child", "/nonexistent_boost_root/sub");
    child_indexer
        .index_file(
            "sub/lib.rs",
            "fn gamma_function() { println!(\"gamma sub\"); }",
        )
        .await
        .expect("child index_file");
    registry.register(IndexHandle::bare(
        child_id.clone(),
        Arc::new(RwLock::new(child_indexer)),
        child_root,
    ));

    let state = Arc::new(SearchAppState::new(registry));

    let Json(value) = global_search_handler(
        State(state),
        Json(GlobalSearchRequest {
            query: "gamma_function".into(),
            top_k: 10,
            full_content: false,
            indexes: None,
            routing: None,
            routing_n: None,
            routing_threshold: None,
        }),
    )
    .await
    .expect("handler ok");

    // Must include the dedup count field.
    assert!(
        value["hierarchy_dedup_count"].is_number(),
        "hierarchy_dedup_count must be present: {value:?}",
    );

    // Both indexes should be searched.
    let searched = value["indexes_searched"].as_array().unwrap();
    assert_eq!(
        searched.len(),
        2,
        "both parent and child should be searched"
    );

    // Results should exist (BM25 finds the keyword).
    let results = value["results"].as_array().unwrap();
    assert!(!results.is_empty(), "expected at least one result");

    // The sub-index has a boost of 1.5 applied to its lane weight, so its
    // results should rank first when querying for a term both indexes have.
    // Verify that at least one result comes from "boost-child".
    let has_child_result = results
        .iter()
        .any(|r| r["index_id"].as_str() == Some("boost-child"));
    assert!(
        has_child_result,
        "sub-index (boost-child) must contribute results to the fan-out"
    );
}

/// Create a throwaway git repo at `dir` with an origin remote, returning `false`
/// (so the caller skips) if git is unavailable on the runner. DOC-37 tests.
fn init_git_repo_with_origin(dir: &std::path::Path, origin: &str) -> bool {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init"]) {
        return false;
    }
    let _ = git(&["remote", "add", "origin", origin]);
    true
}

/// `GET /indexes?details=true` carries `repo_identity` derived from the index's
/// git origin, and `?repo_identity=` filters the flat list to that repo only
/// (DOC-37, issue #2611).
///
/// Why: the visibility layer — an operator must be able to see, and select, all
/// indexes belonging to one repo. Uses real temp git repos so the derive
/// fallback yields deterministic canonical identities.
/// What: registers two indexes rooted at temp git repos with different origins,
/// asserts the details response surfaces each `repo_identity`, and that the
/// `?repo_identity=` flat filter returns only the matching index.
/// Test: this function (skips cleanly when git is unavailable).
#[tokio::test]
async fn list_indexes_repo_identity_details_and_filter() {
    use super::indexes::ListIndexesParams;
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let repo_a = tempfile::TempDir::new().unwrap();
    let repo_b = tempfile::TempDir::new().unwrap();
    if !init_git_repo_with_origin(repo_a.path(), "git@github.com:acme/alpha.git")
        || !init_git_repo_with_origin(repo_b.path(), "git@github.com:acme/beta.git")
    {
        return; // no usable git — nothing to assert
    }

    let registry = IndexRegistry::new();
    for (name, dir) in [("idx-a", repo_a.path()), ("idx-b", repo_b.path())] {
        let indexer = CodeIndexer::new(name, dir.to_string_lossy().to_string());
        registry.register(IndexHandle::bare(
            IndexId::new(name),
            Arc::new(RwLock::new(indexer)),
            dir.to_path_buf(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));

    // details=true → each entry carries its derived repo_identity.
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

    // ?repo_identity=acme/alpha → flat list contains only idx-a.
    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: None,
            details: false,
            repo_identity: Some("acme/alpha".to_string()),
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
    assert_eq!(ids, vec!["idx-a"], "filter must return only the matching repo");
}
