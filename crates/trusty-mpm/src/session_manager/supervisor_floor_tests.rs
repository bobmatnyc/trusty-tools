//! Tests for the #8942 kill-by-name floor.
//!
//! Hermetic: every floor is rooted in a scratch directory, and every tmux call
//! goes to a scripted fake binary that only logs its argv. No test reaches a
//! real tmux server or the daemon.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{KillVerdict, SupervisorFloor};
use crate::daemon::tmux::TmuxDriver;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, RealTmuxDriver, SessionKind,
    SessionManager, SessionRecord, SidecarRole,
};

/// A scratch `~/.trusty-mpm` root with helpers for sidecars and the store.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        Self {
            dir: crate::test_support::hermetic_temp_dir(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join(".trusty-mpm")
    }

    fn floor(&self) -> SupervisorFloor {
        SupervisorFloor::at_root(&self.root())
    }

    /// Record Architect session `session` in a launch sidecar for `pid`.
    fn sidecar(&self, pid: u32, session: &str) {
        let dir = self.root().join("architect-launch");
        std::fs::create_dir_all(&dir).expect("sidecar dir");
        let body = serde_json::json!({"pid": pid, "start_time": 1, "session": session});
        std::fs::write(
            dir.join(format!("{pid}.architect-session")),
            body.to_string(),
        )
        .expect("sidecar");
    }

    /// Write the store with one record per `(tmux_name, state, kind)`.
    fn store(&self, records: &[(&str, &str, &str)]) {
        let dir = self.root().join("session-manager");
        std::fs::create_dir_all(&dir).expect("store dir");
        let sessions: serde_json::Map<String, serde_json::Value> = records
            .iter()
            .map(|(name, state, kind)| {
                let id = ManagedSessionId::new().to_string();
                let record = record_json(&id, name, state, kind);
                (id, record)
            })
            .collect();
        let raw = serde_json::json!({ "sessions": sessions });
        std::fs::write(dir.join("sessions.json"), raw.to_string()).expect("store");
    }

    /// A fake tmux that logs every argv line to `calls.log` and lists
    /// `live` as its only session.
    fn fake_tmux(&self, live: &str) -> (String, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let log = self.dir.path().join("calls.log");
        let bin = self.dir.path().join("fake-tmux");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\n\
             if [ \"$1\" = list-sessions ]; then echo '{live}:1700000000:1'; fi\nexit 0\n",
            log = log.display()
        );
        std::fs::write(&bin, script).expect("write fake tmux");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (bin.to_string_lossy().into_owned(), log)
    }
}

/// One persisted record; `""` omits `kind`.
fn record_json(id: &str, name: &str, state: &str, kind: &str) -> serde_json::Value {
    let mut record = serde_json::json!({
        "id": id, "task": "t", "tmux_name": name, "cwd": "/tmp", "state": state,
        "created_at": "2026-09-30T00:00:00Z",
    });
    if !kind.is_empty() {
        record["kind"] = serde_json::json!(kind);
    }
    record
}

/// Every argv the fake tmux was run with, one line per call.
fn calls(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

fn is_protected(v: &KillVerdict) -> bool {
    matches!(v, KillVerdict::Protected(_))
}

/// Source 1: a name an Architect launch sidecar records.
#[test]
fn a_sidecar_name_is_protected() {
    let s = Scratch::new();
    s.sidecar(111, "tm-arch");
    let v = s.floor().verdict("tm-arch");
    assert!(is_protected(&v), "{v:?}");
}

/// Source 2: the Architect's helper sessions, `<name>-poll` and
/// `<name>-collector`; any other suffix is not protected by this source.
#[test]
fn the_architects_helper_sessions_are_protected() {
    let s = Scratch::new();
    s.sidecar(111, "tm-arch");
    for aux in ["tm-arch-poll", "tm-arch-collector"] {
        let v = s.floor().verdict(aux);
        assert!(is_protected(&v), "{aux}: {v:?}");
    }
    assert_eq!(s.floor().verdict("tm-arch-other"), KillVerdict::Permit);
}

/// Source 3: a live record whose kind is protected. A tombstoned one no
/// longer protects the name.
#[test]
fn a_protected_kind_record_is_protected() {
    let s = Scratch::new();
    s.store(&[
        ("tm-sup", "active", "supervisor"),
        ("tm-sup-poll", "stopped", "supervisor_aux"),
        ("tm-gone", "deleted", "supervisor"),
        ("tm-plain", "active", ""),
    ]);
    for name in ["tm-sup", "tm-sup-poll"] {
        let v = s.floor().verdict(name);
        assert!(is_protected(&v), "{name}: {v:?}");
    }
    for name in ["tm-gone", "tm-plain"] {
        assert_eq!(s.floor().verdict(name), KillVerdict::Permit, "{name}");
    }
}

/// #8942 fail-closed: a record with a kind this build does not know is
/// protected, and the kill primitive refuses it without running tmux.
#[test]
fn a_record_with_an_unknown_kind_is_never_torn_down() {
    let s = Scratch::new();
    s.store(&[("tm-future", "active", "future_role")]);
    let v = s.floor().verdict("tm-future");
    assert!(is_protected(&v), "{v:?}");

    let (bin, log) = s.fake_tmux("tm-future");
    let driver = TmuxDriver::with_tmux_path_for_test(bin).with_floor(s.floor());
    let err = driver
        .kill_session("tm-future")
        .expect_err("an Unknown-kind record's session must not be killed");
    assert!(err.to_string().contains("#8942"), "{err}");
    assert_eq!(
        calls(&log),
        "",
        "no tmux process may run for a refused kill"
    );
}

/// #8942 fail-closed: when the sidecars cannot be read, no name can be
/// cleared, so the kill primitive refuses every name.
#[test]
fn kill_by_name_fails_closed_when_architect_sidecars_cannot_be_read() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.root()).expect("root");
    // A sidecar directory that is a file cannot be listed.
    std::fs::write(s.root().join("architect-launch"), "").expect("unreadable dir");
    let (bin, log) = s.fake_tmux("tm-any");
    let driver = TmuxDriver::with_tmux_path_for_test(bin).with_floor(s.floor());

    let v = s.floor().verdict("tm-any");
    assert!(matches!(v, KillVerdict::Undeterminable(_)), "{v:?}");
    assert!(s.floor().refuse("tm-any", "test").is_some());
    let err = driver
        .kill_session("tm-any")
        .expect_err("an unreadable sidecar dir must refuse the kill");
    assert!(err.to_string().contains("cannot rule out"), "{err}");
    assert_eq!(
        calls(&log),
        "",
        "no tmux process may run for a refused kill"
    );

    // A sidecar that does not parse is unreadable too.
    std::fs::remove_file(s.root().join("architect-launch")).expect("remove file");
    std::fs::create_dir(s.root().join("architect-launch")).expect("dir");
    std::fs::write(
        s.root()
            .join("architect-launch")
            .join("1.architect-session"),
        "not json",
    )
    .expect("garbage sidecar");
    let v = s.floor().verdict("tm-any");
    assert!(matches!(v, KillVerdict::Undeterminable(_)), "{v:?}");
}

/// #8942 fail-closed: a store that does not parse refuses the kill.
#[test]
fn an_unreadable_store_is_undeterminable() {
    let s = Scratch::new();
    let dir = s.root().join("session-manager");
    std::fs::create_dir_all(&dir).expect("store dir");
    std::fs::write(dir.join("sessions.json"), "{ truncated").expect("store");
    let v = s.floor().verdict("tm-any");
    assert!(matches!(v, KillVerdict::Undeterminable(_)), "{v:?}");
}

#[test]
fn a_missing_or_relative_home_is_undeterminable() {
    for home in [None, Some(PathBuf::from("relative/home"))] {
        let v = SupervisorFloor::from_home(home.clone()).verdict("tm-any");
        assert!(
            matches!(v, KillVerdict::Undeterminable(_)),
            "{home:?}: {v:?}"
        );
    }
}

/// The positive control: with sidecars and a store present, an unrelated name
/// is permitted and the kill reaches tmux. Serial: the tmux spawn guard reads
/// `$HOME`, which other serial tests reassign.
#[serial_test::serial]
#[test]
fn an_unrelated_name_is_permitted() {
    let s = Scratch::new();
    s.sidecar(111, "tm-arch");
    s.store(&[("tm-sup", "active", "supervisor")]);
    assert_eq!(s.floor().verdict("tm-work"), KillVerdict::Permit);

    let (bin, log) = s.fake_tmux("tm-work");
    let driver = TmuxDriver::with_tmux_path_for_test(bin).with_floor(s.floor());
    driver
        .kill_session("tm-work")
        .expect("an ordinary kill runs");
    assert!(calls(&log).contains("kill-session"), "{}", calls(&log));
}

/// #8935 repro: a stale ORDINARY record that carries the Architect's tmux
/// name is stopped. Neither the signal (`send-keys` C-c, since no pid
/// resolves) nor the kill reaches the pane; the driver's typed refusal comes
/// back through `graceful_terminate_runtime`, and the record stays `Active`
/// (critic HIGH).
#[serial_test::serial]
#[tokio::test]
async fn stopping_a_stale_record_never_kills_a_session_named_by_an_architect_sidecar() {
    let s = Scratch::new();
    s.sidecar(111, "tm-arch");
    let (bin, log) = s.fake_tmux("tm-arch");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.clone()).with_floor(s.floor());
    let data = s.dir.path().join("data");
    let mgr = SessionManager::new(
        &data,
        Arc::new(RealTmuxDriver::from_driver_for_test(driver)),
    )
    .await
    .expect("manager");
    let id = ManagedSessionId::new();
    let stale: SessionRecord =
        serde_json::from_value(record_json(&id.to_string(), "tm-arch", "active", ""))
            .expect("stale record");
    mgr.store.write().await.upsert(stale).await.expect("seed");

    // Every tmux resolution inside the stop, including the pid probe, goes to
    // the fake, so nothing here can reach a real server.
    let err = crate::core::tmux::with_tmux_binary(PathBuf::from(&bin), mgr.stop(&id))
        .await
        .expect_err("the refused kill aborts the stop");

    assert!(matches!(err, ManagedError::KillRefused(_)), "{err}");
    let after = mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Active);
    let calls = calls(&log);
    assert!(
        !calls.contains("send-keys"),
        "the Architect's pane was signalled: {calls}"
    );
    assert!(
        !calls.contains("kill-session"),
        "the Architect's pane was killed: {calls}"
    );
    assert!(
        calls.contains("list-sessions"),
        "the fake was reached: {calls}"
    );
}

/// A manager's floor reads the sidecars in the framework root above its
/// `session-manager` store directory, and the store inside it; any other
/// directory is its own root.
#[test]
fn for_data_dir_reads_the_framework_root_above_the_store() {
    let s = Scratch::new();
    s.sidecar(111, "tm-arch");
    s.store(&[("tm-sup", "active", "supervisor")]);
    let floor = SupervisorFloor::for_data_dir(&s.root().join("session-manager"));
    for name in ["tm-arch", "tm-sup"] {
        let v = floor.verdict(name);
        assert!(is_protected(&v), "{name}: {v:?}");
    }
    let elsewhere = SupervisorFloor::for_data_dir(&s.dir.path().join("data"));
    assert_eq!(elsewhere.verdict("tm-arch"), KillVerdict::Permit);
}

/// A sidecar name is the Architect; its `-poll`/`-collector` names are its
/// helpers, each with its own id role; nothing else is either.
#[test]
fn a_sidecar_names_the_architect_and_its_helpers() {
    let s = Scratch::new();
    s.sidecar(111, "tm-arch");
    let names = s.floor().sidecar_names().expect("sidecars read");
    let role = |name| SupervisorFloor::sidecar_role(&names, name);
    let expect = |kind, role| Some(SidecarRole { kind, role });
    assert_eq!(
        role("tm-arch"),
        expect(SessionKind::Supervisor, "supervisor")
    );
    assert_eq!(
        role("tm-arch-poll"),
        expect(SessionKind::SupervisorAux, "-poll")
    );
    assert_eq!(
        role("tm-arch-collector"),
        expect(SessionKind::SupervisorAux, "-collector")
    );
    assert_eq!(role("tm-arch-other"), None);
    assert_eq!(role("tm-work"), None);
}
