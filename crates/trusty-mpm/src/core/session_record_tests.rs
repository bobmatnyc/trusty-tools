//! Tests for the shared per-session record store (#6972, generalized #7074).

use super::*;

/// Why: the round trip is the whole contract — a value written by the render
/// must read back in another process, under the same explicit root.
/// Test: itself.
#[test]
fn a_recorded_value_reads_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_TRANSCRIPT, "sess-1", "/tmp/s.jsonl");
    assert_eq!(
        read_session_record(dir.path(), KIND_TRANSCRIPT, "sess-1").as_deref(),
        Some("/tmp/s.jsonl")
    );
}

/// Why: `session_id` comes from Claude Code's stdin JSON, so an unchecked value
/// would let a crafted id name any file on disk.
/// Test: itself.
#[test]
fn rejects_a_path_traversal_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(
        session_record_path_in(dir.path(), KIND_MODEL, "../../etc/passwd"),
        None
    );
    record_session_value(dir.path(), KIND_MODEL, "../../evil", "x");
    assert!(
        !dir.path().parent().expect("parent").join("evil").exists(),
        "nothing may be written outside the root"
    );
}

/// Why: this runs on the hot render path, so an unchanged value must cost one
/// read and no write at all.
/// Test: itself.
#[test]
fn an_unchanged_value_leaves_the_file_untouched() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_MODEL, "sess-1", "claude-opus-4-1");
    let path = session_record_path_in(dir.path(), KIND_MODEL, "sess-1").expect("path");
    let first = std::fs::metadata(&path).expect("metadata").modified().ok();

    record_session_value(dir.path(), KIND_MODEL, "sess-1", "claude-opus-4-1");
    let second = std::fs::metadata(&path).expect("metadata").modified().ok();
    assert_eq!(
        first, second,
        "an unchanged value must not rewrite the file"
    );
}

/// Why: a blank value states nothing, and writing it would replace a good
/// record with an empty one on a payload that happened to omit the field.
/// Test: itself.
#[test]
fn a_blank_value_is_never_recorded() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_MODEL, "sess-1", "   ");
    assert_eq!(read_session_record(dir.path(), KIND_MODEL, "sess-1"), None);
}

/// Why: an absent record is the ordinary state before the first render.
/// Test: itself.
#[test]
fn reading_an_absent_record_is_none() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(read_session_record(dir.path(), KIND_MODEL, "sess-x"), None);
}

/// Why (#7074): the two kinds share one store, so a bug that dropped the kind
/// from the path would make the transcript path read back as the model id —
/// and the commit footer would then name a file as its model.
/// Test: itself.
#[test]
fn two_kinds_do_not_collide() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_MODEL, "sess-1", "claude-opus-4-1");
    record_session_value(dir.path(), KIND_TRANSCRIPT, "sess-1", "/tmp/s.jsonl");
    assert_eq!(
        read_session_record(dir.path(), KIND_MODEL, "sess-1").as_deref(),
        Some("claude-opus-4-1")
    );
    assert_eq!(
        read_session_record(dir.path(), KIND_TRANSCRIPT, "sess-1").as_deref(),
        Some("/tmp/s.jsonl")
    );
}
