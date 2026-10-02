//! Tests for `service::write_admission` (#8922).
//!
//! What: the pushed-write decision against paths on disk and off it, the size
//! caps, and the two arms that must fail closed.
//! Test: `cargo test -p trusty-search -- write_admission`.

use super::*;
use crate::core::registry::IndexId;
use std::sync::Arc;
use tokio::sync::RwLock;

fn fixture(id: &str) -> (tempfile::TempDir, IndexHandle) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let indexer = Arc::new(RwLock::new(CodeIndexer::new(id, &root)));
    let mut handle = IndexHandle::bare(IndexId::new(id), indexer, root);
    handle.exclude_globs = vec!["**/secrets/**".into()];
    (temp, handle)
}

/// #8922: a write to a path the walker skips is refused with 403 and the
/// chunks an earlier write left for it are removed; an admitted path passes.
/// The table covers each rule that needs no directory listing. Fails with
/// `gate` returning `Ok(())`, which is what `index_file` did before.
#[tokio::test]
async fn pushed_write_to_an_excluded_path_is_refused_and_purged() {
    let (_temp, handle) = fixture("wa-excluded");
    let indexer = handle.indexer.read().await;
    indexer
        .index_file("secrets/prod.yaml", "password: hunter2\n")
        .await
        .unwrap();

    let (status, body) = gate(&handle, &indexer, "secrets/prod.yaml", "password: x\n")
        .await
        .expect_err("an excluded path is refused");
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "index_file_excluded");
    assert_eq!(body["reason"], "excluded_path");
    assert_eq!(body["indexed"], false);
    assert!(body["removed_chunks"].as_u64().unwrap_or(0) > 0, "{body}");
    assert!(indexer
        .chunk_ids_for_file("secrets/prod.yaml")
        .await
        .is_empty());

    for path in [
        "data/export.json",          // default extra skip dir
        "node_modules/pkg/index.js", // built-in skip dir
        "src/notes.xyz",             // not a source extension
        "dist/app.min.js",           // skip dir and minified name
        "Cargo.lock",                // lock file
        "../outside.rs",             // escapes the root
    ] {
        assert_eq!(
            admits_pushed(&handle, path, 10),
            Admission::Excluded,
            "{path}"
        );
    }
    let mut narrowed = IndexHandle::bare(
        handle.id.clone(),
        handle.indexer.clone(),
        handle.root_path.clone(),
    );
    narrowed.extensions = vec!["rs".into()];
    assert_eq!(
        admits_pushed(&narrowed, "src/app.py", 10),
        Admission::Excluded
    );
    assert_eq!(
        admits_pushed(&handle, "src/lib.rs", 10),
        Admission::Included
    );
    assert!(gate(&handle, &indexer, "src/lib.rs", "pub fn ok() {}\n")
        .await
        .is_ok());
}

/// #8922: a file on disk is judged by the walker itself, ignore files included,
/// and a tombstone write — which only removes — is never refused.
#[tokio::test]
async fn pushed_write_on_disk_honours_ignore_files() {
    let (_temp, handle) = fixture("wa-ignored");
    std::fs::write(handle.root_path.join(".gitignore"), "ignored.rs\n").unwrap();
    std::fs::write(handle.root_path.join("ignored.rs"), "fn x() {}\n").unwrap();
    assert_eq!(
        admits_pushed(&handle, "ignored.rs", 10),
        Admission::Excluded
    );
    let indexer = handle.indexer.read().await;
    let tombstone = "---\nsource_id: s\nsource_status: deleted\n---\nbody";
    assert!(gate(&handle, &indexer, "secrets/gone.md", tombstone)
        .await
        .is_ok());
}

/// #8922: content over the walker's size caps is refused even though no file
/// on disk carries that size. Fails with the cap check removed.
#[test]
fn pushed_write_over_a_size_cap_is_refused() {
    let (_temp, handle) = fixture("wa-size");
    let data_cap = handle.data_file_max_bytes as usize;
    assert_eq!(
        admits_pushed(&handle, "config/big.json", data_cap + 1),
        Admission::Excluded
    );
    assert_eq!(
        admits_pushed(&handle, "config/small.json", data_cap),
        Admission::Included
    );
    assert_eq!(
        admits_pushed(&handle, "src/huge.rs", walker::MAX_FILE_BYTES as usize + 1),
        Admission::Excluded
    );
}

/// #8922 fail-open check: a path whose presence on disk cannot be read is
/// refused with a retryable 503 and removes nothing. Two fixtures: a
/// self-referential symlink (`ELOOP` inside `admits`) and a path through a
/// regular file (`ENOTDIR` from `symlink_metadata`). Fails with the `Err(_)`
/// arm of `admits_pushed` falling through to the lexical rules, which admit
/// both paths.
#[cfg(unix)]
#[tokio::test]
async fn pushed_write_the_filesystem_cannot_resolve_is_refused() {
    let (_temp, handle) = fixture("wa-undetermined");
    let root = handle.root_path.clone();
    std::os::unix::fs::symlink("loop.rs", root.join("loop.rs")).unwrap();
    std::fs::write(root.join("file.rs"), "fn f() {}\n").unwrap();
    assert_eq!(
        admits_pushed(&handle, "loop.rs", 10),
        Admission::Undetermined
    );
    assert_eq!(
        admits_pushed(&handle, "file.rs/inner.rs", 10),
        Admission::Undetermined
    );

    let indexer = handle.indexer.read().await;
    indexer
        .index_file("loop.rs", "pub fn kept() {}\n")
        .await
        .unwrap();
    let (status, body) = gate(&handle, &indexer, "loop.rs", "pub fn kept() {}\n")
        .await
        .expect_err("an undecidable path is refused");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["reason"], "admission_undetermined");
    assert_eq!(body["retryable"], true);
    assert!(
        !indexer.chunk_ids_for_file("loop.rs").await.is_empty(),
        "an undecidable admission must never remove chunks"
    );
}
