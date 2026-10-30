//! #9092: the launch shim records the supervisor session's `claude`.
//!
//! Hermetic: a scratch home holds the session store, the launch records and
//! the spec; the "claude" is `/bin/sh`, which writes its own PID and a marker
//! so a test can tell what ran. No tmux, daemon or real `claude` is used.

use std::path::{Path, PathBuf};

use trusty_mpm::core::architect_launch::ARCHITECT_RECORDS;
use trusty_mpm::core::architect_session::architect_session_name;
use trusty_mpm::core::twin_arming::process_facts;
use trusty_mpm::runtime::launch_spec::LaunchSpec;

use super::launch_from_spec;

const SESSION_ID: &str = "0708af71-2b5e-5dab-8c92-739ebdb04941";

/// A scratch home: `~/.trusty-mpm`, an Architect project and a spec dir.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new(kind: &str) -> Self {
        let h = Self {
            dir: tempfile::tempdir().expect("tempdir"),
        };
        std::fs::create_dir_all(h.project()).expect("project");
        std::fs::create_dir_all(h.specs()).expect("specs");
        let store = h.root().join("session-manager");
        std::fs::create_dir_all(&store).expect("store dir");
        let record = serde_json::json!({
            "id": SESSION_ID, "task": "t", "tmux_name": "tm-architect",
            "cwd": h.project(), "state": "stopped",
            "created_at": "2026-10-30T00:00:00Z", "kind": kind,
        });
        let raw = serde_json::json!({ "sessions": { SESSION_ID: record } });
        std::fs::write(store.join("sessions.json"), raw.to_string()).expect("store");
        h
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join(".trusty-mpm")
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("architect")
    }

    fn specs(&self) -> PathBuf {
        self.dir.path().join("specs")
    }

    fn records(&self) -> PathBuf {
        self.root().join("architect-launch")
    }

    /// The file a launched "claude" writes: its PID, or that it ran.
    fn out(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A spec for the supervisor session running `/bin/sh -c script`.
    fn spec(&self, script: &str) -> PathBuf {
        let spec = LaunchSpec {
            session_id: SESSION_ID.to_owned(),
            launch_id: "aaaabbbbccccddddeeeeffff00009092".to_owned(),
            cwd: self.project(),
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            env_unset: Vec::new(),
            env_set: Vec::new(),
        };
        spec.write_in(&self.specs()).expect("write spec")
    }

    /// Files in the launch-record directory; none when it is absent.
    fn record_files(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.records()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// Poll `cond` for up to 5 s.
fn eventually(cond: impl Fn() -> bool) -> bool {
    for _ in 0..100 {
        if cond() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

fn read_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid")
}

/// AC1: a managed launch of a `supervisor` session leaves exactly one record
/// and one sidecar, for the spawned PID, mode 0600, with its live start time
/// and the project directory.
#[test]
fn a_supervisor_launch_records_the_claude_it_spawned_9092() {
    let h = Home::new("supervisor");
    let pid_file = h.out("pid");
    let spec = h.spec(&format!("echo $$ > '{}'; sleep 2", pid_file.display()));
    let root = h.root();
    let launch = std::thread::spawn(move || launch_from_spec(&spec, &root, |_| None));

    assert!(eventually(
        || pid_file.exists() && h.record_files().len() == 2
    ));
    let pid = read_pid(&pid_file);
    let live_start = process_facts(pid).expect("the child is alive").start_time;
    let (status, _) = launch.join().expect("join").expect("launch");
    assert_eq!(status, 0);

    assert_eq!(
        h.record_files(),
        vec![
            format!("{pid}.architect"),
            format!("{pid}.architect-session")
        ]
    );
    let record = ARCHITECT_RECORDS
        .read(&h.root(), pid)
        .expect("readable")
        .expect("record");
    assert_eq!(record.pid, pid);
    assert_eq!(record.start_time, live_start);
    assert_eq!(
        record.project_dir,
        std::fs::canonicalize(h.project()).expect("canonical")
    );
    assert_eq!(
        architect_session_name(&h.root(), pid).expect("sidecar"),
        "tm-architect"
    );
    use std::os::unix::fs::PermissionsExt as _;
    for name in h.record_files() {
        let mode = std::fs::metadata(h.records().join(&name))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
}

/// AC2 (Fail-Open): a record directory that cannot be written fails the
/// launch with an error, and the "claude" never runs.
#[test]
fn a_supervisor_launch_with_an_unwritable_record_dir_spawns_nothing_9092() {
    use std::os::unix::fs::PermissionsExt as _;
    let h = Home::new("supervisor");
    let marker = h.out("ran");
    let spec = h.spec(&format!("touch '{}'", marker.display()));
    std::fs::create_dir_all(h.records()).expect("dir");
    std::fs::set_permissions(h.records(), std::fs::Permissions::from_mode(0o500)).expect("chmod");

    let result = launch_from_spec(&spec, &h.root(), |_| None);
    std::fs::set_permissions(h.records(), std::fs::Permissions::from_mode(0o700)).expect("restore");

    let err = result.expect_err("an unwritable record dir fails the launch");
    assert!(
        format!("{err:#}").contains("launch record cannot be written"),
        "{err:#}"
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!marker.exists(), "claude started unbound");
    assert!(h.record_files().is_empty(), "{:?}", h.record_files());
}

/// AC2: a supervisor launch from inside a Claude Code session refuses before
/// anything runs, so a hooked session cannot mint an Architect record.
#[test]
fn a_supervisor_launch_inside_claude_code_spawns_nothing_9092() {
    let h = Home::new("supervisor");
    let marker = h.out("ran");
    let spec = h.spec(&format!("touch '{}'", marker.display()));
    let env = |name: &str| (name == "CLAUDECODE").then(|| "1".into());

    let err = launch_from_spec(&spec, &h.root(), env).expect_err("refused");
    assert!(format!("{err:#}").contains("CLAUDECODE"), "{err:#}");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!marker.exists(), "claude started");
    assert!(h.record_files().is_empty(), "{:?}", h.record_files());
}

/// AC3: an ordinary session's launch writes no launch record.
#[test]
fn an_ordinary_launch_writes_no_launch_record_9092() {
    let h = Home::new("ordinary");
    let marker = h.out("ran");
    let spec = h.spec(&format!("touch '{}'", marker.display()));

    let (status, _) = launch_from_spec(&spec, &h.root(), |_| None).expect("launch");
    assert_eq!(status, 0);
    assert!(marker.exists(), "the ordinary launch ran");
    assert!(h.record_files().is_empty(), "{:?}", h.record_files());
}
