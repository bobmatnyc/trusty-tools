//! Size-skip visibility (#11651).
//!
//! Why: a file over the walker's size cap was dropped with no record, so
//! large documents were silently unsearchable.
//! What: the walk records each refused file with its size and reason, capped
//! at `MAX_SKIPPED_FILE_RECORDS` entries plus a full total.
//! Test: this file.

use super::orchestrator::collect_files_to_index;
use crate::core::registry::{
    IndexHandle, IndexId, SkipReason, WalkDiagnostics, MAX_SKIPPED_FILE_RECORDS,
};
use crate::service::walker::MAX_FILE_BYTES;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn handle_for(root: &Path) -> Arc<IndexHandle> {
    let indexer = crate::core::indexer::CodeIndexer::new("size-skip", root.to_path_buf());
    Arc::new(IndexHandle::bare(
        IndexId::new("size-skip"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ))
}

/// Why: AC3 of #11651 — the skip must be recorded with its reason and size.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn oversized_file_is_recorded_with_reason_and_size() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join("small.rs"), "pub fn small() {}\n").expect("write");
    let big_len = MAX_FILE_BYTES + 1;
    let line = "// padding\n";
    let big: String = line.repeat((big_len as usize).div_ceil(line.len()));
    let big_path = root.path().join("big.rs");
    std::fs::write(&big_path, &big).expect("write big");

    let handle = handle_for(root.path());
    let walked = collect_files_to_index(&handle);
    assert!(walked.files.iter().all(|f| !f.ends_with("big.rs")));
    assert!(walked.files.iter().any(|f| f.ends_with("small.rs")));

    let diag = handle.walk_diagnostics.blocking_read().clone();
    assert_eq!(diag.skipped_for_size_total, 1);
    assert_eq!(diag.skipped_files.len(), 1, "got {:?}", diag.skipped_files);
    let rec = &diag.skipped_files[0];
    assert!(rec.path.ends_with("big.rs"), "path {}", rec.path);
    assert!(Path::new(&rec.path).is_absolute(), "path {}", rec.path);
    assert_eq!(rec.size_bytes, big.len() as u64);
    assert_eq!(rec.reason, SkipReason::ExceedsSizeLimit);
    let wire = serde_json::to_value(rec).expect("serialize");
    assert_eq!(wire["reason"], "exceeds_size_limit");
}

/// Why: a tree with thousands of oversized files must not bloat the status
/// body; the total still counts every one.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn size_skips_are_capped_with_a_full_total() {
    let oversized: Vec<(PathBuf, u64)> = (0..150u64)
        .map(|i| (PathBuf::from(format!("/r/f{i:03}.pdf")), i))
        .collect();
    let mut diag = WalkDiagnostics::default();
    diag.record_size_skips(&oversized);
    assert_eq!(diag.skipped_for_size_total, 150);
    assert_eq!(diag.skipped_files.len(), MAX_SKIPPED_FILE_RECORDS);
    assert_eq!(diag.skipped_files[0].path, "/r/f000.pdf");

    diag.record_size_skips(&[]);
    assert_eq!(
        diag.skipped_for_size_total, 0,
        "a new walk replaces the record"
    );
    assert!(diag.skipped_files.is_empty());
}
