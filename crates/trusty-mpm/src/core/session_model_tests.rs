//! Tests for the per-session parent-model record (#6972).
//!
//! Why: this store is the only thing standing between an Opus session and a
//! divert row priced at Sonnet, and it runs on Claude Code's hot render path.
//! Both halves need pinning: that a written id comes back out, and that the
//! steady state writes nothing.
//! What: every case runs against a tempdir root, so no test touches the
//! operator's real framework root and none mutates the environment.
//! Test: this file.

use super::*;

/// Why (#6972 closure condition 1): the whole feature is "the render writes it,
/// the diversion reads it". If that round trip does not hold, nothing else here
/// matters.
/// Test: itself.
#[test]
fn a_recorded_model_reads_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_model(dir.path(), "sess-a", "claude-opus-4-5-20260101");
    assert_eq!(
        read_session_model(dir.path(), "sess-a").as_deref(),
        Some("claude-opus-4-5-20260101")
    );
    // The record is per session: another id reads nothing from the same root.
    assert_eq!(read_session_model(dir.path(), "sess-b"), None);
}

/// Why: the session id comes off Claude Code's stdin JSON, so it is
/// attacker-influenceable. A traversal must produce no path at all rather than
/// a path outside the framework root.
/// Test: itself.
#[test]
fn rejects_a_path_traversal_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    for bad in ["", "../../.bashrc", "/tmp/evil", "sess/a", "sess.a"] {
        assert_eq!(
            session_model_path_in(dir.path(), bad),
            None,
            "{bad:?} must not resolve to a path"
        );
        record_session_model(dir.path(), bad, "claude-opus-4-5");
        assert_eq!(read_session_model(dir.path(), bad), None);
    }
    assert!(
        !dir.path().join("usage").exists(),
        "a rejected id must not create the store directory"
    );
}

/// Why (#6972): `statusLine` fires on every render cycle. An implementation that
/// wrote unconditionally would do a create-write-rename per render forever, and
/// the issue's own instruction is to keep the hot path cheap. Asserting on the
/// file's modified time is what distinguishes "wrote the same bytes again" from
/// "did not write".
/// Test: itself.
#[test]
fn an_unchanged_model_leaves_the_file_untouched() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_model(dir.path(), "sess-a", "claude-opus-4-5");
    let path = session_model_path_in(dir.path(), "sess-a").expect("a path");
    let first = std::fs::metadata(&path).expect("metadata").modified().ok();

    record_session_model(dir.path(), "sess-a", "claude-opus-4-5");
    let second = std::fs::metadata(&path).expect("metadata").modified().ok();
    assert_eq!(
        first, second,
        "an unchanged model must not rewrite the file"
    );

    // A genuine change does write, and the new value is what reads back.
    record_session_model(dir.path(), "sess-a", "claude-haiku-4-5");
    assert_eq!(
        read_session_model(dir.path(), "sess-a").as_deref(),
        Some("claude-haiku-4-5")
    );
}

/// Why (#6972): Claude Code sends `model.id` as `""` before the session settles.
/// Recording that would replace a good id with a blank one and silently send the
/// next diversion back to the config-chain guess.
/// Test: itself.
#[test]
fn a_blank_model_is_never_recorded() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_model(dir.path(), "sess-a", "claude-opus-4-5");
    for blank in ["", "   ", "\n"] {
        record_session_model(dir.path(), "sess-a", blank);
        assert_eq!(
            read_session_model(dir.path(), "sess-a").as_deref(),
            Some("claude-opus-4-5"),
            "a blank model must not overwrite a recorded one"
        );
    }
}

/// Why (#6972 closure condition 2): before any render has happened the store is
/// simply absent, and that is the ordinary state — it must read as `None`
/// without erroring, so the resolver can fall through to the config chain.
/// Test: itself.
#[test]
fn reading_an_absent_record_is_none() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(read_session_model(dir.path(), "sess-a"), None);
    // A record holding only whitespace is indistinguishable from no record.
    let path = session_model_path_in(dir.path(), "sess-a").expect("a path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, "   \n").expect("write");
    assert_eq!(read_session_model(dir.path(), "sess-a"), None);
}
