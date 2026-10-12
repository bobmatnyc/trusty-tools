//! Unit tests for the #8433 P0 taint contract.

use super::*;
use std::os::unix::fs::PermissionsExt as _;

fn mark() -> TaintMark {
    TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: Utc::now(),
        workspace: std::path::PathBuf::from("/"),
    }
}

fn dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = taint_dir(tmp.path());
    (tmp, dir)
}

/// 🔴 #8433: the file is named for the session, mode 0600, complete JSON, and
/// no temp file is left beside it.
#[test]
fn taint_write_is_atomic_0600_and_names_session() {
    let (_tmp, dir) = dir();
    let path = write_taint(&dir, &mark().record_for("abc-123", "m-1")).expect("write");
    assert_eq!(path, dir.join("abc-123.json"));
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "taint file mode");
    let record: TaintRecord =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    assert_eq!((record.v, record.session_id.as_str()), (1, "abc-123"));
    assert_eq!(record.managed_session, "m-1");
    let names: Vec<_> = std::fs::read_dir(&dir)
        .expect("ls")
        .map(|e| e.expect("e").file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("abc-123.json")],
        "a temp file was left"
    );
}

/// 🔴 #8433: a write that cannot land is an `Err`, never a silent success.
#[test]
fn taint_write_failure_returns_err() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let blocked = tmp.path().join("taint");
    std::fs::write(&blocked, "a file where the directory should be").expect("write");
    let err = write_taint(&blocked, &mark().record_for("s1", "m")).expect_err("must fail");
    assert!(matches!(err, TaintError::Io { .. }), "{err}");
    let (_t, dir) = dir();
    std::fs::create_dir_all(dir.join("s2.json").join("occupied")).expect("dir at the target");
    assert!(
        write_taint(&dir, &mark().record_for("s2", "m")).is_err(),
        "rename over a dir"
    );
    assert!(
        mark_managed(&blocked, "m", &mark()).is_err(),
        "the mark write fails too"
    );
}

/// 🔴 #8433: a file that exists but cannot be read is tainted.
#[test]
fn unreadable_taint_reads_as_tainted() {
    let (_tmp, dir) = dir();
    let path = write_taint(&dir, &mark().record_for("s1", "m")).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let state = read_state(&dir, "s1");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod back");
    assert_eq!(state, TaintState::Tainted(TaintReason::Unreadable));
    std::fs::create_dir_all(dir.join("s2.json")).expect("dir at the file path");
    assert!(
        read_state(&dir, "s2").is_tainted(),
        "a directory at the path is tainted"
    );
}

/// 🔴 #8433: content that is not a record — garbage, empty, truncated — is tainted.
#[test]
fn malformed_taint_reads_as_tainted() {
    let (_tmp, dir) = dir();
    std::fs::create_dir_all(&dir).expect("mkdir");
    for (id, body) in [("g", "not json"), ("e", ""), ("t", "{\"v\":1,\"sess")] {
        std::fs::write(dir.join(format!("{id}.json")), body).expect("write");
        assert_eq!(
            read_state(&dir, id),
            TaintState::Tainted(TaintReason::Malformed),
            "{id}"
        );
    }
}

/// 🔴 #8433: only a provably absent file is untainted — also with no directory.
#[test]
fn absent_taint_reads_untainted() {
    let (_tmp, dir) = dir();
    assert_eq!(read_state(&dir, "nobody"), TaintState::Untainted);
    write_taint(&dir, &mark().record_for("other", "m")).expect("write");
    assert_eq!(read_state(&dir, "nobody"), TaintState::Untainted);
    assert!(read_state(&dir, "other").is_tainted());
}

/// 🔴 #8433: an id that could leave the directory never builds a path.
#[test]
fn session_id_with_slash_or_dotdot_is_refused() {
    let (tmp, dir) = dir();
    let long = "a".repeat(129);
    for id in [
        "../x",
        "a/b",
        "..",
        ".",
        "",
        "a\0b",
        "x.json",
        "~",
        long.as_str(),
    ] {
        assert!(sanitize_id(id).is_err(), "{id:?} accepted");
        assert!(taint_path(&dir, id).is_err(), "{id:?} built a path");
        assert_eq!(
            read_state(&dir, id),
            TaintState::Tainted(TaintReason::BadId),
            "{id:?}"
        );
        assert!(
            write_taint(&dir, &mark().record_for(id, "m")).is_err(),
            "{id:?} written"
        );
        assert!(reapply(&dir, "m", id).is_err() || managed_mark(&dir, "m").expect("m").is_none());
    }
    assert!(
        !tmp.path().join("x").exists(),
        "nothing landed outside the directory"
    );
    assert_eq!(
        sanitize_id("550e8400-e29b-41d4-a716-446655440001").ok(),
        Some("550e8400-e29b-41d4-a716-446655440001")
    );
}

/// 🔴 #8433: retire removes one managed session's files and mark, nobody else's.
#[test]
fn retire_removes_only_that_managed_sessions_files() {
    let (_tmp, dir) = dir();
    taint_managed_session(&dir, "m1", "a1", &mark()).expect("m1");
    write_taint(&dir, &mark().record_for("a2", "m1")).expect("m1 second id");
    taint_managed_session(&dir, "m2", "b1", &mark()).expect("m2");
    assert_eq!(retire(&dir, "m1").expect("retire"), 2);
    assert_eq!(read_state(&dir, "a1"), TaintState::Untainted);
    assert_eq!(read_state(&dir, "a2"), TaintState::Untainted);
    assert!(
        managed_mark(&dir, "m1").expect("read").is_none(),
        "m1 mark retired"
    );
    assert!(read_state(&dir, "b1").is_tainted(), "m2's file survived");
    assert!(
        managed_mark(&dir, "m2").expect("read").is_some(),
        "m2's mark survived"
    );
}

/// 🔴 #8433 (fails closed): a retire that cannot read a file keeps the mark.
#[test]
fn retire_failure_keeps_the_mark_and_the_files() {
    let (_tmp, dir) = dir();
    taint_managed_session(&dir, "m1", "a1", &mark()).expect("m1");
    std::fs::write(dir.join("zz.json"), "garbage").expect("unparseable");
    assert!(
        retire(&dir, "m1").is_err(),
        "an unreadable file fails the retire"
    );
    assert!(
        managed_mark(&dir, "m1").expect("read").is_some(),
        "the mark stays"
    );
    assert!(read_state(&dir, "zz").is_tainted());
}

/// 🔴 #8433 (Dfc): age never untaints.
#[test]
fn no_timer_retires_taint() {
    let (_tmp, dir) = dir();
    let mut old = mark();
    old.tainted_at = DateTime::parse_from_rfc3339("2001-01-01T00:00:00Z")
        .expect("date")
        .into();
    write_taint(&dir, &old.record_for("ancient", "m")).expect("write");
    assert_eq!(
        read_state(&dir, "ancient"),
        TaintState::Tainted(TaintReason::Present)
    );
}

/// #8433: reapply writes for a marked session, nothing for an unmarked one,
/// and an unreadable mark is an `Err`.
#[test]
fn reapply_follows_the_managed_mark() {
    let (_tmp, dir) = dir();
    assert!(!reapply(&dir, "m0", "n0").expect("unmarked"));
    assert_eq!(read_state(&dir, "n0"), TaintState::Untainted);
    mark_managed(&dir, "m1", &mark()).expect("mark");
    assert!(reapply(&dir, "m1", "n1").expect("marked"));
    assert!(read_state(&dir, "n1").is_tainted());
    std::fs::write(dir.join("managed").join("m2.json"), "garbage").expect("bad mark");
    assert!(
        reapply(&dir, "m2", "n2").is_err(),
        "a malformed mark fails closed"
    );
}

/// #8433: presence sees a taint file or a mark, and nothing in an empty tree.
#[test]
fn presence_counts_files_and_marks() {
    let (_tmp, dir) = dir();
    assert_eq!(presence(&dir), Presence::None);
    std::fs::create_dir_all(dir.join("managed")).expect("mkdir");
    assert_eq!(
        presence(&dir),
        Presence::None,
        "an empty managed dir is no taint"
    );
    mark_managed(&dir, "m1", &mark()).expect("mark");
    assert_eq!(presence(&dir), Presence::Some);
}

/// Set `path`'s mode; the tests restore it before their tempdir drops.
fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// 🔴 #8433 (r-078): a directory that cannot be listed is `Unknown`, never `None`.
#[test]
fn presence_is_unknown_when_a_dir_cannot_be_read() {
    let (_tmp, dir) = dir();
    mark_managed(&dir, "m1", &mark()).expect("mark");
    chmod(&dir, 0o000);
    let top = presence(&dir);
    chmod(&dir, 0o700);
    assert_eq!(top, Presence::Unknown, "an unreadable taint dir");
    chmod(&dir.join("managed"), 0o000);
    let sub = presence(&dir);
    chmod(&dir.join("managed"), 0o700);
    assert_eq!(sub, Presence::Unknown, "an unreadable managed dir");
}

/// 🔴 #8433 (r-078): a mark that exists but cannot be read is an `Io` error,
/// and a re-apply over it fails.
#[test]
fn managed_mark_io_error_is_err_and_reapply_fails() {
    let (_tmp, dir) = dir();
    std::fs::create_dir_all(dir.join("managed").join("m1.json")).expect("dir at the mark");
    assert!(
        matches!(managed_mark(&dir, "m1"), Err(TaintError::Io { .. })),
        "an unreadable mark is an Io error"
    );
    assert!(reapply(&dir, "m1", "n1").is_err(), "the re-apply fails");
    assert_eq!(read_state(&dir, "n1"), TaintState::Untainted);
}

/// 🔴 #8433 (r-078): a taint file retire cannot remove fails the retire and
/// keeps the mark, so the session stays tainted.
#[test]
fn retire_remove_failure_keeps_the_mark() {
    let (_tmp, dir) = dir();
    taint_managed_session(&dir, "m1", "a1", &mark()).expect("m1");
    chmod(&dir, 0o500);
    let result = retire(&dir, "m1");
    chmod(&dir, 0o700);
    assert!(matches!(result, Err(TaintError::Io { .. })), "{result:?}");
    assert!(
        managed_mark(&dir, "m1").expect("read").is_some(),
        "the mark stays"
    );
    assert!(read_state(&dir, "a1").is_tainted(), "the file stays");
}

/// 🔴 #8433 (r-078): a mark retire cannot remove fails the retire.
#[test]
fn retire_marker_removal_failure_is_err() {
    let (_tmp, dir) = dir();
    taint_managed_session(&dir, "m1", "a1", &mark()).expect("m1");
    chmod(&dir.join("managed"), 0o500);
    let result = retire(&dir, "m1");
    chmod(&dir.join("managed"), 0o700);
    assert!(matches!(result, Err(TaintError::Io { .. })), "{result:?}");
    assert!(
        managed_mark(&dir, "m1").expect("read").is_some(),
        "the mark stays"
    );
}

/// 🔴 #8433 (r-078): a failed atomic write removes its temp file.
#[test]
fn taint_write_failure_leaves_no_temp_file() {
    let (_tmp, dir) = dir();
    std::fs::create_dir_all(dir.join("s2.json").join("occupied")).expect("dir at the target");
    assert!(write_taint(&dir, &mark().record_for("s2", "m")).is_err());
    let names: Vec<_> = std::fs::read_dir(&dir)
        .expect("ls")
        .map(|e| e.expect("e").file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("s2.json")],
        "a temp file was left"
    );
}

/// 🔴 #8433 (D09): a mark covers a cwd at or under its workspace; a mark that
/// cannot be read covers every cwd; marks that cannot be listed are unknown.
#[test]
fn covering_mark_follows_the_workspace_and_fails_closed() {
    let (_tmp, dir) = dir();
    let ws = tempfile::tempdir().expect("ws");
    let other = tempfile::tempdir().expect("other");
    std::fs::create_dir_all(ws.path().join("sub")).expect("sub");
    assert_eq!(covering_mark(&dir, Some(ws.path())), Cover::None, "no mark");
    let mut m1 = mark();
    m1.workspace = ws.path().to_path_buf();
    mark_managed(&dir, "m1", &m1).expect("mark");
    let m1_id = Cover::Managed("m1".into());
    assert_eq!(covering_mark(&dir, Some(ws.path())), m1_id);
    assert_eq!(covering_mark(&dir, Some(&ws.path().join("sub"))), m1_id);
    assert_eq!(covering_mark(&dir, None), m1_id, "no cwd: covered");
    assert_eq!(covering_mark(&dir, Some(other.path())), Cover::None);
    std::fs::write(dir.join("managed").join("m2.json"), "garbage").expect("bad mark");
    assert_eq!(
        covering_mark(&dir, Some(other.path())),
        Cover::Managed("m2".into()),
        "an unreadable mark covers every cwd"
    );
    std::fs::remove_file(dir.join("managed").join("m2.json")).expect("rm");
    let mut rel = mark();
    rel.workspace = PathBuf::new();
    mark_managed(&dir, "m3", &rel).expect("mark");
    assert_eq!(
        covering_mark(&dir, Some(other.path())),
        Cover::Managed("m3".into()),
        "an empty workspace covers every cwd"
    );
    chmod(&dir.join("managed"), 0o000);
    let blocked = covering_mark(&dir, Some(other.path()));
    chmod(&dir.join("managed"), 0o700);
    assert_eq!(blocked, Cover::Unknown);
}

/// #8433 (D09): the double-failure record in the taint dir is not a taint
/// file, but it keeps the directory live.
#[test]
fn the_reapply_failures_record_is_not_a_taint_file() {
    let (_tmp, dir) = dir();
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(REAPPLY_FAILURES_FILE), "{}\n").expect("record");
    assert!(list_taints(&dir).expect("list").is_empty());
    assert_eq!(presence(&dir), Presence::Some);
}
