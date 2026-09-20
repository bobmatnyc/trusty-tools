//! Multi-root walk and corpus-path coverage (#7434).
//!
//! Why: two properties carry the whole feature, and both fail silently against
//! the pre-#7434 code rather than erroring. The walk must reach every index
//! root — a single-root walk simply returns fewer files, which looks like an
//! empty tree. And a file under an ADDITIONAL root must be stored RELATIVE to
//! its own root — the naive walk stores it ABSOLUTE, via
//! `strip_prefix(root).unwrap_or(path)`, which still "works" until the tree
//! moves and every one of those chunks becomes unresolvable.
//!
//! What: unit coverage over `collect_files_to_index` and the shared
//! `to_corpus_relative_path` normalisation, built on real temp directories so
//! the existence checks and `strip_prefix` behave as they do in production.
//!
//! Test: this file.

use super::orchestrator::collect_files_to_index;
use super::prune::to_corpus_relative_path;
use crate::core::registry::{IndexHandle, IndexId};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Build a handle over `primary` with `additional` extra roots.
///
/// Why: `IndexHandle::bare` covers the single-root shape; multi-root needs the
/// one extra assignment, and doing it in one place keeps each test to its
/// subject.
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

/// Write `rel` under `root` with trivial Rust content the walker will accept.
fn write_source(root: &Path, rel: &str) -> PathBuf {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
    std::fs::write(&path, "pub fn marker() {}\n").expect("write");
    path
}

/// Why: the walk is the feature. Against the pre-#7434 walker this fails —
/// only the primary root's file is returned, and the additional root
/// contributes nothing at all.
/// What: two real trees, one file each, one handle spanning both.
/// Test: this test.
#[test]
fn walk_covers_every_index_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    write_source(primary.path(), "src/in_primary.rs");
    write_source(extra.path(), "src/in_extra.rs");

    let handle = handle_with_roots(primary.path(), vec![extra.path().to_path_buf()]);
    let collected = collect_files_to_index(&handle);
    let names: Vec<String> = collected
        .walk
        .files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();

    assert!(
        names.iter().any(|n| n == "in_primary.rs"),
        "primary root must still be walked; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "in_extra.rs"),
        "#7434: the additional root must be walked too; got {names:?}"
    );
    assert!(
        collected.missing_roots.is_empty(),
        "both roots exist, so nothing is missing; got {:?}",
        collected.missing_roots
    );
}

/// Why: this is the corpus-safety property. Against the pre-#7434
/// normalisation the additional-root file falls through `strip_prefix` and is
/// stored ABSOLUTE, silently giving up the #402 relocation resilience for
/// exactly the files the feature added — and the assertion below on the
/// leading `/` is what catches it.
/// What: relativises one file from each root through the SAME function the
/// batch loop and the prune pass both call, then resolves each back.
/// Test: this test.
#[test]
fn additional_root_file_is_stored_root_relative() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let in_primary = write_source(primary.path(), "src/in_primary.rs");
    let in_extra = write_source(extra.path(), "src/in_extra.rs");

    let roots = crate::core::index_roots::IndexRoots::new(
        primary.path().to_path_buf(),
        vec![extra.path().to_path_buf()],
    );

    let primary_key = to_corpus_relative_path(&roots, &in_primary);
    assert_eq!(
        primary_key, "src/in_primary.rs",
        "#7434: the primary root's stored form must be byte-identical to the \
         single-root one, or every existing corpus needs rewriting"
    );

    let extra_key = to_corpus_relative_path(&roots, &in_extra);
    assert!(
        !extra_key.starts_with('/'),
        "#7434: an additional-root file must be stored root-relative, not \
         absolute — this is the naive-walk failure; got {extra_key:?}"
    );
    assert!(
        extra_key.ends_with("src/in_extra.rs"),
        "the stored key must still name the file; got {extra_key:?}"
    );

    // Both keys must decode back to the file they came from, or a search hit
    // hands the caller a path it cannot open.
    assert_eq!(roots.resolve_absolute(&primary_key), in_primary);
    assert_eq!(roots.resolve_absolute(&extra_key), in_extra);
}

/// Why: an unmounted or deleted additional root leaves the index covering
/// fewer trees than configured while still reporting a successful reindex.
/// Naming the absent root is what makes that diagnosable instead of looking
/// like an empty tree.
/// What: a handle whose additional root does not exist; the primary root's
/// file must still be walked, and the absent root must be reported.
/// Test: this test.
#[test]
fn walk_records_a_missing_additional_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    write_source(primary.path(), "src/in_primary.rs");
    let absent = primary.path().join("does-not-exist");

    let handle = handle_with_roots(primary.path(), vec![absent.clone()]);
    let collected = collect_files_to_index(&handle);

    assert_eq!(
        collected.missing_roots,
        vec![absent],
        "#7434: an absent additional root must be named, not silently skipped"
    );
    assert!(
        !collected.walk.files.is_empty(),
        "a missing ADDITIONAL root degrades that root's coverage only — the \
         primary root must still be walked"
    );
}

/// Why: the collision guard's whole job is that two indexes never cover one
/// tree, and with multi-root indexes the claim can sit in any slot. A guard
/// that only reads `root_path` lets a second index register over a tree the
/// first already indexes, which is the #2305 shared-corpus hazard.
/// What: the any-of-N primitive the live-handle and cold-entry arms share.
/// Test: this test.
#[test]
fn collision_guard_sees_additional_roots() {
    use crate::service::server::helpers::identifies_any_same_root;

    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let unrelated = tempfile::tempdir().expect("tempdir");
    let additional = vec![extra.path().to_path_buf()];

    assert!(
        identifies_any_same_root(primary.path(), &additional, primary.path()),
        "the primary root must still collide with itself"
    );
    assert!(
        identifies_any_same_root(primary.path(), &additional, extra.path()),
        "#7434: an additional root is as claimed as the primary one"
    );
    assert!(
        !identifies_any_same_root(primary.path(), &additional, unrelated.path()),
        "an unrelated tree must not collide"
    );
}
