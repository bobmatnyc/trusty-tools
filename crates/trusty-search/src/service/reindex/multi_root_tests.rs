//! Multi-root walk and corpus-path coverage (#7434).
//!
//! Why: both properties the feature rests on fail silently on the
//! single-root code. A walk that misses a root returns fewer files, which
//! reads as an empty tree; a file under an additional root that is stored
//! ABSOLUTE (`strip_prefix(root).unwrap_or(path)`) works until the tree moves.
//! What: unit coverage over `collect_files_to_index` and the shared
//! `to_corpus_relative_path`, on real temp directories.
//! Test: this file.

use super::orchestrator::collect_files_to_index;
use super::prune::to_corpus_relative_path;
use crate::core::registry::{IndexHandle, IndexId};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A handle over `primary` with `additional` extra roots.
fn handle_with_roots(primary: &Path, additional: Vec<PathBuf>) -> Arc<IndexHandle> {
    let indexer = crate::core::indexer::CodeIndexer::new("multi-root", primary.to_path_buf());
    let mut h = IndexHandle::bare(
        IndexId::new("multi-root"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        primary.to_path_buf(),
    );
    h.additional_roots = additional;
    Arc::new(h)
}

/// Write `rel` under `root` with trivial Rust content the walker accepts.
fn write_source(root: &Path, rel: &str) -> PathBuf {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
    std::fs::write(&path, "pub fn marker() {}\n").expect("write");
    path
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Why: the walk is the feature. On the single-root walker only the primary
/// root's file is returned and the additional root contributes nothing.
/// Test: this test.
#[test]
fn walk_covers_every_index_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    write_source(primary.path(), "src/in_primary.rs");
    write_source(extra.path(), "src/in_extra.rs");

    let handle = handle_with_roots(primary.path(), vec![extra.path().to_path_buf()]);
    let walked = names(&collect_files_to_index(&handle).files);

    assert!(
        walked.iter().any(|n| n == "in_primary.rs"),
        "the primary root must still be walked; got {walked:?}"
    );
    assert!(
        walked.iter().any(|n| n == "in_extra.rs"),
        "#7434: the additional root must be walked too; got {walked:?}"
    );
    assert!(
        handle
            .walk_diagnostics
            .blocking_read()
            .missing_index_roots
            .is_empty(),
        "both roots exist, so none is reported missing"
    );
}

/// Why: an index with `path_filter` must apply it to each root against that
/// root, or every additional-root file fails the filter's `strip_prefix`.
/// Test: this test.
#[test]
fn walk_applies_path_filter_per_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    write_source(primary.path(), "keep/a.rs");
    write_source(primary.path(), "drop/b.rs");
    write_source(extra.path(), "keep/c.rs");

    let indexer = crate::core::indexer::CodeIndexer::new("pf", primary.path().to_path_buf());
    let mut h = IndexHandle::bare(
        IndexId::new("pf"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        primary.path().to_path_buf(),
    );
    h.additional_roots = vec![extra.path().to_path_buf()];
    h.path_filter = vec!["keep".into()];
    let walked = names(&collect_files_to_index(&h).files);

    assert!(walked.contains(&"a.rs".to_string()), "{walked:?}");
    assert!(walked.contains(&"c.rs".to_string()), "{walked:?}");
    assert!(!walked.contains(&"b.rs".to_string()), "{walked:?}");
}

/// Why: the corpus-safety property. On the single-root normalisation an
/// additional-root file falls through `strip_prefix` and is stored absolute.
/// What: one file per root through the function the batch loop and the prune
/// pass both call, then each key decoded back.
/// Test: this test.
#[test]
fn additional_root_file_is_stored_root_relative() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let in_primary = write_source(primary.path(), "src/in_primary.rs");
    let in_extra = write_source(extra.path(), "src/in_extra.rs");
    let additional = vec![extra.path().to_path_buf()];

    let primary_key = to_corpus_relative_path(primary.path(), &additional, &in_primary);
    assert_eq!(
        primary_key, "src/in_primary.rs",
        "the primary root's stored form must stay byte-identical to the single-root one"
    );
    let extra_key = to_corpus_relative_path(primary.path(), &additional, &in_extra);
    assert_eq!(extra_key, "@root1/src/in_extra.rs");

    let roots = crate::core::index_roots::IndexRoots::new(primary.path().to_path_buf(), additional);
    assert_eq!(roots.resolve_absolute(&primary_key), in_primary);
    assert_eq!(roots.resolve_absolute(&extra_key), in_extra);
}

/// Why: an unmounted additional root leaves the index covering fewer trees
/// than configured while the reindex still succeeds; it must be named.
/// Error arm of `walk_roots`.
/// Test: this test.
#[test]
fn walk_records_a_missing_additional_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    write_source(primary.path(), "src/in_primary.rs");
    let absent = primary.path().join("does-not-exist");

    let handle = handle_with_roots(primary.path(), vec![absent.clone()]);
    let walked = collect_files_to_index(&handle);

    assert!(
        !walked.files.is_empty(),
        "a missing ADDITIONAL root must not stop the primary walk"
    );
    assert_eq!(
        handle.walk_diagnostics.blocking_read().missing_index_roots,
        vec![absent.display().to_string()],
        "#7434: the absent root must be named"
    );
}

/// Why: the collision guard's job is that two indexes never cover one tree;
/// with multi-root indexes the claim can sit in any slot.
/// Test: this test.
#[test]
fn collision_guard_sees_additional_roots() {
    use crate::service::server::helpers::identifies_any_same_root;

    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let unrelated = tempfile::tempdir().expect("tempdir");
    let additional = vec![extra.path().to_path_buf()];

    assert!(identifies_any_same_root(
        primary.path(),
        &additional,
        primary.path()
    ));
    assert!(
        identifies_any_same_root(primary.path(), &additional, extra.path()),
        "#7434: an additional root is as claimed as the primary one"
    );
    assert!(!identifies_any_same_root(
        primary.path(),
        &additional,
        unrelated.path()
    ));
}

/// Why (#7434 review, fail-open check): an absent ADDITIONAL root contributes
/// no files to the walk, so the prune's set-difference read every one of its
/// keys as deleted and dropped them; an absent primary fails closed instead.
/// What: a corpus holding `@root1/kept.rs` (its root now gone) and
/// `gone.rs` (deleted from the live primary); the walk records the missing
/// root, and the prune keeps the first and still prunes the second.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_additional_root_keeps_its_chunks() {
    let primary = tempfile::tempdir().expect("tempdir");
    let store = tempfile::tempdir().expect("tempdir");
    let primary_path = primary.path().canonicalize().expect("canonical");
    write_source(&primary_path, "src/live.rs");
    let absent = primary_path.join("unmounted-extra");
    let mut indexer = crate::core::indexer::CodeIndexer::new("mr-missing", primary_path.clone());
    indexer.set_additional_roots(vec![absent.clone()]);
    indexer.set_corpus_store(Arc::new(
        crate::core::corpus::CorpusStore::open(&store.path().join("i.redb")).expect("corpus"),
    ));
    for key in ["@root1/kept.rs", "gone.rs", "src/live.rs"] {
        indexer
            .index_file(key, "pub fn k() {}\n")
            .await
            .expect("seed");
    }
    let mut h = IndexHandle::bare(
        IndexId::new("mr-missing"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        primary_path.clone(),
    );
    h.additional_roots = vec![absent];
    let handle = Arc::new(h);
    let walked = {
        let handle = Arc::clone(&handle);
        tokio::task::spawn_blocking(move || collect_files_to_index(&handle))
            .await
            .expect("walk")
    };
    let hashes = Arc::new(dashmap::DashMap::new());
    super::prune::prune_deleted_files_from_staging(
        &handle,
        &walked.files,
        &primary_path,
        &hashes,
        &handle.id,
    )
    .await;

    let idx = handle.indexer.read().await;
    assert!(
        !idx.chunk_ids_for_file("@root1/kept.rs").await.is_empty(),
        "#7434: an absent root's chunks must be kept, not pruned"
    );
    assert!(
        idx.chunk_ids_for_file("gone.rs").await.is_empty(),
        "control: still prunes"
    );
}
