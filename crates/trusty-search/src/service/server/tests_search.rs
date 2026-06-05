//! Tests for `file_is_within_root` and the search handler.
use super::helpers::file_is_within_root;
use super::*;
use axum::Json;
#[test]
fn file_is_within_root_relative_ok() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(file_is_within_root("src/auth.rs", root));
    assert!(file_is_within_root("./src/auth.rs", root));
    assert!(file_is_within_root("Cargo.toml", root));
}

/// Issue #64: relative paths that climb out via `..` must be rejected,
/// even though they may resolve inside `root` for some `root` values.
#[test]
fn file_is_within_root_rejects_dotdot() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(!file_is_within_root("../other/file.rs", root));
    assert!(!file_is_within_root("src/../../leak.rs", root));
}

/// Issue #64: absolute paths must literally start with the index root.
/// This is the load-bearing guard against cross-index bleed when the
/// daemon ever stores absolute file paths (e.g. legacy chunks from a
/// misregistered index — see #63).
#[test]
fn file_is_within_root_absolute_must_start_with_root() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(file_is_within_root("/Users/me/proj/src/auth.rs", root));
    assert!(!file_is_within_root(
        "/Users/me/other-proj/src/auth.rs",
        root
    ));
    assert!(!file_is_within_root("/etc/passwd", root));
}

/// Issue #64: empty file strings are defensively rejected — they should
/// never occur in a valid chunk and we don't want them sneaking past
/// the filter as a benign-looking relative path.
#[test]
fn file_is_within_root_rejects_empty() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(!file_is_within_root("", root));
}

/// Issue #541: when the index root is a symlink alias pointing at a real
/// directory, an absolute file path stored under the real (canonical) root
/// must NOT be dropped — `file_is_within_root` must fall back to
/// canonicalized comparison and return `true`.
///
/// This exercises the slow-path fallback added for #541: the lexical check
/// `/real/dir/src/auth.rs`.starts_with(`/link`) fails, so the predicate
/// canonicalizes both sides and retries.
#[cfg(unix)]
#[test]
fn file_is_within_root_symlinked_root_does_not_drop_valid_result() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    // Create a real directory that will be the "canonical" root.
    let real_dir = tempdir().unwrap();
    let canonical_root = std::fs::canonicalize(real_dir.path()).unwrap();

    // Symlink → real_dir (the handle holds the symlink path as its root_path).
    let link = canonical_root
        .parent()
        .unwrap()
        .join(format!("trusty-541-root-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    symlink(&canonical_root, &link).expect("create symlink");

    // A file stored with its canonical (non-symlink) absolute path — this
    // is exactly what the indexer produces after walking the real directory.
    let file_path = canonical_root.join("src/auth.rs");
    let file_str = file_path.to_str().unwrap();

    // With the link as `root`, the lexical check fails but the canonical
    // fallback must pass — the file IS within the root.
    let result = file_is_within_root(file_str, &link);
    let _ = std::fs::remove_file(&link);

    assert!(
        result,
        "file under canonical root must pass even when index root is a symlink alias; \
             file={file_str}, root={link}",
        link = link.display(),
    );
}

/// Issue #541: a file genuinely outside the root must still be rejected
/// even after the canonicalize fallback runs.
#[test]
fn file_is_within_root_outside_root_still_rejected_after_canonicalize() {
    use tempfile::tempdir;

    let root_dir = tempdir().unwrap();
    let canonical_root = std::fs::canonicalize(root_dir.path()).unwrap();

    // A path that is definitely outside the root.
    let outside = "/etc/passwd";
    assert!(
        !file_is_within_root(outside, &canonical_root),
        "path genuinely outside root must still be rejected"
    );
}

/// Issue #541: `search_handler` must always include `stale_index_root` in
/// the response `meta` block (as a boolean). When no results are dropped by
/// the out-of-root filter the field is `false`; we verify its presence and
/// type because the BM25 / MockEmbedder may return 0 results on a minimal
/// test index, making it hard to guarantee `true` without complex setup.
/// What: builds a minimal bare index, calls `search_handler`, and asserts the
/// `stale_index_root` field is present and boolean in the `meta` block.
/// Test: this test.
#[tokio::test]
async fn search_handler_meta_includes_stale_index_root_field() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("stale-meta-test", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));

    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("stale-meta-idx"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    registry.register(handle);

    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;

    let resp = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("stale-meta-idx".to_string()),
        axum::extract::Json(crate::core::indexer::SearchQuery {
            text: "hello".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            branch_files: None,
            branch_boost: 1.5,
            branch: None,
            stage: Some(crate::core::indexer::SearchStage::Lexical),
            mode: crate::core::indexer::SearchMode::Code,
            exclude_archived: false,
            refine_query: None,
        }),
    )
    .await;

    let Json(body) = resp.expect("handler must succeed");
    let meta = body.get("meta").expect("meta block present");

    assert!(
        meta.get("stale_index_root").is_some(),
        "meta block must contain stale_index_root field; meta={meta:?}"
    );
    assert!(
        meta["stale_index_root"].is_boolean(),
        "stale_index_root must be a boolean; got={:?}",
        meta["stale_index_root"]
    );
    // For an empty index (no chunks were added), no results can be dropped,
    // so stale_index_root must be false.
    assert_eq!(
        meta["stale_index_root"], false,
        "stale_index_root must be false when no results were dropped"
    );
}
