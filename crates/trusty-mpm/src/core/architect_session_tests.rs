//! Tests for the Architect session-name record (#8878 R1).

use std::path::{Path, PathBuf};

use super::*;
use crate::core::architect_launch::record_architect;

/// A scratch `~/.trusty-mpm` root and a project directory beside it.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("scratch");
        std::fs::create_dir(dir.path().join("project")).expect("project dir");
        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join(".trusty-mpm")
    }

    fn project(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path().join("project")).expect("canonical project")
    }

    fn sidecar(&self, pid: u32) -> PathBuf {
        sidecar_path(&self.root(), pid)
    }
}

/// A PID with no process behind it: a child that was spawned and reaped.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let pid = child.id();
    child.wait().expect("reap");
    pid
}

fn write_mode(path: &Path, body: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, body).expect("write sidecar");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[test]
fn session_names_are_validated() {
    let longest = "a".repeat(MAX_SESSION_NAME);
    for good in [
        "tm-architect",
        "tm-supervisor",
        "a",
        "A_1-b",
        longest.as_str(),
    ] {
        assert_eq!(validate_session_name(good), Ok(()), "{good:?}");
    }
    let too_long = "a".repeat(MAX_SESSION_NAME + 1);
    for bad in [
        "",
        "tm:arch",
        "tm.arch",
        "tm arch",
        "tm\tarch",
        " tm",
        "tm\n",
        "-tm",
        "_tm",
        "=tm",
        "$1",
        "%1",
        "@1",
        "tm/arch",
        "tm;x",
        "tëm",
        too_long.as_str(),
    ] {
        assert!(validate_session_name(bad).is_err(), "{bad:?} was accepted");
    }
    assert_eq!(poll_session_name("tm-supervisor"), "tm-supervisor-poll");
    assert_eq!(
        poll_session_name(DEFAULT_ARCHITECT_SESSION),
        "tm-architect-poll"
    );
}

#[test]
fn a_recorded_session_name_reads_back() {
    let s = Scratch::new();
    let me = std::process::id();
    assert_eq!(
        architect_session_name(&s.root(), me),
        Err(LaunchRefusal::NoLaunchRecord)
    );
    let record = record_launch(&s.root(), me, &s.project(), "tm-supervisor").expect("record");
    assert_eq!(record.pid, me);
    assert_eq!(
        architect_session_name(&s.root(), me).as_deref(),
        Ok("tm-supervisor")
    );
    // A record from a tm without session names has no sidecar: `tm-architect`.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    record_architect(&s.root(), child.id(), &s.project()).expect("legacy record");
    let legacy = architect_session_name(&s.root(), child.id());
    child.kill().expect("kill");
    child.wait().expect("reap");
    assert_eq!(legacy.as_deref(), Ok(DEFAULT_ARCHITECT_SESSION));
    // An invalid name is refused before anything is written.
    let other = Scratch::new();
    assert!(record_launch(&other.root(), me, &other.project(), "tm:x").is_err());
    assert!(!other.root().exists(), "a refused name wrote a record");
}

/// Fail-Open Check: every sidecar fault is an unreadable record, never the
/// `tm-architect` default, and a sidecar without its record is no record.
#[test]
fn a_bad_session_sidecar_is_unreadable_never_the_default() {
    let s = Scratch::new();
    let me = std::process::id();
    record_launch(&s.root(), me, &s.project(), "tm-supervisor").expect("record");
    let good = std::fs::read_to_string(s.sidecar(me)).expect("sidecar");
    let parsed: serde_json::Value = serde_json::from_str(&good).expect("json");
    let start = parsed["start_time"].as_u64().expect("start_time");
    let cases = [
        ("garbage", "not json".to_owned(), 0o600),
        (
            "other pid",
            format!(r#"{{"pid":{},"start_time":{start},"session":"x"}}"#, me + 1),
            0o600,
        ),
        (
            "other start time",
            format!(r#"{{"pid":{me},"start_time":{},"session":"x"}}"#, start + 1),
            0o600,
        ),
        (
            "invalid name",
            format!(r#"{{"pid":{me},"start_time":{start},"session":"tm:x"}}"#),
            0o600,
        ),
        (
            "unknown field",
            format!(r#"{{"pid":{me},"start_time":{start},"session":"x","extra":1}}"#),
            0o600,
        ),
        ("writable by others", good.clone(), 0o666),
    ];
    for (case, body, mode) in cases {
        write_mode(&s.sidecar(me), &body, mode);
        assert_eq!(
            architect_session_name(&s.root(), me),
            Err(LaunchRefusal::UnreadableRecord),
            "{case}"
        );
    }
    write_mode(&s.sidecar(me), &good, 0o600);
    std::fs::remove_file(ARCHITECT_RECORDS.path(&s.root(), me)).expect("remove record");
    assert_eq!(
        architect_session_name(&s.root(), me),
        Err(LaunchRefusal::NoLaunchRecord)
    );
}

/// Fail-Open Check: a missing record, an unreadable name, a dead process and
/// a name other than the session's are all "not bound".
#[test]
fn a_session_binding_needs_the_record_and_the_same_name() {
    let s = Scratch::new();
    let me = std::process::id();
    let bind =
        |session: &str, project: &Path| check_session_binding(&s.root(), project, session, me);
    assert_eq!(
        bind("tm-supervisor", &s.project()),
        Err(BindingRefusal::Launch(LaunchRefusal::NoLaunchRecord))
    );
    record_launch(&s.root(), me, &s.project(), "tm-supervisor").expect("record");
    assert_eq!(bind("tm-supervisor", &s.project()), Ok(()));
    assert_eq!(
        bind(DEFAULT_ARCHITECT_SESSION, &s.project()),
        Err(BindingRefusal::OtherSession {
            recorded: "tm-supervisor".to_owned()
        })
    );
    assert_eq!(
        bind("tm-supervisor", s.dir.path()),
        Err(BindingRefusal::Launch(LaunchRefusal::OtherProject))
    );
    write_mode(&s.sidecar(me), "{}", 0o600);
    assert_eq!(
        bind("tm-supervisor", &s.project()),
        Err(BindingRefusal::Launch(LaunchRefusal::UnreadableRecord))
    );
    assert_eq!(
        check_session_binding(&s.root(), &s.project(), "tm-supervisor", dead_pid()),
        Err(BindingRefusal::Launch(LaunchRefusal::ProcessLookup))
    );
    let text = BindingRefusal::OtherSession {
        recorded: "tm-supervisor".to_owned(),
    }
    .to_string();
    assert!(text.contains("names tmux session tm-supervisor"), "{text}");
}

/// #8878 R1 round 2, Fail-Open Check: the pane guard's fallback lists every
/// sidecar's name, live or stale; a sidecar or directory that does not read
/// is an error, never a shorter list.
#[test]
fn recorded_session_names_list_every_sidecar_and_fail_closed() {
    let s = Scratch::new();
    assert_eq!(
        recorded_session_names(&s.root()),
        Ok(vec![]),
        "no directory"
    );
    let me = std::process::id();
    record_launch(&s.root(), me, &s.project(), "tm-supervisor").expect("record");
    let stale = dead_pid();
    let body = format!(r#"{{"pid":{stale},"start_time":1,"session":"old-arch"}}"#);
    write_mode(&s.sidecar(stale), &body, 0o666);
    let mut names = recorded_session_names(&s.root()).expect("names");
    names.sort();
    assert_eq!(names, ["old-arch", "tm-supervisor"]);
    for (case, body) in [
        ("garbage", "not json".to_owned()),
        (
            "invalid name",
            format!(r#"{{"pid":{stale},"start_time":1,"session":"tm:x"}}"#),
        ),
    ] {
        write_mode(&s.sidecar(stale), &body, 0o600);
        assert!(recorded_session_names(&s.root()).is_err(), "{case}");
    }
    std::fs::remove_file(s.sidecar(stale)).expect("remove sidecar");
    std::fs::create_dir(s.sidecar(stale)).expect("a sidecar that is a directory");
    assert!(
        recorded_session_names(&s.root()).is_err(),
        "unreadable sidecar"
    );
    std::fs::remove_dir(s.sidecar(stale)).expect("remove dir");
    let dir = s.root().join(ARCHITECT_DIR);
    std::fs::remove_dir_all(&dir).expect("remove records");
    std::fs::write(&dir, "").expect("a record directory that is a file");
    assert!(recorded_session_names(&s.root()).is_err(), "unreadable dir");
}
