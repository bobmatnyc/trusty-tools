//! Tests for the analyst instructions loader (#2340).
//!
//! Why: the loader's contract — verbatim read, loud failure on a missing file,
//! and warn-then-ignore on an empty file — is the deterministic half of the
//! instructions feature and must be pinned.
//! What: writes temp files and asserts each load outcome.
//! Test: included as `#[cfg(test)] mod tests` from `instructions.rs`.

use super::load_instructions;
use crate::report::error::ReportError;

/// Why: a present, non-empty brief must load verbatim (trailing whitespace only
/// trimmed) with its source path recorded.
/// What: writes a brief and asserts the text and source.
/// Test: this test itself.
#[test]
fn load_reads_verbatim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("brief.md");
    std::fs::write(&path, "# Focus\n\nAssess auth and data retention.\n").expect("write");

    let loaded = load_instructions(&path).expect("ok").expect("some");
    assert_eq!(loaded.text, "# Focus\n\nAssess auth and data retention.");
    assert_eq!(loaded.source, path);
}

/// Why: a mistyped path must fail loudly, not silently produce a report with no
/// recorded focus.
/// What: asserts a missing file maps to `InstructionsNotFound`.
/// Test: this test itself.
#[test]
fn missing_file_errors() {
    let err = load_instructions(std::path::Path::new("/nonexistent/brief.md"))
        .expect_err("missing must error");
    assert!(matches!(err, ReportError::InstructionsNotFound { .. }));
}

/// Why: an empty brief is tolerated — the analyst may stub the file — and must
/// proceed as if absent (a warning, not an error).
/// What: writes a whitespace-only file and asserts `Ok(None)`.
/// Test: this test itself.
#[test]
fn empty_file_is_ignored() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("empty.md");
    std::fs::write(&path, "   \n\n\t\n").expect("write");
    assert!(load_instructions(&path).expect("ok").is_none());
}
