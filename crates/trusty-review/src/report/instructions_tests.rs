//! Tests for the analyst instructions loader (#2340).
//!
//! Why: the loader's contract — verbatim read, loud failure on a missing file,
//! and warn-then-ignore on an empty file — is the deterministic half of the
//! instructions feature and must be pinned.
//! What: writes temp files and asserts each load outcome.
//! Test: included as `#[cfg(test)] mod tests` from `instructions.rs`.

use super::{MANIFEST_INSTRUCTIONS_FILE, discover_manifest_instructions, load_instructions};
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

// ─── #6180: instructions.md discovered beside the manifest ───────────────────

/// Why: the engagement drops `instructions.md` next to `manifest.toml` and
/// declares nothing; the file must be found and read verbatim.
/// What: writes both files and asserts the brief loads with the discovered path
/// as its source.
/// Test: this test itself.
#[test]
fn a_discovered_file_is_loaded() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest = tmp.path().join("manifest.toml");
    std::fs::write(&manifest, "[report]\ntitle = \"T\"\n").expect("write manifest");
    let instructions = tmp.path().join(MANIFEST_INSTRUCTIONS_FILE);
    std::fs::write(&instructions, "Weigh data retention above all else.\n").expect("write");

    let loaded = discover_manifest_instructions(&manifest)
        .expect("ok")
        .expect("some");
    assert_eq!(loaded.text, "Weigh data retention above all else.");
    assert_eq!(loaded.source, instructions);
}

/// Why: no `instructions.md` is the normal case for every engagement written
/// before #6180 and must change nothing.
/// What: asserts a manifest directory without the file yields `Ok(None)`.
/// Test: this test itself.
#[test]
fn an_absent_discovered_file_is_none() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest = tmp.path().join("manifest.toml");
    std::fs::write(&manifest, "[report]\ntitle = \"T\"\n").expect("write manifest");
    assert!(
        discover_manifest_instructions(&manifest)
            .expect("ok")
            .is_none()
    );
}

/// Why: the fail-open doctrine covers an absent input, never a present one —
/// silently skipping a file the author dropped there would render a report they
/// believe carries their instructions and does not.
/// What: writes non-UTF-8 bytes to `instructions.md` and asserts the run fails
/// with [`ReportError::InstructionsUnreadable`], naming the path.
///
/// 🔴 Fail-open regression guard: against the pre-#6180 binary this file was
/// never opened at all, so this case returned no error and the render proceeded.
/// Test: this test itself.
#[test]
fn an_unreadable_discovered_file_is_a_hard_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest = tmp.path().join("manifest.toml");
    std::fs::write(&manifest, "[report]\ntitle = \"T\"\n").expect("write manifest");
    // Invalid UTF-8: a lone continuation byte. The file exists and has content;
    // only decoding it fails.
    std::fs::write(
        tmp.path().join(MANIFEST_INSTRUCTIONS_FILE),
        [0x66, 0x80, 0x66],
    )
    .expect("write");

    let err = discover_manifest_instructions(&manifest).expect_err("present-but-unreadable errors");
    let ReportError::InstructionsUnreadable { path, .. } = &err else {
        panic!("expected InstructionsUnreadable, got {err:?}");
    };
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some(MANIFEST_INSTRUCTIONS_FILE)
    );
}

/// Why: `--manifest` is often passed from another working directory, so
/// discovery must key off the manifest FILE's parent, never the process cwd.
/// What: puts the manifest and its `instructions.md` in a subdirectory and
/// asserts a sibling directory's file is not picked up.
/// Test: this test itself.
#[test]
fn a_discovered_file_is_resolved_against_the_manifest_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let engagement = tmp.path().join("engagement");
    std::fs::create_dir(&engagement).expect("mkdir");
    let manifest = engagement.join("manifest.toml");
    std::fs::write(&manifest, "[report]\ntitle = \"T\"\n").expect("write manifest");
    // A decoy one level up must not be read.
    std::fs::write(tmp.path().join(MANIFEST_INSTRUCTIONS_FILE), "decoy\n").expect("write decoy");

    assert!(
        discover_manifest_instructions(&manifest)
            .expect("ok")
            .is_none()
    );

    std::fs::write(engagement.join(MANIFEST_INSTRUCTIONS_FILE), "real\n").expect("write real");
    let loaded = discover_manifest_instructions(&manifest)
        .expect("ok")
        .expect("some");
    assert_eq!(loaded.text, "real");
}

/// Why: an empty stub file must behave exactly like an absent one, matching the
/// declared-path loader rather than inventing a second contract.
/// What: asserts a whitespace-only `instructions.md` yields `Ok(None)`.
/// Test: this test itself.
#[test]
fn an_empty_discovered_file_is_ignored() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest = tmp.path().join("manifest.toml");
    std::fs::write(&manifest, "[report]\ntitle = \"T\"\n").expect("write manifest");
    std::fs::write(tmp.path().join(MANIFEST_INSTRUCTIONS_FILE), "  \n\t\n").expect("write");
    assert!(
        discover_manifest_instructions(&manifest)
            .expect("ok")
            .is_none()
    );
}
