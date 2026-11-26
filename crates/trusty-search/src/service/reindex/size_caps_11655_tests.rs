//! Configurable size caps (#11655).
//!
//! Why: the source (1 MiB) and office/PDF (10 MiB) caps were constants; the
//! #11655 makes both settable per index, with the defaults
//! unchanged and a hard ceiling. A cap raised at one gate and still enforced
//! at the old value at another is the defect these tests guard against.
//! What: a raised source cap indexes a file over the old default end to end;
//! a file over the raised cap is still recorded as `ExceedsSizeLimit`; the
//! watcher gate, extraction and persistence honour the configured value; an
//! out-of-range value is refused or falls back to the default.
//! Test: this file.

use super::orchestrator::{collect_files_to_index, spawn_reindex_awaitable};
use super::{ReindexProgress, ReindexStatus};
use crate::core::registry::{IndexHandle, IndexId, IndexStages, SkipReason, StageState};
use crate::service::size_caps::{validate_size_cap, FileSizeCaps, MAX_CONFIGURABLE_FILE_BYTES};
use crate::service::walker::MAX_FILE_BYTES;
use std::path::Path;
use std::sync::Arc;

const MIB: u64 = 1024 * 1024;

/// A lexical-only handle over `root` with `caps`, so a reindex needs no embedder.
fn handle_with_caps(id: &str, root: &Path, caps: FileSizeCaps) -> Arc<IndexHandle> {
    let indexer = crate::core::indexer::CodeIndexer::new(id, root.to_path_buf());
    let mut handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    );
    handle.size_caps = caps;
    handle.lexical_only = true;
    handle.defer_embed = false;
    handle.include_docs = false;
    handle.stages = Arc::new(tokio::sync::RwLock::new(IndexStages {
        lexical: StageState::pending(),
        semantic: StageState::skipped(),
        graph: StageState::skipped(),
    }));
    Arc::new(handle)
}

/// A Rust source file of at least `len` bytes whose first line names `marker`.
fn write_rust_file(path: &Path, marker: &str, len: u64) -> u64 {
    let mut body = format!("pub fn {marker}() {{}}\n");
    let line = "// padding line for the size cap test\n";
    while (body.len() as u64) < len {
        body.push_str(line);
    }
    std::fs::write(path, &body).expect("write");
    body.len() as u64
}

/// AC2: with nothing configured the caps are the pre-#11655 constants.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn default_caps_are_unchanged() {
    let caps = FileSizeCaps::default();
    assert_eq!(caps.source, MIB);
    assert_eq!(caps.office, 10 * MIB);
    assert_eq!(caps.for_ext("rs"), MIB);
    assert_eq!(caps.for_ext("pdf"), 10 * MIB);
    assert_eq!(caps.overrides(), (None, None));
    assert_eq!(FileSizeCaps::resolve(None, None), caps);
}

/// AC3 + AC4: a 1.5 MiB source file is indexed under a 2 MiB cap, and a
/// 3 MiB one is still recorded in `skipped_files` as `ExceedsSizeLimit`.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn raised_source_cap_indexes_a_file_over_the_old_default() {
    let root = tempfile::tempdir().expect("tempdir");
    let mid = root.path().join("mid.rs");
    let mid_len = write_rust_file(&mid, "size_cap_marker_11655", MAX_FILE_BYTES + MIB / 2);
    assert!(
        mid_len > MAX_FILE_BYTES,
        "fixture must exceed the old default"
    );
    let huge = root.path().join("huge.rs");
    let huge_len = write_rust_file(&huge, "over_raised_cap_11655", 3 * MIB);

    let caps = FileSizeCaps {
        source: 2 * MIB,
        office: 10 * MIB,
    };
    let handle = handle_with_caps("size-caps-11655", root.path(), caps);

    // `collect_files_to_index` takes a blocking lock, as the reindex runner's
    // blocking pool does.
    let walk_handle = Arc::clone(&handle);
    let walked = tokio::task::spawn_blocking(move || collect_files_to_index(&walk_handle))
        .await
        .expect("walk task");
    assert!(
        walked.files.iter().any(|f| f.ends_with("mid.rs")),
        "a file under the raised cap must be walked: {:?}",
        walked.files
    );
    assert!(walked.files.iter().all(|f| !f.ends_with("huge.rs")));
    let diag = handle.walk_diagnostics.read().await.clone();
    assert_eq!(diag.skipped_for_size_total, 1, "{:?}", diag.skipped_files);
    let rec = &diag.skipped_files[0];
    assert!(rec.path.ends_with("huge.rs"), "path {}", rec.path);
    assert_eq!(rec.size_bytes, huge_len);
    assert_eq!(rec.reason, SkipReason::ExceedsSizeLimit);

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    let idx = handle.indexer.read().await;
    let results = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "size_cap_marker_11655".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            stage: Some(crate::core::indexer::SearchStage::Lexical),
            ..Default::default()
        })
        .await
        .expect("search");
    assert!(
        results.iter().any(|c| c.file.ends_with("mid.rs")),
        "the file over the old default must be indexed: {results:?}"
    );
}

/// The watcher's post-admission gate reads the index's caps, not the constants.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn watcher_gate_honours_a_raised_source_cap() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("saved.rs");
    write_rust_file(&path, "saved_11655", MAX_FILE_BYTES + 1);
    let raised = FileSizeCaps {
        source: 2 * MIB,
        ..FileSizeCaps::default()
    };
    assert!(crate::service::walker::should_skip_path(&path));
    assert!(!crate::service::walker::should_skip_path_with_caps(
        &path, raised
    ));
}

/// Extraction refuses by the configured office cap, not the 10 MiB constant.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn raised_office_cap_is_honoured_by_extraction() {
    use crate::core::extract::{extract_text, extract_text_with_cap, ExtractError};
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("big.pdf");
    std::fs::write(&path, vec![0u8; (11 * MIB) as usize]).expect("write");

    let default_err = extract_text(&path).expect_err("over the default cap");
    assert!(matches!(default_err, ExtractError::TooLarge { .. }));
    let raised = extract_text_with_cap(&path, 16 * MIB);
    assert!(
        !matches!(raised, Err(ExtractError::TooLarge { .. })),
        "a raised office cap must not refuse an 11 MiB document by size: {raised:?}"
    );
    let lowered = extract_text_with_cap(&path, MIB).expect_err("over a lowered cap");
    assert!(matches!(lowered, ExtractError::TooLarge { cap, .. } if cap == MIB));
}

/// 0 and values above the ceiling are refused; the ceiling itself is legal.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn size_cap_validation_refuses_zero_and_over_ceiling() {
    assert!(validate_size_cap("source_file_max_bytes", None).is_ok());
    assert!(validate_size_cap("source_file_max_bytes", Some(1)).is_ok());
    assert!(validate_size_cap("office_file_max_bytes", Some(MAX_CONFIGURABLE_FILE_BYTES)).is_ok());
    let zero = validate_size_cap("source_file_max_bytes", Some(0)).expect_err("0");
    assert!(zero.contains("source_file_max_bytes"), "{zero}");
    let over = validate_size_cap(
        "office_file_max_bytes",
        Some(MAX_CONFIGURABLE_FILE_BYTES + 1),
    )
    .expect_err("over the ceiling");
    assert!(over.contains("office_file_max_bytes"), "{over}");
}

/// A hand-edited out-of-range value in `indexes.toml` falls back to the default.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn resolve_falls_back_to_the_default_for_an_invalid_stored_cap() {
    let caps = FileSizeCaps::resolve(Some(0), Some(u64::MAX));
    assert_eq!(caps, FileSizeCaps::default());
    let caps = FileSizeCaps::resolve(Some(4 * MIB), None);
    assert_eq!(caps.source, 4 * MIB);
    assert_eq!(caps.office, 10 * MIB);
    assert_eq!(caps.overrides(), (Some(4 * MIB), None));
}

/// The overrides survive a TOML round trip; an unset cap writes nothing.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn size_caps_round_trip_through_indexes_toml() {
    use crate::service::persistence::PersistedIndex;
    let entry = PersistedIndex {
        id: "caps".into(),
        source_file_max_bytes: Some(2 * MIB),
        office_file_max_bytes: Some(20 * MIB),
        ..PersistedIndex::default()
    };
    let text = toml::to_string(&entry).expect("serialize");
    let back: PersistedIndex = toml::from_str(&text).expect("parse");
    assert_eq!(back.source_file_max_bytes, Some(2 * MIB));
    assert_eq!(back.office_file_max_bytes, Some(20 * MIB));

    let plain = toml::to_string(&PersistedIndex::default()).expect("serialize");
    assert!(!plain.contains("source_file_max_bytes"), "{plain}");
    assert!(!plain.contains("office_file_max_bytes"), "{plain}");
}
