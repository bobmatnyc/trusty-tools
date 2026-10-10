//! Tests for #9101: a pane operation acts only on a pane proven to be the
//! record's own on the live tmux server.
//!
//! A child of `runtime_identity_tests`, whose fixture it reuses: the
//! production `RealTmuxDriver` against a scripted fake tmux that logs its
//! argv, or (`live_*`) a private `-L` tmux server that skips without tmux.

use super::*;
use crate::core::sm::control::Submit;
use crate::session_manager::runtime_identity::{RuntimeOwnership, runtime_ownership};

/// Text no pane in these tests prints unless a send reached it.
const MARK: &str = "mark-9101";

/// What a pane-touching tmux call looks like in the fake's argv log.
const PANE_VERBS: [&str; 3] = ["send-keys", "capture-pane", "kill-session"];

/// Run every pane operation against the Active record `active` and the
/// Stopped record `stopped`, and assert each refused with no pane touched.
async fn assert_every_pane_op_refuses(
    f: &Fixture,
    active: &ManagedSessionId,
    stopped: &ManagedSessionId,
) {
    let m = &f.mgr;
    for submit in [Submit::Enter, Submit::NoSubmit, Submit::Interrupt] {
        let r = f.scoped(m.inject(active, MARK, submit)).await;
        assert!(r.is_err(), "inject {submit:?} acted: {r:?}");
    }
    let r = f.scoped(m.send_input(active, MARK)).await;
    assert!(r.is_err(), "send_input acted: {r:?}");
    let r = f.scoped(m.answer_decision(active, MARK)).await;
    assert!(r.is_err(), "answer_decision acted: {r:?}");
    let r = f.scoped(m.observe(active, 20)).await;
    assert!(r.is_err(), "observe read the pane: {r:?}");
    let r = f.scoped(m.capture_pane(active, 20)).await;
    assert!(r.is_err(), "capture_pane read the pane: {r:?}");
    let r = f.scoped(m.capture_pane_by_tmux_name(LIVE, 20)).await;
    assert!(r.is_none(), "the idle-reaper capture read the pane: {r:?}");
    let r = f.scoped(m.resume(stopped)).await;
    assert!(r.is_err(), "resume adopted the pane: {r:?}");
    let state = m.get(stopped).await.expect("record").state;
    assert_eq!(
        state,
        ManagedSessionState::Stopped,
        "a refused resume moved the record"
    );
    f.scoped(m.shutdown()).await;

    let calls = f.calls();
    assert!(
        calls.contains("list-sessions"),
        "the fake was reached: {calls}"
    );
    for verb in PANE_VERBS {
        assert!(
            !calls.contains(verb),
            "a pane operation ran `{verb}`: {calls}"
        );
    }
}

/// Seed an Active and a Stopped record named [`LIVE`], both on `pane_id`
/// and `server`, and assert every pane operation refuses.
async fn refuses_with(fake: FakeTmux, pane_id: Option<&str>, server: Option<&str>) {
    let f = Fixture::new(fake).await;
    let active = f.seed_on("active", pane_id, server).await;
    let stopped = f.seed_on("stopped", pane_id, server).await;
    assert_every_pane_op_refuses(&f, &active, &stopped).await;
}

/// #9101 repro on the scripted fake: a record captured pane %9 on the server
/// before a restart; the live server lists a %9 of its own. No pane
/// operation sends keys to it, reads it, kills it or adopts it. Red on
/// b016bf17fa, which sent keys and read the pane.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_sends_keys() {
    refuses_with(FakeTmux::default(), Some("%9"), Some(OLD_SERVER)).await;
}

/// Fail open: a record written before #9004 has no server identity, so its
/// pane id cannot be told from a restarted server's.
#[serial_test::serial]
#[tokio::test]
async fn a_record_without_a_server_identity_refuses_every_pane_operation() {
    refuses_with(FakeTmux::default(), Some("%9"), None).await;
}

/// Fail open: a record with no pane id has nothing to prove ownership with,
/// so the old session-scoped fallback is gone.
#[serial_test::serial]
#[tokio::test]
async fn a_record_without_a_pane_id_refuses_every_pane_operation() {
    refuses_with(FakeTmux::default(), None, None).await;
}

/// Fail open: the pane list cannot be read.
#[serial_test::serial]
#[tokio::test]
async fn an_unlistable_pane_set_refuses_every_pane_operation() {
    let fake = FakeTmux {
        panes: None,
        ..FakeTmux::default()
    };
    refuses_with(fake, Some("%9"), Some(LIVE_SERVER)).await;
}

/// Fail open: the pane-identity read exits non-zero.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_refuses_every_pane_operation() {
    let fake = FakeTmux {
        identity: None,
        ..FakeTmux::default()
    };
    refuses_with(fake, Some("%9"), Some(LIVE_SERVER)).await;
}

/// Fail open: tmux exits 0 with empty fields, its answer for a missing pane.
#[serial_test::serial]
#[tokio::test]
async fn a_missing_pane_identity_answer_refuses_every_pane_operation() {
    let fake = FakeTmux {
        identity: Some("|||"),
        ..FakeTmux::default()
    };
    refuses_with(fake, Some("%9"), Some(LIVE_SERVER)).await;
}

/// Fail open: the identity places the pane in another session.
#[serial_test::serial]
#[tokio::test]
async fn a_pane_identity_naming_another_session_refuses_every_pane_operation() {
    let fake = FakeTmux {
        identity: Some("%9|$3|4242:1700000000|tmpm-test-8935-other"),
        ..FakeTmux::default()
    };
    refuses_with(fake, Some("%9"), Some(LIVE_SERVER)).await;
}

/// Control: a record whose pane is on the live server still gets its keys,
/// its capture and its shutdown kill, so the refusals above are the gate's.
#[serial_test::serial]
#[tokio::test]
async fn an_owned_pane_still_receives_keys_capture_and_shutdown() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    f.scoped(f.mgr.inject(&id, MARK, Submit::Enter))
        .await
        .expect("inject into the owned pane");
    f.scoped(f.mgr.capture_pane(&id, 20))
        .await
        .expect("capture the owned pane");
    f.scoped(f.mgr.shutdown()).await;
    let calls = f.calls();
    for verb in PANE_VERBS {
        assert!(
            calls.contains(verb),
            "the owned pane never saw `{verb}`: {calls}"
        );
    }
    // #9101: the shutdown kills by `$N` session id, never by exact name.
    assert!(calls.contains("kill-session -t $"), "{calls}");
    assert!(!calls.contains("kill-session -t ="), "{calls}");
}

/// Fail open: a shutdown neither signals nor kills a session whose pane
/// identity cannot be read.
#[serial_test::serial]
#[tokio::test]
async fn shutdown_skips_a_record_whose_pane_identity_is_unreadable() {
    let fake = FakeTmux {
        identity: None,
        ..FakeTmux::default()
    };
    let f = Fixture::new(fake).await;
    f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    f.scoped(f.mgr.shutdown()).await;
    let calls = f.calls();
    assert!(!calls.contains("send-keys"), "{calls}");
    assert!(!calls.contains("kill-session"), "{calls}");
    assert!(
        calls.contains("display-message"),
        "the identity was read: {calls}"
    );
}

/// The pane command for the live tests: prints [`LIVE_TEXT`], and Ctrl-C
/// does not end it, so only a kill can.
const LIVE_PRINTING_PANE: &str = "echo live-9101; trap '' INT; sleep 600";

/// Like [`LIVE_PRINTING_PANE`], but the text arrives a second late, as it does
/// on a loaded host.
const LATE_PRINTING_PANE: &str = "sleep 1; echo live-9101; trap '' INT; sleep 600";

/// What the restarted server's pane prints.
const LIVE_TEXT: &str = "live-9101";

/// A private server that ran a session `name` holding pane `%N`, exited, and
/// restarted with a new session of the same name and the same `%N`.
/// Returns the server, the manager fixture, the name, the new session's
/// guard, the pane id and the OLD server identity a stale record carries.
struct Restarted {
    server: PrivateTmuxServer,
    f: Fixture,
    name: String,
    _second: ScratchTmuxSession,
    pane: String,
    old_server: String,
}

impl Restarted {
    /// `None` without tmux.
    async fn new(tag: &str) -> Option<Self> {
        Self::with_pane_command(tag, LIVE_PRINTING_PANE).await
    }

    /// [`Restarted::new`] with the restarted session running `pane_command`.
    async fn with_pane_command(tag: &str, pane_command: &str) -> Option<Self> {
        let (server, f) = live_fixture(tag).await?;
        let name = reserved_session_name(tag);
        let first =
            ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, LIVE_PANE);
        let (pane, old_server, _) = live_pane(&server, &name).expect("the first session is live");
        drop(first);
        let _ = server.query(&["kill-server"]);
        for _ in 0..50 {
            if server.query(&["list-sessions"]).is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let second =
            ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, pane_command);
        let now = live_pane(&server, &name).expect("the second session is live");
        assert_eq!(
            now.0, pane,
            "precondition: the restarted server reused the pane id"
        );
        assert_ne!(now.1, old_server, "precondition: a new server instance");
        let restarted = Self {
            server,
            f,
            name,
            _second: second,
            pane,
            old_server,
        };
        restarted.wait_until_printed();
        Some(restarted)
    }

    /// Why: #9587: the pane prints asynchronously, so a single read right
    /// after the spawn loses the race on a loaded host.
    /// What: polls the new pane every 50 ms for up to 5 s until it shows
    /// [`LIVE_TEXT`]; panics with the last screen on a real timeout.
    /// Test: `restarted_returns_only_once_the_pane_has_printed`.
    fn wait_until_printed(&self) {
        let mut screen = self.screen();
        for _ in 0..100 {
            if screen.contains(LIVE_TEXT) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            screen = self.screen();
        }
        assert!(
            screen.contains(LIVE_TEXT),
            "precondition: the restarted pane never printed {LIVE_TEXT:?} within 5s: {screen:?}"
        );
    }

    /// A stale record in `state`, captured on the server before the restart.
    async fn stale(&self, state: &str) -> ManagedSessionId {
        self.f
            .seed_named(&self.name, state, Some(&self.pane), Some(&self.old_server))
            .await
    }

    /// The new session's pane text.
    fn screen(&self) -> String {
        let target = trusty_common::tmux::exact_pane_target(&self.name, &self.pane);
        self.server
            .query(&["capture-pane", "-p", "-t", &target])
            .unwrap_or_default()
    }
}

/// #9587: a `Restarted` harness hands back a pane that has already printed,
/// even when the print lands late.
#[serial_test::serial]
#[tokio::test]
async fn restarted_returns_only_once_the_pane_has_printed() {
    let Some(r) = Restarted::with_pane_command("9587-late", LATE_PRINTING_PANE).await else {
        return;
    };
    let screen = r.screen();
    assert!(
        screen.contains(LIVE_TEXT),
        "the harness returned before the pane printed: {screen:?}"
    );
}

/// #9101 send class, on a real tmux: no send reaches the restarted server's
/// pane that took the stale record's name and `%N`. Red on b016bf17fa.
#[serial_test::serial]
#[tokio::test]
async fn live_a_stale_record_after_a_server_restart_never_sends_keys() {
    let Some(r) = Restarted::new("9101-send").await else {
        return;
    };
    let id = r.stale("active").await;
    let m = &r.f.mgr;
    let sent = [
        r.f.scoped(m.inject(&id, MARK, Submit::NoSubmit)).await,
        r.f.scoped(m.send_input(&id, MARK)).await,
        r.f.scoped(m.answer_decision(&id, MARK)).await,
    ];
    // tmux echoes typed keys through the pane's tty asynchronously.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let screen = r.screen();
    assert!(
        !screen.contains(MARK),
        "keys reached the live pane: {screen:?}"
    );
    assert!(sent.iter().all(Result::is_err), "{sent:?}");
}

/// #9101 capture class, on a real tmux: no read returns the restarted
/// server's pane. Red on b016bf17fa.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_reads_the_pane() {
    let Some(r) = Restarted::new("9101-read").await else {
        return;
    };
    let id = r.stale("active").await;
    let m = &r.f.mgr;
    let observed = r.f.scoped(m.observe(&id, 20)).await;
    let captured = r.f.scoped(m.capture_pane(&id, 20)).await;
    let by_name = r.f.scoped(m.capture_pane_by_tmux_name(&r.name, 20)).await;
    assert!(
        r.screen().contains(LIVE_TEXT),
        "precondition: the pane printed"
    );
    assert!(
        observed.is_err(),
        "observe read the live pane: {observed:?}"
    );
    assert!(
        captured.is_err(),
        "capture_pane read the live pane: {captured:?}"
    );
    assert_eq!(by_name, None, "the idle-reaper capture read the live pane");
}

/// #9101 kill class, on a real tmux: the daemon shutdown leaves the restarted
/// server's session running. Red on b016bf17fa, which killed it by name.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_is_never_killed_by_shutdown() {
    let Some(r) = Restarted::new("9101-kill").await else {
        return;
    };
    r.stale("active").await;
    let before = live_pane(&r.server, &r.name);
    r.f.scoped(r.f.mgr.shutdown()).await;
    assert_eq!(
        live_pane(&r.server, &r.name),
        before,
        "shutdown killed the live session"
    );
}

/// #9101 adopt class, on a real tmux: resuming the stale record refuses and
/// adopts nothing; the record stays Stopped. Red on b016bf17fa, which
/// re-attached to the live pane and marked the record Active.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_is_never_adopted_by_resume() {
    let Some(r) = Restarted::new("9101-adopt").await else {
        return;
    };
    let id = r.stale("stopped").await;
    let before = live_pane(&r.server, &r.name);
    let resumed = r.f.scoped(r.f.mgr.resume(&id)).await;
    assert!(
        matches!(&resumed, Err(ManagedError::InvalidState(_, why)) if why.contains("stale")),
        "{resumed:?}"
    );
    let record = r.f.mgr.get(&id).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Stopped);
    assert_eq!(record.tmux_server.as_deref(), Some(r.old_server.as_str()));
    assert_eq!(
        live_pane(&r.server, &r.name),
        before,
        "resume replaced the pane"
    );
}

/// #9101 recovery, on a real tmux: once a record's session has ended, a
/// resume creates a new pane and captures it with the live server, so the
/// record owns its session again and pane operations reach it.
#[serial_test::serial]
#[tokio::test]
async fn a_resume_that_recreates_the_pane_makes_the_record_ownable() {
    let Some((server, f)) = live_fixture("9101-recover").await else {
        return;
    };
    let name = reserved_session_name("9101-recover");
    let id = f
        .seed_named(&name, "stopped", Some("%0"), Some(OLD_SERVER))
        .await;
    let ws = std::fs::canonicalize(f.dir.path()).expect("workspace");
    let mut record = f.mgr.get(&id).await.expect("record");
    record.cwd = ws;
    f.mgr
        .store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed");

    let resumed = f.scoped(f.mgr.resume(&id)).await.expect("resume recreates");

    let (pane, live_server, _) = live_pane(&server, &name).expect("the resumed session is live");
    assert_eq!(resumed.pane_id.as_deref(), Some(pane.as_str()));
    assert_eq!(resumed.tmux_server.as_deref(), Some(live_server.as_str()));
    let ownership = f
        .scoped(async { runtime_ownership(&resumed, f.mgr.tmux.as_ref()) })
        .await
        .expect("probe");
    assert!(
        matches!(ownership, RuntimeOwnership::Owned { .. }),
        "{ownership:?}"
    );
    f.scoped(f.mgr.inject(&id, MARK, Submit::NoSubmit))
        .await
        .expect("the resumed record's pane takes keys");
}

// #9101: the callers outside `SessionManager`'s own pane operations reuse the
// restarted-server fixture above, so they live in a child module.
#[path = "pane_callers_identity_tests.rs"]
mod callers;
