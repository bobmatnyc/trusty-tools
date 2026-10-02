//! Tests for #8935: a stale record never tears down a live session that
//! reused its tmux name.
//!
//! Hermetic: every manager runs the production `RealTmuxDriver` against a
//! scripted fake tmux binary, inside a `with_tmux_binary` scope so the pid
//! probe reaches the fake too. The fake only logs its argv and prints canned
//! answers. #9004: the `live_*` tests instead run a private `-L` tmux server
//! (`PrivateTmuxServer`), never the default one, and skip without tmux.

use std::path::PathBuf;
use std::sync::Arc;

use super::runtime_identity::RuntimeTeardown;
use crate::daemon::tmux::TmuxDriver;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, RealTmuxDriver, SessionManager,
    SessionRecord, StopCause, SupervisorFloor,
};
use crate::test_support::tmux_session::{
    PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
};

/// The name every fixture's live session carries. Reserved test prefix, so
/// even a leak could never collide with an operator's session.
const LIVE: &str = "tmpm-test-8935-reuse";

/// #9004: the live tmux server instance, `<pid>:<start_time>`.
const LIVE_SERVER: &str = "4242:1700000000";

/// #9004: the server instance before a restart — same socket, other pid.
const OLD_SERVER: &str = "1111:1600000000";

/// #9004: what the live server's `display-message` prints for pane %9.
const LIVE_IDENTITY: &str = "%9|$3|4242:1700000000|tmpm-test-8935-reuse";

/// What the fake tmux answers.
struct FakeTmux {
    /// `list-sessions` exits 1 with an unrecognised error when `true`.
    sessions_fail: bool,
    /// The panes `list-panes` prints, or `None` to exit 1.
    panes: Option<&'static str>,
    /// #9004: what the pane-identity `display-message` prints, or `None` to
    /// exit 1.
    identity: Option<&'static str>,
    /// What tmux answers once a `send-keys` reached it — the state the
    /// post-grace re-check sees — or `None` to keep the answers above.
    after_signal: Option<AfterSignal>,
}

impl Default for FakeTmux {
    /// A live session [`LIVE`] holding only pane %9, on [`LIVE_SERVER`].
    fn default() -> Self {
        Self {
            sessions_fail: false,
            panes: Some("%9:1"),
            identity: Some(LIVE_IDENTITY),
            after_signal: None,
        }
    }
}

/// tmux's state after the teardown's signal (#8935 critic round).
enum AfterSignal {
    /// No session carries the name any more.
    Gone,
    /// The session lists these panes.
    Panes(&'static str),
    /// `list-panes` fails: ownership can no longer be proved.
    Unlistable,
    /// #9004: the pane-identity read prints this line, or exits 1 on `None`.
    Identity(Option<&'static str>),
}

/// The fake's answer to a pane-identity `display-message`.
fn identity_answer(identity: Option<&str>) -> String {
    match identity {
        Some(line) => format!("echo '{line}'; exit 0"),
        None => "echo 'lost server' >&2; exit 1".to_string(),
    }
}

/// One `echo` per row: `printf` would read `%9` as a conversion.
fn echo_rows(rows: &str) -> String {
    rows.lines()
        .map(|row| format!("echo '{row}'"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A scratch framework root, a fake tmux and a manager over both.
struct Fixture {
    dir: tempfile::TempDir,
    bin: PathBuf,
    mgr: SessionManager,
}

impl Fixture {
    async fn new(fake: FakeTmux) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::test_support::hermetic_temp_dir();
        let log = dir.path().join("calls.log");
        let bin = dir.path().join("fake-tmux");
        let sessions = if fake.sessions_fail {
            "echo 'protocol version mismatch' >&2; exit 1".to_string()
        } else {
            format!("echo '{LIVE}:1700000000:1'")
        };
        let panes = match fake.panes {
            Some(rows) => echo_rows(rows),
            None => "echo 'lost server' >&2; exit 1".to_string(),
        };
        let signalled = dir.path().join("signalled");
        let after = match fake.after_signal {
            None => String::new(),
            Some(AfterSignal::Gone) => "if [ \"$1\" = list-sessions ]; then exit 0; fi".into(),
            Some(AfterSignal::Panes(rows)) => format!(
                "if [ \"$1\" = list-panes ]; then {}; exit 0; fi",
                echo_rows(rows)
            ),
            Some(AfterSignal::Unlistable) => {
                "if [ \"$1\" = list-panes ]; then echo 'lost server' >&2; exit 1; fi".into()
            }
            Some(AfterSignal::Identity(line)) => format!(
                "case \"$*\" in *session_id*) {};; esac",
                identity_answer(line)
            ),
        };
        // #9004: only the pane-identity read asks for `session_id`; the pid
        // probe's `#{pane_pid}` read keeps its empty answer.
        let identity = identity_answer(fake.identity);
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\n\
             if [ \"$1\" = send-keys ]; then touch '{signalled}'; fi\n\
             if [ -f '{signalled}' ]; then :; {after}\nfi\n\
             if [ \"$1\" = list-sessions ]; then {sessions}; fi\n\
             case \"$*\" in *session_id*) {identity};; esac\n\
             if [ \"$1\" = list-panes ]; then {panes}; fi\nexit 0\n",
            log = log.display(),
            signalled = signalled.display(),
        );
        std::fs::write(&bin, script).expect("write fake tmux");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self::with_bin(dir, bin).await
    }

    /// A manager whose every tmux call runs `bin` — the fake, or a private
    /// server's shim (#9004).
    async fn with_bin(dir: tempfile::TempDir, bin: PathBuf) -> Self {
        let data = dir.path().join(".trusty-mpm").join("session-manager");
        std::fs::create_dir_all(&data).expect("data dir");
        let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned())
            .with_floor(SupervisorFloor::for_data_dir(&data));
        let mgr = SessionManager::new(
            &data,
            Arc::new(RealTmuxDriver::from_driver_for_test(driver)),
        )
        .await
        .expect("manager");
        Self { dir, bin, mgr }
    }

    /// Seed an ordinary record named [`LIVE`], bound to `pane_id`, with no
    /// tmux server identity — the shape of a record written before #9004.
    async fn seed(&self, state: &str, pane_id: Option<&str>) -> ManagedSessionId {
        self.seed_on(state, pane_id, None).await
    }

    /// [`Self::seed`], with the pane captured on tmux server `server` (#9004).
    async fn seed_on(
        &self,
        state: &str,
        pane_id: Option<&str>,
        server: Option<&str>,
    ) -> ManagedSessionId {
        self.seed_named(LIVE, state, pane_id, server).await
    }

    /// [`Self::seed_on`] for a record named `name`.
    async fn seed_named(
        &self,
        name: &str,
        state: &str,
        pane_id: Option<&str>,
        server: Option<&str>,
    ) -> ManagedSessionId {
        let id = ManagedSessionId::new();
        let mut raw = serde_json::json!({
            "id": id.to_string(), "task": "t", "tmux_name": name, "cwd": "/tmp",
            "state": state, "created_at": "2026-09-22T19:41:12Z",
        });
        if let Some(pane) = pane_id {
            raw["pane_id"] = serde_json::json!(pane);
        }
        if let Some(server) = server {
            raw["tmux_server"] = serde_json::json!(server);
        }
        let record: SessionRecord = serde_json::from_value(raw).expect("record");
        self.mgr
            .store
            .write()
            .await
            .upsert(record)
            .await
            .expect("seed");
        id
    }

    /// Give `id`'s record an owned workspace under a scratch managed root,
    /// holding build output only so the removal would otherwise run (#8663).
    /// Returns `(managed_root, workspace)`.
    async fn own_workspace(&self, id: &ManagedSessionId) -> (PathBuf, PathBuf) {
        let root = self.dir.path().join("managed-root");
        let ws = root.join("owner").join("repo").join("sess");
        std::fs::create_dir_all(ws.join("target")).expect("workspace");
        std::fs::write(ws.join("target/sentinel.txt"), "kept").expect("sentinel");
        let mut record = self.mgr.get(id).await.expect("record");
        record.workspace_path = Some(ws.clone());
        record.workspace_owned = true;
        self.mgr
            .store
            .write()
            .await
            .upsert(record)
            .await
            .expect("seed");
        (root, ws)
    }

    /// Every argv the fake was run with, one per line.
    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }

    /// Run `fut` with every tmux resolution pointed at the fake.
    async fn scoped<F: std::future::Future>(&self, fut: F) -> F::Output {
        crate::core::tmux::with_tmux_binary(self.bin.clone(), fut).await
    }

    fn assert_untouched(&self) {
        let calls = self.calls();
        assert!(
            calls.contains("list-sessions"),
            "the fake was reached: {calls}"
        );
        assert!(
            !calls.contains("send-keys"),
            "the live session was signalled: {calls}"
        );
        assert!(
            !calls.contains("kill-session"),
            "the live session was killed: {calls}"
        );
    }
}

/// #8935 repro: stale record a8a726e2 (pane %2077, gone) shares its tmux name
/// with a live session whose only pane is %9. Stopping the stale record moves
/// the record and neither signals nor kills the live session. Red on 889f555fc3.
#[serial_test::serial]
#[tokio::test]
async fn stopping_a_stale_record_never_signals_the_live_session_that_reused_its_name() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("errored", Some("%2077")).await;

    let stopped = f.scoped(f.mgr.stop(&id)).await.expect("record-only stop");

    assert_eq!(stopped.state, ManagedSessionState::Stopped);
    f.assert_untouched();
}

/// #8935 repro, delete half: the stale record deletes without `--force`
/// (the live session is not its runtime) and nothing reaches the live
/// session. Red on 889f555fc3, whose name-only guard refused the delete.
#[serial_test::serial]
#[tokio::test]
async fn deleting_a_stale_record_never_touches_the_live_session_that_reused_its_name() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("stopped", Some("%2077")).await;

    f.scoped(f.mgr.delete_record(&id, false))
        .await
        .expect("a stale record is not running");

    let after = f.mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Deleted);
    f.assert_untouched();
}

/// The delete's output says the live session was left running and why.
#[serial_test::serial]
#[tokio::test]
async fn deleting_a_stale_record_leaves_the_live_session_and_says_so() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("stopped", Some("%2077")).await;

    let (_, note) = f
        .scoped(f.mgr.delete_record_reporting(&id, false))
        .await
        .expect("delete");

    let note = note.expect("a live session carries the name");
    assert!(
        note.contains(LIVE) && note.contains("%2077") && note.contains("left running"),
        "{note}"
    );
    f.assert_untouched();
}

/// The stop report carries the reason the session was left running.
#[serial_test::serial]
#[tokio::test]
async fn the_stop_report_says_the_runtime_was_left_running() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1\n%10:0"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("active", Some("%2077")).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    assert!(
        matches!(&report.runtime, RuntimeTeardown::Foreign(why) if why.contains("another session now uses the name")),
        "{:?}",
        report.runtime
    );
    assert_eq!(report.record.state, ManagedSessionState::Stopped);
    f.assert_untouched();
}

/// The fix keeps its job: a record whose own pane is live is signalled and
/// its session reclaimed.
#[serial_test::serial]
#[tokio::test]
async fn stopping_a_record_whose_pane_is_live_still_kills_it() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    assert_eq!(report.runtime, RuntimeTeardown::Terminated);
    let calls = f.calls();
    // #8935 critic round: with no claude pid, the Ctrl-C goes to the
    // record's pane %9, not to the session's active pane.
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("send-keys") && l.contains("%9") && l.contains("C-c")),
        "the record's pane %9 was signalled: {calls}"
    );
    // #9004 closure condition 2: the kill names the session by the `$3` id
    // the re-check read, never by its reusable name.
    let kills: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("kill-session"))
        .collect();
    assert_eq!(
        kills,
        ["kill-session -t $3"],
        "the session was reclaimed by id: {calls}"
    );
}

/// Error arm (Fail-Open Check): the pane list cannot be read, so ownership
/// cannot be proved. The stop moves the record only and kills nothing.
#[serial_test::serial]
#[tokio::test]
async fn an_unlistable_pane_set_leaves_the_runtime_running() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: None,
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    assert!(
        matches!(&report.runtime, RuntimeTeardown::Unproven { why, .. } if why.contains("could not be listed")),
        "{:?}",
        report.runtime
    );
    f.assert_untouched();
}

/// Error arm: the session probe itself fails. The teardown kills nothing; the
/// delete refuses, as #5859 requires, rather than guess.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_session_probe_leaves_the_runtime_running() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: true,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let err = f
        .scoped(f.mgr.delete_record(&id, false))
        .await
        .expect_err("an unreadable probe refuses the delete");
    assert!(matches!(err, ManagedError::TmuxUnavailable(_)), "{err}");

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");
    let why = report.runtime.left_running().expect("left running");
    assert!(why.contains("probe"), "{why}");
    let calls = f.calls();
    assert!(!calls.contains("send-keys"), "{calls}");
    assert!(!calls.contains("kill-session"), "{calls}");
}

/// A record with no pane id cannot prove the live session is its own: the
/// stop kills nothing and the delete still needs `--force`.
#[serial_test::serial]
#[tokio::test]
async fn delete_refuses_an_unverifiable_live_name_without_force() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("active", None).await;

    let err = f
        .scoped(f.mgr.delete_record(&id, false))
        .await
        .expect_err("an unverifiable live session may be this record's");
    assert!(matches!(err, ManagedError::InvalidState(..)), "{err}");

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");
    assert!(
        report
            .runtime
            .left_running()
            .is_some_and(|why| why.contains("no pane id")),
        "{:?}",
        report.runtime
    );
    f.assert_untouched();

    let (_, note) = f
        .scoped(f.mgr.delete_record_reporting(&id, true))
        .await
        .expect("forced delete");
    assert!(note.is_some_and(|n| n.contains("left running")));
}

/// Decommission takes the same identity check: a stale record's teardown
/// leaves the live session that reused its name alone.
#[serial_test::serial]
#[tokio::test]
async fn decommissioning_a_stale_record_never_kills_the_live_session() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("stopped", Some("%2077")).await;

    let root = f.dir.path().join("managed-root");
    f.scoped(f.mgr.decommission_with_root(&id, &root, None))
        .await
        .expect("decommission");

    let after = f.mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Decommissioned);
    f.assert_untouched();
}

/// #8935 critic round, error arm (Fail-Open Check): the pane list cannot be
/// read, so the live session may be this record's claude. Decommission
/// refuses before it removes the owned workspace or tombstones the record.
/// Red on a73e5ac7f3, which discarded the verdict and decommissioned.
#[serial_test::serial]
#[tokio::test]
async fn decommission_with_an_unlistable_pane_set_changes_nothing() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: None,
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let (root, ws) = f.own_workspace(&id).await;

    let err = f
        .scoped(f.mgr.decommission_with_root(&id, &root, None))
        .await
        .expect_err("an unproven runtime refuses the decommission");

    // #8935 delta critic: the refusal says nothing was done and names both
    // ways out.
    let ManagedError::InvalidState(_, why) = &err else {
        panic!("{err}")
    };
    for part in [
        "could not be listed",
        "may still be this record's",
        "left as they are",
        "end it yourself and rerun",
        "tm session delete --force",
    ] {
        assert!(why.contains(part), "missing {part:?}: {why}");
    }
    assert!(ws.join("target/sentinel.txt").exists(), "workspace removed");
    let after = f.mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Active);
    f.assert_untouched();
}

/// #8935 (supervisor ruling): a stop of a stale record whose name another
/// session holds snapshots nothing. Red on 6310cfa9d8, which captured the
/// live session by name into the stale record's workspace.
#[serial_test::serial]
#[tokio::test]
async fn a_foreign_stop_writes_no_scrollback_and_keeps_last_cwd() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed("active", Some("%2077")).await;
    let (_, ws) = f.own_workspace(&id).await;
    let mut record = f.mgr.get(&id).await.expect("record");
    record.last_cwd = Some(PathBuf::from("/work/x"));
    f.mgr
        .store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed");

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");

    assert!(matches!(report.runtime, RuntimeTeardown::Foreign(_)));
    assert!(!ws.join(".trusty-mpm/scrollback.txt").exists());
    assert_eq!(report.record.scrollback_path, None);
    assert_eq!(report.record.last_cwd, Some(PathBuf::from("/work/x")));
    let calls = f.calls();
    assert!(!calls.contains("capture-pane"), "{calls}");
    assert!(!calls.contains("pane_current_path"), "{calls}");
    f.assert_untouched();
}

/// #8935 delta critic: a failed session probe refuses decommission and says
/// liveness is unknown rather than calling the session live.
#[serial_test::serial]
#[tokio::test]
async fn decommission_with_a_failed_probe_says_liveness_unknown() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: true,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let (root, ws) = f.own_workspace(&id).await;

    let err = f
        .scoped(f.mgr.decommission_with_root(&id, &root, None))
        .await
        .expect_err("an unreadable probe refuses the decommission");

    let ManagedError::InvalidState(_, why) = &err else {
        panic!("{err}")
    };
    assert!(why.contains("liveness unknown"), "{why}");
    assert!(!why.contains("the live tmux session"), "{why}");
    assert!(ws.join("target/sentinel.txt").exists(), "workspace removed");
    f.assert_untouched();
}

/// #8935 post-grace re-check, `Unproven` arm: the record's pane %9 is
/// signalled, then the pane list cannot be read. Decommission refuses, says
/// the pane was signalled, keeps the workspace and the record, and kills
/// nothing.
#[serial_test::serial]
#[tokio::test]
async fn a_pane_list_lost_after_the_signal_refuses_decommission() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: Some(AfterSignal::Unlistable),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let (root, ws) = f.own_workspace(&id).await;

    let err = f
        .scoped(f.mgr.decommission_with_root(&id, &root, None))
        .await
        .expect_err("ownership lost after the signal refuses the decommission");

    let ManagedError::InvalidState(_, why) = &err else {
        panic!("{err}")
    };
    assert!(why.contains("signalled"), "{why}");
    assert!(!why.contains("left as they are"), "{why}");
    assert!(ws.join("target/sentinel.txt").exists(), "workspace removed");
    let after = f.mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Active);
    let calls = f.calls();
    assert!(
        calls.contains("send-keys"),
        "the pane was signalled: {calls}"
    );
    assert!(!calls.contains("kill-session"), "{calls}");
}

/// #8935 critic round: `--force` deletes the record even when the tmux probe
/// itself fails, and the note says so. Red on a73e5ac7f3, whose `?` turned
/// the probe error into `TmuxUnavailable` under `--force` too.
#[serial_test::serial]
#[tokio::test]
async fn a_forced_delete_survives_a_failed_tmux_probe() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: true,
        panes: Some("%9:1"),
        after_signal: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let (_, note) = f
        .scoped(f.mgr.delete_record_reporting(&id, true))
        .await
        .expect("a forced delete does not need tmux");

    let note = note.expect("the note names the failed probe");
    assert!(note.contains("probe failed"), "{note}");
    let after = f.mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Deleted);
    f.assert_untouched();
}

/// #8935 post-grace re-check, `Foreign` arm: the record's pane %9 is signalled,
/// and by the re-check the name holds only %10. The session is not killed.
#[serial_test::serial]
#[tokio::test]
async fn a_pane_that_changes_hands_during_the_grace_window_is_not_killed() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: Some(AfterSignal::Panes("%10:1")),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let record = f.mgr.get(&id).await.expect("record");

    let teardown = f
        .scoped(f.mgr.graceful_terminate_runtime(&record, "test"))
        .await
        .expect("teardown");

    assert!(
        matches!(&teardown, RuntimeTeardown::Foreign(why) if why.contains("%9")),
        "{teardown:?}"
    );
    let calls = f.calls();
    assert!(
        calls.contains("send-keys"),
        "the pane was signalled: {calls}"
    );
    assert!(!calls.contains("kill-session"), "{calls}");
}

/// #8935 post-grace re-check, `Absent` arm: the session ended during the
/// grace window. The teardown reports `Terminated` and kills nothing.
#[serial_test::serial]
#[tokio::test]
async fn a_session_gone_after_the_grace_window_is_terminated_without_a_kill() {
    let f = Fixture::new(FakeTmux {
        sessions_fail: false,
        panes: Some("%9:1"),
        after_signal: Some(AfterSignal::Gone),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let record = f.mgr.get(&id).await.expect("record");

    let teardown = f
        .scoped(f.mgr.graceful_terminate_runtime(&record, "test"))
        .await
        .expect("teardown");

    assert_eq!(teardown, RuntimeTeardown::Terminated);
    let calls = f.calls();
    assert!(
        calls.contains("send-keys"),
        "the pane was signalled: {calls}"
    );
    assert!(!calls.contains("kill-session"), "{calls}");
}

/// #9004 closure condition 1: the record's pane %9 was captured on a server
/// that has since restarted, and the new server's session under the same name
/// holds a new %9. The stop and the delete leave it alone.
#[serial_test::serial]
#[tokio::test]
async fn a_record_from_before_a_server_restart_never_owns_the_new_session() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed_on("active", Some("%9"), Some(OLD_SERVER)).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");
    assert!(
        matches!(&report.runtime, RuntimeTeardown::Foreign(why)
            if why.contains(OLD_SERVER) && why.contains(LIVE_SERVER)),
        "{:?}",
        report.runtime
    );
    f.scoped(f.mgr.delete_record(&id, false))
        .await
        .expect("a record from an earlier server is not running");

    f.assert_untouched();
}

/// #9004 closure condition 3: a record written before the server identity
/// existed cannot prove its %9 is not a restarted server's. Decommission
/// refuses, the stop is record-only, the delete needs `--force`.
#[serial_test::serial]
#[tokio::test]
async fn a_record_without_a_server_identity_never_owns_its_pane() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed("active", Some("%9")).await;
    let (root, ws) = f.own_workspace(&id).await;

    let err = f
        .scoped(f.mgr.decommission_with_root(&id, &root, None))
        .await
        .expect_err("an unproven runtime refuses the decommission");
    assert!(err.to_string().contains("no tmux server identity"), "{err}");
    assert!(ws.join("target/sentinel.txt").exists(), "workspace removed");

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");
    assert!(
        matches!(&report.runtime, RuntimeTeardown::Unproven { why, signalled: false, .. }
            if why.contains("no tmux server identity")),
        "{:?}",
        report.runtime
    );
    let err = f
        .scoped(f.mgr.delete_record(&id, false))
        .await
        .expect_err("the live session may be this record's");
    assert!(matches!(err, ManagedError::InvalidState(..)), "{err}");

    f.assert_untouched();
}

/// #9004 error arm (Fail-Open Check): the discriminator read,
/// `display-message`, fails. Ownership is unproven and nothing is killed.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_leaves_the_runtime_running() {
    let f = Fixture::new(FakeTmux {
        identity: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");

    assert!(
        matches!(&report.runtime, RuntimeTeardown::Unproven { why, .. }
            if why.contains("could not be read")),
        "{:?}",
        report.runtime
    );
    f.assert_untouched();
}

/// #9004 error arm: tmux 3.6b answers `display-message -t %N` for a missing
/// pane with exit 0 and empty fields. That answer proves nothing.
#[serial_test::serial]
#[tokio::test]
async fn a_missing_pane_identity_answer_leaves_the_runtime_running() {
    let f = Fixture::new(FakeTmux {
        identity: Some("||4242:1700000000|"),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");

    assert!(
        matches!(&report.runtime, RuntimeTeardown::Unproven { why, .. }
            if why.contains("unreadable identity")),
        "{:?}",
        report.runtime
    );
    f.assert_untouched();
}

/// #9004 error arm: the pane's identity places it in another session than
/// the one that listed it, so ownership is unproven.
#[serial_test::serial]
#[tokio::test]
async fn a_pane_identity_naming_another_session_leaves_the_runtime_running() {
    let f = Fixture::new(FakeTmux {
        identity: Some("%9|$3|4242:1700000000|tmpm-test-9004-other"),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");

    assert!(
        matches!(&report.runtime, RuntimeTeardown::Unproven { why, .. }
            if why.contains("tmpm-test-9004-other")),
        "{:?}",
        report.runtime
    );
    f.assert_untouched();
}

/// #9004 post-grace re-check, error arm: the record's pane %9 is signalled,
/// then its identity cannot be read. Nothing is killed, and the verdict says
/// the pane was signalled.
#[serial_test::serial]
#[tokio::test]
async fn an_identity_lost_after_the_signal_is_not_killed() {
    let f = Fixture::new(FakeTmux {
        after_signal: Some(AfterSignal::Identity(None)),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let record = f.mgr.get(&id).await.expect("record");

    let teardown = f
        .scoped(f.mgr.graceful_terminate_runtime(&record, "test"))
        .await
        .expect("teardown");

    assert!(
        matches!(
            &teardown,
            RuntimeTeardown::Unproven {
                signalled: true,
                ..
            }
        ),
        "{teardown:?}"
    );
    let calls = f.calls();
    assert!(
        calls.contains("send-keys"),
        "the pane was signalled: {calls}"
    );
    assert!(!calls.contains("kill-session"), "{calls}");
}

/// #9004 TOCTOU: the server restarts during the grace window and a new
/// session under the name gets a new %9. The re-check sees another server, so
/// nothing is killed.
#[serial_test::serial]
#[tokio::test]
async fn a_server_restart_during_the_grace_window_is_not_killed() {
    let f = Fixture::new(FakeTmux {
        after_signal: Some(AfterSignal::Identity(Some(
            "%9|$0|5555:1700000099|tmpm-test-8935-reuse",
        ))),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    let record = f.mgr.get(&id).await.expect("record");

    let teardown = f
        .scoped(f.mgr.graceful_terminate_runtime(&record, "test"))
        .await
        .expect("teardown");

    assert!(
        matches!(&teardown, RuntimeTeardown::Foreign(why) if why.contains("5555:1700000099")),
        "{teardown:?}"
    );
    let calls = f.calls();
    assert!(!calls.contains("kill-session"), "{calls}");
}

/// The private server's `(pane_id, server)` for `name`'s active pane, plus
/// the pane's pid, which changes if the pane is replaced.
fn live_pane(server: &PrivateTmuxServer, name: &str) -> Option<(String, String, String)> {
    let line = server.query(&[
        "display-message",
        "-p",
        "-t",
        &trusty_common::tmux::exact_window_target(name),
        "#{pane_id}|#{pid}:#{start_time}|#{pane_pid}",
    ])?;
    let mut parts = line.split('|').map(str::to_owned);
    Some((parts.next()?, parts.next()?, parts.next()?))
}

/// A manager over a private tmux server's shim, or `None` without tmux.
async fn live_fixture(tag: &str) -> Option<(PrivateTmuxServer, Fixture)> {
    if !ScratchTmuxSession::tmux_available(TMUX) {
        eprintln!("tmux not available; skipping");
        return None;
    }
    let server = PrivateTmuxServer::new(TMUX, tag);
    let bin = PathBuf::from(server.shim_bin());
    let f = Fixture::with_bin(crate::test_support::hermetic_temp_dir(), bin).await;
    Some((server, f))
}

/// The pane command for a live fixture: Ctrl-C does not end it, so only a
/// kill can.
const LIVE_PANE: &str = "trap '' INT; sleep 600";

const TMUX: &str = "tmux";

/// #9004 closure condition 1, on a real tmux: a record captures `%0` on a
/// private server; the server exits and restarts, and `tm fleet init`'s
/// fixed launch order gives a new session the same name and the same `%0`.
/// Stopping and deleting the stale record leaves the new session's pane
/// untouched. Red on e4fbd4d39e, which killed it.
#[serial_test::serial]
#[tokio::test]
async fn live_a_server_restart_never_hands_a_stale_record_the_new_session() {
    let Some((server, f)) = live_fixture("9004-restart").await else {
        return;
    };
    let name = reserved_session_name("9004-restart");
    let first = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, LIVE_PANE);
    let (pane, old_server, _) = live_pane(&server, &name).expect("the first session is live");
    drop(first);
    // The server exits with its last session; make sure it is gone before
    // the next session starts a new one.
    let _ = server.query(&["kill-server"]);
    for _ in 0..50 {
        if server.query(&["list-sessions"]).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _second = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, LIVE_PANE);
    let before = live_pane(&server, &name).expect("the second session is live");
    assert_eq!(
        before.0, pane,
        "precondition: the restarted server reused the pane id"
    );
    assert_ne!(before.1, old_server, "precondition: a new server instance");
    let id = f
        .seed_named(&name, "active", Some(&pane), Some(&old_server))
        .await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");
    assert!(
        matches!(report.runtime, RuntimeTeardown::Foreign(_)),
        "{:?}",
        report.runtime
    );
    f.scoped(f.mgr.delete_record(&id, false))
        .await
        .expect("a record from an earlier server is not running");

    assert_eq!(
        live_pane(&server, &name),
        Some(before),
        "the new session's pane was killed or replaced"
    );
}

/// #9004 closure condition 2, on a real tmux: a record captured on the live
/// server owns its session, which the stop kills by `$N` id. A session whose
/// name extends the record's survives.
#[serial_test::serial]
#[tokio::test]
async fn live_a_record_on_its_own_server_is_killed_by_session_id() {
    let Some((server, f)) = live_fixture("9004-own").await else {
        return;
    };
    let name = reserved_session_name("9004-own");
    let sibling_name = format!("{name}-suffix");
    let _own = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, LIVE_PANE);
    let _sibling =
        ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &sibling_name, LIVE_PANE);
    let (pane, live_server, _) = live_pane(&server, &name).expect("the session is live");
    let sibling = live_pane(&server, &sibling_name).expect("the sibling is live");
    let id = f
        .seed_named(&name, "active", Some(&pane), Some(&live_server))
        .await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    assert_eq!(report.runtime, RuntimeTeardown::Terminated);
    assert!(
        !ScratchTmuxSession::exists_on_socket(TMUX, Some(server.name()), &name),
        "the record's own session survived the stop"
    );
    assert_eq!(live_pane(&server, &sibling_name), Some(sibling));
}

// #9101: the pane operations reuse this fixture, so they live in a child module.
#[path = "pane_ops_identity_tests.rs"]
mod pane_ops;
