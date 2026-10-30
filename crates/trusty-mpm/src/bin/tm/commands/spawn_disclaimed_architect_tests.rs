//! #9092: the launch shim refuses a spec naming the supervisor session.
//!
//! Hermetic: a scratch home holds the session store and the spec; the
//! "claude" is `/bin/sh`, which touches a marker so a test can tell whether
//! anything ran. No tmux, daemon or real `claude` is used.

use std::path::{Path, PathBuf};

use trusty_mpm::runtime::launch_spec::LaunchSpec;

use super::launch_from_spec;

const SESSION_ID: &str = "0708af71-2b5e-5dab-8c92-739ebdb04941";

/// A scratch home: `~/.trusty-mpm`, a project and a spec dir.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    /// A home whose store holds one `SESSION_ID` record of `kind`.
    fn new(kind: &str) -> Self {
        let h = Self {
            dir: tempfile::tempdir().expect("tempdir"),
        };
        std::fs::create_dir_all(h.project()).expect("project");
        std::fs::create_dir_all(h.specs()).expect("specs");
        let record = serde_json::json!({
            "id": SESSION_ID, "task": "t", "tmux_name": "tm-architect",
            "cwd": h.project(), "state": "stopped",
            "created_at": "2026-10-30T00:00:00Z", "kind": kind,
        });
        h.write_store(&serde_json::json!({ "sessions": { SESSION_ID: record } }).to_string());
        h
    }

    fn write_store(&self, raw: &str) {
        let store = self.root().join("session-manager");
        std::fs::create_dir_all(&store).expect("store dir");
        std::fs::write(store.join("sessions.json"), raw).expect("store");
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

    /// The file the launched "claude" touches when it runs.
    fn marker(&self) -> PathBuf {
        self.dir.path().join("ran")
    }

    /// A spec for `session_id` in `cwd` that touches [`Self::marker`].
    fn spec(&self, session_id: &str, cwd: &Path) -> PathBuf {
        let spec = LaunchSpec {
            session_id: session_id.to_owned(),
            launch_id: "aaaabbbbccccddddeeeeffff00009092".to_owned(),
            cwd: cwd.to_path_buf(),
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!("touch '{}'", self.marker().display()),
            ],
            env_unset: Vec::new(),
            env_set: Vec::new(),
        };
        spec.write_in(&self.specs()).expect("write spec")
    }

    /// Run the shim on `spec` under this home, as an operator pane would
    /// with the Claude Code markers stripped (`env -u CLAUDECODE ...`).
    fn launch(&self, spec: &Path) -> anyhow::Result<(i32, Option<i32>)> {
        launch_from_spec(spec, Some(&self.root()))
    }

    /// Files in the launch-record directory; none when it is absent.
    fn record_files(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.root().join("architect-launch")) else {
            return Vec::new();
        };
        entries
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect()
    }
}

/// The critic's attack: a spec naming the supervisor session, run with the
/// Claude Code markers stripped, is refused before the spawn. Nothing runs,
/// and no launch record or sidecar is written.
#[test]
fn a_supervisor_spec_with_markers_stripped_writes_no_record_and_spawns_nothing_9092() {
    let h = Home::new("supervisor");
    let spec = h.spec(SESSION_ID, &h.project());

    let result = h.launch(&spec);
    std::thread::sleep(std::time::Duration::from_millis(200));

    assert!(h.record_files().is_empty(), "{:?}", h.record_files());
    assert!(!h.marker().exists(), "the supervisor spec spawned claude");
    let err = result.expect_err("a supervisor spec is refused");
    assert!(format!("{err:#}").contains("tm fleet relaunch"), "{err:#}");
}

/// An ordinary, unknown-kind or unlisted session launches as before,
/// including from a cwd that is not the record's workspace; so do an empty
/// session id and a missing store.
#[test]
fn ordinary_unknown_and_unlisted_specs_still_launch_9092() {
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let cases: [(&str, &str, bool, bool); 6] = [
        ("ordinary", SESSION_ID, false, true),
        ("ordinary", SESSION_ID, false, false),
        ("a-kind-from-a-newer-tm", SESSION_ID, false, true),
        ("supervisor", "another-session", false, true),
        ("supervisor", "", false, true),
        ("supervisor", SESSION_ID, true, true),
    ];
    for (kind, id, drop_store, in_project) in cases {
        let h = Home::new(kind);
        if drop_store {
            std::fs::remove_dir_all(h.root()).expect("drop store");
        }
        let cwd = if in_project {
            h.project()
        } else {
            elsewhere.path().to_path_buf()
        };
        let spec = h.spec(id, &cwd);
        let (status, _) = h
            .launch(&spec)
            .unwrap_or_else(|e| panic!("{kind}/{id:?}/{drop_store}: {e:#}"));
        assert_eq!(status, 0, "{kind}/{id:?}");
        assert!(h.marker().exists(), "{kind}/{id:?} did not run");
        assert!(h.record_files().is_empty(), "{:?}", h.record_files());
    }
}

/// Fail-closed: a store that does not parse refuses a spec that names a
/// session, since its kind cannot be told; a spec with no session id still
/// launches.
#[test]
fn an_unreadable_store_refuses_a_spec_that_names_a_session_9092() {
    let h = Home::new("ordinary");
    h.write_store("{\"sessions\":");

    let err = h
        .launch(&h.spec(SESSION_ID, &h.project()))
        .expect_err("an unreadable store refuses");
    assert!(
        format!("{err:#}").contains("cannot tell whether it is the Architect's"),
        "{err:#}"
    );
    assert!(!h.marker().exists(), "claude started");

    let (status, _) = h.launch(&h.spec("", &h.project())).expect("no session id");
    assert_eq!(status, 0);
    assert!(h.marker().exists());
}
