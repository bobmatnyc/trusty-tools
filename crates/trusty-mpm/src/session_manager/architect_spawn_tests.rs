//! Tests for the #9092 supervisor launch record.
//!
//! Hermetic: every root is a scratch directory; nothing reads the operator's
//! `~/.trusty-mpm`, and nothing is spawned (the record binds this test
//! process, whose start time the process table can read).

use std::path::{Path, PathBuf};

use super::{SpawnBinding, binding_for};
use crate::core::architect_launch::ARCHITECT_RECORDS;
use crate::core::architect_session::architect_session_name;
use crate::session_manager::ManagedSessionId;

/// A scratch `~/.trusty-mpm` root and a project directory.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let s = Self {
            dir: crate::test_support::hermetic_temp_dir(),
        };
        std::fs::create_dir_all(s.project()).expect("project dir");
        s
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join(".trusty-mpm")
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("architect")
    }

    /// Write a store holding one record of `kind` in `workspace`; its id.
    fn store(&self, kind: &str, workspace: &Path) -> String {
        let dir = self.root().join("session-manager");
        std::fs::create_dir_all(&dir).expect("store dir");
        let id = ManagedSessionId::new().to_string();
        let record = serde_json::json!({
            "id": id, "task": "t", "tmux_name": "tm-architect", "cwd": workspace,
            "state": "stopped", "created_at": "2026-10-30T00:00:00Z", "kind": kind,
        });
        let raw = serde_json::json!({ "sessions": { id.clone(): record } });
        std::fs::write(dir.join("sessions.json"), raw.to_string()).expect("store");
        id
    }
}

/// A `supervisor` record launched in its own workspace needs a binding, and
/// the binding writes a 0600 record and sidecar that read back this session.
#[test]
fn a_supervisor_record_needs_a_binding() {
    let s = Scratch::new();
    let id = s.store("supervisor", &s.project());
    let binding = binding_for(&s.root(), &id, &s.project())
        .expect("a readable store")
        .expect("a supervisor launch needs a record");
    assert_eq!(binding.session(), "tm-architect");
    binding.preflight().expect("a writable record dir");

    let me = std::process::id();
    let record = binding.record(me).expect("record");
    assert_eq!(record.pid, me);
    assert_eq!(
        record.project_dir,
        std::fs::canonicalize(s.project()).expect("canonical")
    );
    let path = ARCHITECT_RECORDS.path(&s.root(), me);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)
            .expect("record")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{}", path.display());
    }
    assert_eq!(
        architect_session_name(&s.root(), me).expect("sidecar"),
        "tm-architect"
    );
}

/// No binding for a CLI launch (no session id), an ordinary or helper
/// record, a session the store does not hold, or a missing store.
#[test]
fn an_ordinary_record_or_a_cli_launch_needs_none() {
    let s = Scratch::new();
    assert_eq!(binding_for(&s.root(), "", &s.project()), Ok(None));
    assert_eq!(
        binding_for(&s.root(), "no-store-yet", &s.project()),
        Ok(None)
    );
    for kind in ["ordinary", "supervisor_aux"] {
        let id = s.store(kind, &s.project());
        assert_eq!(
            binding_for(&s.root(), &id, &s.project()),
            Ok(None),
            "{kind}"
        );
    }
    let _ = s.store("supervisor", &s.project());
    let other = ManagedSessionId::new().to_string();
    assert_eq!(binding_for(&s.root(), &other, &s.project()), Ok(None));
}

/// Fail closed: an unparseable store, or a kind this build does not know,
/// refuses the launch rather than reading as "not the Architect".
#[test]
fn an_unreadable_store_or_unknown_kind_refuses() {
    let s = Scratch::new();
    let id = s.store("from-the-future", &s.project());
    assert!(binding_for(&s.root(), &id, &s.project()).is_err());
    std::fs::write(
        s.root().join("session-manager").join("sessions.json"),
        "{ not json",
    )
    .expect("corrupt");
    assert!(binding_for(&s.root(), &id, &s.project()).is_err());
}

/// A supervisor record launched outside its workspace refuses: the record
/// would bind a `claude` running in another project.
#[test]
fn a_supervisor_launch_in_another_directory_refuses() {
    let s = Scratch::new();
    let id = s.store("supervisor", &s.project());
    let elsewhere = s.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("dir");
    let err = binding_for(&s.root(), &id, &elsewhere).expect_err("another cwd refuses");
    assert!(err.contains("workspace"), "{err}");
}

/// A record directory that cannot be written fails the preflight, so the
/// shim refuses before it spawns anything.
#[cfg(unix)]
#[test]
fn an_unwritable_record_dir_fails_the_preflight() {
    use std::os::unix::fs::PermissionsExt as _;
    let s = Scratch::new();
    let id = s.store("supervisor", &s.project());
    let binding: SpawnBinding = binding_for(&s.root(), &id, &s.project())
        .expect("store")
        .expect("binding");
    let dir = s.root().join("architect-launch");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let result = binding.preflight();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
    let err = result.expect_err("an unwritable record dir fails");
    assert!(err.contains("architect-launch"), "{err}");
}
