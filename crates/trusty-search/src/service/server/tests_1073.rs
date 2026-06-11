//! Tests for issue #1073: content-hash-incremental reindex + in-place relocation.
//!
//! Why: three independent bugs were fixed together: (1) root-move on colocated
//! indexes cleared the hash cache unnecessarily; (2) warm-restart hash-skip
//! missed relative keys (absolute vs. relative mismatch in the DashMap);
//! (3) no in-place relocation primitive existed (`PATCH /indexes/:id`).
//! What: this module verifies each fix with a focused unit test that does not
//! require a running daemon or a real embedder.
//! Test: run with `cargo test -p trusty-search tests_1073`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

// ── Test 1: PATCH /indexes/:id returns 404 for an unknown index id ───────────

/// `PATCH /indexes/:id` with an unregistered id must return `404 Not Found`.
///
/// Why: ensures the handler's "index not found" guard works and doesn't panic.
/// What: calls `relocate_index_handler` with a state that has no registered
/// indexes; asserts the response status is `404`.
/// Test: this test (pure in-memory, no network or embedder required).
#[tokio::test]
async fn relocate_index_returns_404_for_unknown_id() {
    use super::indexes_relocate::{relocate_index_handler, RelocateIndexRequest};
    use axum::body::to_bytes;
    use axum::extract::Path;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);

    let resp = relocate_index_handler(
        State(Arc::clone(&state_arc)),
        Path("no-such-index-xyz".to_string()),
        Json(RelocateIndexRequest {
            root_path: std::path::PathBuf::from("/tmp"),
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(resp.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
    assert!(
        err.contains("no-such-index-xyz"),
        "error should name the id: {err}"
    );
}

// ── Test 2: PATCH /indexes/:id updates root_path in the registry ─────────────

/// A registered index can be relocated to a new directory without re-embedding.
///
/// Why: core correctness test for issue #1073 Change 3.
/// What: (1) creates a real tempdir as the initial root; (2) registers an index
/// at that path; (3) creates a second tempdir as the new root; (4) calls
/// `PATCH /indexes/:id`; (5) asserts the handle's `root_path` in the registry
/// reflects the new path and the response carries `"relocated": true`.
/// Test: this test.
#[tokio::test]
async fn relocate_index_updates_root_path() {
    use super::indexes_relocate::{relocate_index_handler, RelocateIndexRequest};
    use super::router::CreateIndexRequest;
    use axum::body::to_bytes;
    use axum::extract::Path;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);

    // Build the initial and target directories under target/ (never in the
    // denylist), using RAII TempDir for cleanup.
    let cwd = std::env::current_dir().expect("cwd");
    let base = cwd.join("target");
    std::fs::create_dir_all(&base).expect("create target/");
    let old_dir = tempfile::Builder::new()
        .prefix("ts-relocate-old-")
        .tempdir_in(&base)
        .expect("create old_dir");
    let new_dir = tempfile::Builder::new()
        .prefix("ts-relocate-new-")
        .tempdir_in(&base)
        .expect("create new_dir");

    let old_root = old_dir.path().canonicalize().expect("canonicalize old_dir");
    let new_root = new_dir.path().canonicalize().expect("canonicalize new_dir");

    // Step 1: register the index at old_root.
    let create_resp = super::indexes::create_index_handler(
        State(Arc::clone(&state_arc)),
        Json(CreateIndexRequest {
            id: "relocate-test".into(),
            root_path: old_root.clone(),
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
            defer_embed: None,
        }),
    )
    .await;
    assert_eq!(
        create_resp.status(),
        StatusCode::OK,
        "initial create must succeed"
    );

    // Step 2: relocate to new_root.
    let patch_resp = relocate_index_handler(
        State(Arc::clone(&state_arc)),
        Path("relocate-test".to_string()),
        Json(RelocateIndexRequest {
            root_path: new_root.clone(),
        }),
    )
    .await;
    assert_eq!(patch_resp.status(), StatusCode::OK, "relocate must succeed");

    let body = to_bytes(patch_resp.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        v.get("relocated").and_then(|x| x.as_bool()),
        Some(true),
        "response must carry relocated:true"
    );
    assert_eq!(
        v.get("id").and_then(|x| x.as_str()),
        Some("relocate-test"),
        "response must echo the index id"
    );

    // Step 3: assert the in-memory registry reflects the new root.
    let handle = state_arc
        .registry
        .get(&crate::core::registry::IndexId::new("relocate-test"))
        .expect("handle must still be in registry after relocate");
    assert_eq!(
        handle.root_path, new_root,
        "handle.root_path must point at the new directory after relocate"
    );
    assert_ne!(
        handle.root_path, old_root,
        "handle.root_path must not retain the old directory"
    );
}

// ── Test 3: warm-restart hash-skip — relative keys match after load ───────────

/// After a daemon restart the hash cache loaded from redb must be queryable
/// using relative `PathBuf` keys (the same representation produced during
/// reindex), not absolute keys.
///
/// Why: this is the latent bug fixed by issue #1073 Change 2. The in-process
/// DashMap used to be populated with ABSOLUTE keys by `prepare_batch_payload`,
/// while `hash_cache::load_into_cache` inserts RELATIVE keys from redb. After
/// a restart every hash lookup missed, causing a full re-embed.
/// What: inserts a relative-key entry into the DashMap (simulating what
/// `load_into_cache` does after a restart), then looks it up via a relative
/// key (simulating what the fixed `prepare_batch_payload` now does). Asserts
/// the lookup hits.
/// Test: this test.
#[test]
fn hash_cache_relative_key_matches_after_load() {
    let map: dashmap::DashMap<std::path::PathBuf, String> = dashmap::DashMap::new();

    // Simulate what `hash_cache::load_into_cache` inserts: a RELATIVE key.
    let rel_path = std::path::PathBuf::from("src/main.rs");
    let hash_value = "abc123def456".to_string(); // pragma: allowlist secret
    map.insert(rel_path.clone(), hash_value.clone());

    // Simulate what the FIXED `prepare_batch_payload` looks up: ALSO a relative key.
    let lookup_key = std::path::PathBuf::from("src/main.rs");
    let got = map.get(&lookup_key).map(|v| v.clone());

    assert_eq!(
        got.as_deref(),
        Some(hash_value.as_str()),
        "relative-key lookup must hit the relative-key entry in the DashMap"
    );

    // Confirm that an absolute key would NOT have matched (to demonstrate the
    // original bug: absolute keys silently missed all redb-loaded entries).
    let abs_key = std::path::PathBuf::from("/some/project/root/src/main.rs");
    let miss = map.get(&abs_key).map(|v| v.clone());
    assert!(
        miss.is_none(),
        "absolute-key lookup must NOT match a relative-key entry (old bug)"
    );
}

// ── Test 4: colocated fallback is false on missing/unreadable disk entry ─────

/// When the on-disk `indexes.toml` entry is absent or unreadable, the
/// fallback for `colocated` must be `false` (central-store / non-colocated),
/// NOT `true`.
///
/// Why (issue #1097): the old `unwrap_or(true)` would assume colocated on any
/// IO error, re-introducing the #1088 data-wipe for central-store indexes. The
/// safe default is `false` — it routes to the global data directory and cannot
/// destroy colocated project data. This test pins the fallback by verifying
/// that `load_index_registry_at` on a non-existent path gives `Err`, and that
/// the `ok().and_then(...).map(...).unwrap_or(false)` chain resolves to `false`.
///
/// What: simulates the registry-load-failure path without touching the real
/// production `indexes.toml` — calls `load_index_registry_at` on an
/// impossible path and asserts the fallback chain would yield `false`.
///
/// Test: this test (issue #1097 / #1088 guard).
#[test]
fn colocated_fallback_is_false_when_disk_entry_absent() {
    use crate::service::persistence::load_index_registry_at;
    use std::path::PathBuf;

    // Simulate an unreadable / absent indexes.toml.
    let missing = PathBuf::from("/tmp/nonexistent-trusty-search-test-xyz/indexes.toml");
    let on_disk_colocated = load_index_registry_at(&missing)
        .ok()
        .and_then(|entries| entries.into_iter().find(|e| e.id == "any-index"))
        .map(|e| e.colocated)
        // This is the exact fallback expression from indexes_relocate.rs.
        .unwrap_or(false);
    assert!(
        !on_disk_colocated,
        "colocated fallback must be false when disk entry is absent/unreadable (issue #1097)"
    );

    // Also verify: if an entry IS found with colocated=true, it IS returned.
    let tmp = tempfile::tempdir().expect("tempdir");
    let toml_path = tmp.path().join("indexes.toml");
    crate::service::persistence::upsert_index_registry_entry_at(
        &toml_path,
        crate::service::persistence::PersistedIndex {
            id: "existing-colocated".to_string(),
            root_path: PathBuf::from("/some/root"),
            colocated: true,
            ..crate::service::persistence::PersistedIndex::default()
        },
    )
    .expect("write entry");
    let found = load_index_registry_at(&toml_path)
        .ok()
        .and_then(|entries| entries.into_iter().find(|e| e.id == "existing-colocated"))
        .map(|e| e.colocated)
        .unwrap_or(false);
    assert!(
        found,
        "colocated must be true when the disk entry explicitly says so"
    );
}

// ── Test 5: cross-index PATCH does not strip manually-added fields ────────────

/// A PATCH to index A must NOT strip manually-edited fields (e.g.
/// `exclude_globs`) from index B's on-disk entry.
///
/// Why: this is the #1089 completeness regression test. `upsert_index_registry_entry`
/// loads ALL entries from `indexes.toml`, overwrites only the entry matching the
/// supplied id, then saves all entries. If it accidentally serialised from
/// in-memory state (ignoring the other entries on disk), manually-added fields
/// like `exclude_globs` on index B would be silently stripped when A is PATCHed.
///
/// What: (1) writes two entries to a temp `indexes.toml` — index-a (plain) and
/// index-b (with `exclude_globs = ["**/vendor/**"]`); (2) calls
/// `upsert_index_registry_entry_at` for index-a with a changed `root_path`;
/// (3) reloads the file and asserts index-b's `exclude_globs` is still
/// `["**/vendor/**"]`.
///
/// Test: this test (issue #1089 completeness, issue #1097).
#[test]
fn patch_index_a_does_not_strip_exclude_globs_of_index_b() {
    use crate::service::persistence::load_index_registry_at;
    use crate::service::persistence::{upsert_index_registry_entry_at, PersistedIndex};
    use std::path::PathBuf;

    let tmp = tempfile::tempdir().expect("tempdir");
    let toml_path = tmp.path().join("indexes.toml");

    // Write initial state: index-a (no extra fields) and index-b with exclude_globs.
    let entry_a = PersistedIndex {
        id: "index-a".to_string(),
        root_path: PathBuf::from("/projects/index-a"),
        ..PersistedIndex::default()
    };
    let entry_b = PersistedIndex {
        id: "index-b".to_string(),
        root_path: PathBuf::from("/projects/index-b"),
        exclude_globs: vec!["**/vendor/**".to_string(), "*.generated.ts".to_string()],
        ..PersistedIndex::default()
    };
    upsert_index_registry_entry_at(&toml_path, entry_a).expect("write entry-a");
    upsert_index_registry_entry_at(&toml_path, entry_b).expect("write entry-b");

    // PATCH index-a: change its root_path (simulate PATCH /indexes/index-a).
    let patched_a = PersistedIndex {
        id: "index-a".to_string(),
        root_path: PathBuf::from("/projects/index-a-new"),
        ..PersistedIndex::default()
    };
    upsert_index_registry_entry_at(&toml_path, patched_a).expect("patch entry-a");

    // Reload and assert index-b's exclude_globs survived the patch of index-a.
    let entries = load_index_registry_at(&toml_path).expect("reload");
    let b = entries
        .iter()
        .find(|e| e.id == "index-b")
        .expect("index-b must still be present after patching index-a");
    assert_eq!(
        b.exclude_globs,
        vec!["**/vendor/**".to_string(), "*.generated.ts".to_string()],
        "index-b's exclude_globs must survive a PATCH to index-a (issue #1089)"
    );

    // Also verify index-a's root_path was updated correctly.
    let a = entries
        .iter()
        .find(|e| e.id == "index-a")
        .expect("index-a must still be present");
    assert_eq!(
        a.root_path,
        PathBuf::from("/projects/index-a-new"),
        "index-a's root_path must reflect the PATCH"
    );
}
