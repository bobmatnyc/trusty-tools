//! Tests for the #8942 Architect registration. Hermetic: a scratch manager,
//! the recording `FakeTmuxDriver`, and a fake [`BindingVerifier`]; nothing
//! reaches the daemon or `~/.trusty-mpm`. Only the two pane-probe tests run
//! real tmux, on a private `-L` server, never the host's.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::real_tmux::RealTmuxDriver;
use super::super::tests::{FakeTmuxDriver, seed_record};
use super::{BindingVerifier, RegisterError, SupervisorRegistration};
use crate::core::process::PaneClaude;
use crate::session_manager::{
    KillVerdict, ManagedSessionId, ManagedSessionState, SessionKind, SessionManager,
    SupervisorFloor,
};
use crate::test_support::tmux_session::{PrivateTmuxServer, ScratchTmuxSession};

/// A verifier with a fixed answer that counts its calls.
struct Verifier {
    answer: Result<(), String>,
    calls: AtomicUsize,
}

impl Verifier {
    fn bound() -> Self {
        Self {
            answer: Ok(()),
            calls: AtomicUsize::new(0),
        }
    }

    fn unbound(why: &str) -> Self {
        Self {
            answer: Err(why.to_owned()),
            calls: AtomicUsize::new(0),
        }
    }
}

impl BindingVerifier for Verifier {
    fn verify(&self, _dir: &Path, _session: &str) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

async fn manager() -> (tempfile::TempDir, Arc<FakeTmuxDriver>, SessionManager) {
    let dir = crate::test_support::hermetic_temp_dir();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");
    (dir, fake, mgr)
}

/// A registration for an Architect directory inside `root`.
fn registration(root: &tempfile::TempDir, poll: Option<&str>) -> SupervisorRegistration {
    let dir = root.path().join("architect");
    std::fs::create_dir_all(&dir).expect("architect dir");
    SupervisorRegistration {
        dir,
        session: "tm-arch".into(),
        poll_session: poll.map(str::to_owned),
        collector_session: None,
    }
}

/// Seed an ordinary record in `state` that carries tmux name `name`.
async fn seed_named(
    mgr: &SessionManager,
    root: &tempfile::TempDir,
    state: ManagedSessionState,
    name: &str,
) -> ManagedSessionId {
    seed_kind(mgr, root, state, SessionKind::Ordinary, name).await
}

/// Seed a record of `kind` in `state` that carries tmux name `name`.
async fn seed_kind(
    mgr: &SessionManager,
    root: &tempfile::TempDir,
    state: ManagedSessionState,
    kind: SessionKind,
    name: &str,
) -> ManagedSessionId {
    let id = ManagedSessionId::new();
    seed_record(mgr, root, id, state, false).await;
    let mut record = mgr.get(&id).await.expect("seeded");
    record.tmux_name = name.to_owned();
    record.kind = kind;
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("rename");
    id
}

/// Fail-Open Check: a verifier that cannot bind the session writes nothing,
/// so no caller can make an arbitrary session unkillable.
#[tokio::test]
async fn register_supervisor_refuses_an_unbound_session() {
    let (root, _fake, mgr) = manager().await;
    let reg = registration(&root, None);
    let err = mgr
        .register_supervisor(&reg, &Verifier::unbound("no launch record"))
        .await
        .expect_err("an unbound session is refused");
    assert!(
        matches!(&err, RegisterError::Unbound(why) if why.contains("no launch record")),
        "{err:?}"
    );
    assert!(mgr.list().await.is_empty(), "nothing is written");
}

/// Design §4: a relaunch upserts the same id; another LIVE record with the
/// Architect's name is marked deleted, record-only; a stopped one (like the
/// #8935 tombstone) is left as it is.
#[tokio::test]
async fn relaunching_the_architect_replaces_its_record() {
    let (root, fake, mgr) = manager().await;
    let reg = registration(&root, None);
    let live = seed_named(&mgr, &root, ManagedSessionState::Active, "tm-arch").await;
    let stopped = seed_named(&mgr, &root, ManagedSessionState::Stopped, "tm-arch").await;

    let first = mgr
        .register_supervisor(&reg, &Verifier::bound())
        .await
        .expect("registers");
    let second = mgr
        .register_supervisor(&reg, &Verifier::bound())
        .await
        .expect("registers again");

    let id = ManagedSessionId::for_supervisor(&reg.dir, "supervisor");
    assert_eq!(first.registered[0].id, id.to_string());
    assert_eq!(second.registered, first.registered);
    assert_eq!(first.replaced, vec![live.to_string()]);
    assert!(second.replaced.is_empty(), "{second:?}");
    let record = mgr.get(&id).await.expect("the Architect's record");
    assert_eq!(record.kind, SessionKind::Supervisor);
    assert_eq!(record.state, ManagedSessionState::Active);
    assert!(!record.workspace_owned);
    let supervisors = mgr.list().await;
    let supervisors = supervisors
        .iter()
        .filter(|r| r.kind == SessionKind::Supervisor);
    assert_eq!(supervisors.count(), 1, "one live row, never two");
    let live_state = mgr.get(&live).await.expect("record").state;
    assert_eq!(live_state, ManagedSessionState::Deleted);
    let stopped_state = mgr.get(&stopped).await.expect("record").state;
    assert_eq!(stopped_state, ManagedSessionState::Stopped);
    assert!(fake.kill_calls.lock().unwrap().is_empty(), "record-only");
    assert!(matches!(
        SupervisorFloor::for_data_dir(mgr.data_dir()).verdict("tm-arch"),
        KillVerdict::Protected(_)
    ));
}

/// Fail-Open Check, #8942 critic HIGH: a helper name not derived from the
/// Architect's (a PM's own session, say) is a 400 before the binding check,
/// so one POST cannot make an arbitrary pane in the directory unkillable.
#[tokio::test]
async fn a_helper_not_named_after_the_architect_is_refused() {
    let (root, fake, mgr) = manager().await;
    *fake.pane_cwd_override.lock().unwrap() = Some(root.path().join("architect"));
    let verifier = Verifier::bound();
    let renamed_poller = registration(&root, Some("tm-pm-session"));
    let mut foreign_collector = registration(&root, None);
    foreign_collector.collector_session = Some("tm-pm-session-collector".into());
    for reg in [renamed_poller, foreign_collector] {
        let err = mgr
            .register_supervisor(&reg, &verifier)
            .await
            .expect_err("a helper not named after the Architect is refused");
        assert!(
            matches!(&err, RegisterError::Invalid(why) if why.contains("tm-pm-session")),
            "{err:?}"
        );
    }
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
    assert!(mgr.list().await.is_empty(), "nothing is written");
    assert_eq!(
        SupervisorFloor::for_data_dir(mgr.data_dir()).verdict("tm-pm-session"),
        KillVerdict::Permit
    );
}

/// Fail-Open Check, #8942 critic HIGH: a correctly named helper whose pane
/// runs `claude` is a PM or an Architect, so it gets no protected record.
#[tokio::test]
async fn a_helper_whose_pane_runs_claude_is_not_registered() {
    let (root, fake, mgr) = manager().await;
    let reg = registration(&root, Some("tm-arch-poll"));
    let poll_id = ManagedSessionId::for_supervisor(&reg.dir, "-poll");
    *fake.pane_cwd_override.lock().unwrap() = Some(reg.dir.clone());
    for (answer, registered) in [
        (PaneClaude::Present, false),
        (PaneClaude::Unknown, false),
        (PaneClaude::Absent, true),
    ] {
        *fake.pane_claude_override.lock().unwrap() = Some(answer);
        let report = mgr
            .register_supervisor(&reg, &Verifier::bound())
            .await
            .expect("the Architect registers");
        assert_eq!(
            report.registered.len() == 2,
            registered,
            "{answer:?}: {report:?}"
        );
        let live = mgr
            .get(&poll_id)
            .await
            .is_ok_and(|r| !r.state.is_terminal());
        assert_eq!(live, registered, "{answer:?}");
    }
}

/// Fail-Open Check, #8942 delta critic MEDIUM: a helper pane whose own
/// process is `claude` (`tmux new -s x claude`) is not registered. Real tmux
/// on a private `-L` server; the fake `claude` is `sleep` under that name.
#[tokio::test]
async fn a_helper_whose_pane_process_is_claude_is_not_registered() {
    let Some((server, root, mgr)) = real_tmux_manager("claude").await else {
        return;
    };
    let reg = tmux_registration(&root);
    let fake_claude = root.path().join("claude");
    // macOS names a process after the symlink it ran through and kills a
    // copied system binary; Linux names it after the file, so copy there.
    #[cfg(target_os = "macos")]
    std::os::unix::fs::symlink("/bin/sleep", &fake_claude).expect("fake claude");
    #[cfg(not(target_os = "macos"))]
    std::fs::copy("/bin/sleep", &fake_claude).expect("fake claude");
    let pane = helper_pane(
        &server,
        &reg,
        &format!("exec '{}' 60", fake_claude.display()),
    );
    // Fixture precondition: the pane process has exec'd into `claude`.
    let named_claude = (0..50).any(|_| {
        let pid = server.query(&["display-message", "-t", pane.name(), "-p", "#{pane_pid}"]);
        let named = pid
            .and_then(|p| p.parse().ok())
            .is_some_and(crate::core::process::process_name_is_claude);
        named || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            false
        }
    });
    assert!(named_claude, "the pane process never became `claude`");

    let report = crate::core::tmux::with_tmux_binary(
        server.shim_bin().into(),
        mgr.register_supervisor(&reg, &Verifier::bound()),
    )
    .await
    .expect("the Architect registers");

    assert_eq!(report.registered.len(), 1, "{report:?}");
    assert!(report.skipped[0].contains("a `claude` runs"), "{report:?}");
}

/// Fail-Open Check, #8942 delta critic MEDIUM: when the pane's pid cannot be
/// read, whether `claude` runs there is unknown, so the helper is not
/// registered. The pane's cwd reads through the driver's private-server shim;
/// the process probe's tmux is a binary that always fails.
#[tokio::test]
async fn a_helper_whose_pane_pid_cannot_be_read_is_not_registered() {
    let Some((server, root, mgr)) = real_tmux_manager("nopid").await else {
        return;
    };
    let reg = tmux_registration(&root);
    let _pane = helper_pane(&server, &reg, "exec /bin/sleep 60");
    let failing = root.path().join("tmux-fails");
    std::fs::write(&failing, "#!/bin/sh\nexit 1\n").expect("failing tmux");
    std::fs::set_permissions(
        &failing,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod failing tmux");

    let report = crate::core::tmux::with_tmux_binary(
        failing,
        mgr.register_supervisor(&reg, &Verifier::bound()),
    )
    .await
    .expect("the Architect registers");

    assert_eq!(report.registered.len(), 1, "{report:?}");
    assert!(
        report.skipped[0].contains("whether a `claude`"),
        "{report:?}"
    );
}

/// A manager over the real driver whose tmux is private server `tag`; `None`
/// (skip) when tmux is not installed.
async fn real_tmux_manager(
    tag: &str,
) -> Option<(PrivateTmuxServer, tempfile::TempDir, SessionManager)> {
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return None;
    }
    let server = PrivateTmuxServer::new("tmux", &format!("reg8942-{tag}"));
    let root = crate::test_support::hermetic_temp_dir();
    let driver = crate::daemon::tmux::TmuxDriver::with_tmux_path_for_test(server.shim_bin());
    let driver = Arc::new(RealTmuxDriver::from_driver_for_test(driver));
    let mgr = SessionManager::new(root.path(), driver)
        .await
        .expect("manager");
    Some((server, root, mgr))
}

/// A registration under the reserved test prefix, so a leaked pane is never
/// adopted.
fn tmux_registration(root: &tempfile::TempDir) -> SupervisorRegistration {
    let mut reg = registration(root, None);
    reg.session = "tm-xtest-arch8942".into();
    reg.poll_session = Some("tm-xtest-arch8942-poll".into());
    reg
}

/// The poller's pane on `server`, in the Architect directory, running `cmd`.
fn helper_pane(
    server: &PrivateTmuxServer,
    reg: &SupervisorRegistration,
    cmd: &str,
) -> ScratchTmuxSession {
    let name = reg.poll_session.as_deref().expect("a poller");
    ScratchTmuxSession::spawn_in_on_socket("tmux", Some(server.name()), name, Some(&reg.dir), cmd)
}

/// #8942 critic MEDIUM: one Architect per user, so a registration marks every
/// other non-terminal Architect record deleted, record-only — a stopped or
/// errored one included, so it stops pinning its name. Ordinary records and
/// terminal ones are left as they are.
#[tokio::test]
async fn registration_deletes_every_stale_architect_record() {
    let (root, fake, mgr) = manager().await;
    let reg = registration(&root, None);
    let stale = [
        (
            ManagedSessionState::Stopped,
            SessionKind::Supervisor,
            "old-arch",
        ),
        (
            ManagedSessionState::Errored,
            SessionKind::SupervisorAux,
            "old-arch-poll",
        ),
        (
            ManagedSessionState::Active,
            SessionKind::Supervisor,
            "other-arch",
        ),
    ];
    let mut stale_ids = Vec::new();
    for (state, kind, name) in stale {
        stale_ids.push(seed_kind(&mgr, &root, state, kind, name).await);
    }
    let kept = [
        (
            ManagedSessionState::Deleted,
            SessionKind::Supervisor,
            "gone-arch",
        ),
        (
            ManagedSessionState::Stopped,
            SessionKind::Ordinary,
            "tm-arch",
        ),
    ];
    let mut kept_ids = Vec::new();
    for (state, kind, name) in kept {
        let id = seed_kind(&mgr, &root, state.clone(), kind, name).await;
        kept_ids.push((id, state));
    }

    let report = mgr
        .register_supervisor(&reg, &Verifier::bound())
        .await
        .expect("registers");

    let mut replaced = report.replaced.clone();
    replaced.sort();
    let mut expected: Vec<String> = stale_ids.iter().map(ToString::to_string).collect();
    expected.sort();
    assert_eq!(replaced, expected, "{report:?}");
    for id in &stale_ids {
        let state = mgr.get(id).await.expect("record").state;
        assert_eq!(state, ManagedSessionState::Deleted, "{id}");
    }
    for (id, state) in &kept_ids {
        assert_eq!(mgr.get(id).await.expect("record").state, *state, "{id}");
    }
    assert!(fake.kill_calls.lock().unwrap().is_empty(), "record-only");
    assert_eq!(
        SupervisorFloor::for_data_dir(mgr.data_dir()).verdict("old-arch"),
        KillVerdict::Permit,
        "a stale Architect record no longer pins its name"
    );
}

/// Fail-Open Check: a helper is granted a protected record only when its
/// pane runs in the Architect directory; an unreadable or foreign pane is
/// skipped, with the reason.
#[tokio::test]
async fn a_helper_whose_pane_cannot_be_placed_is_not_registered() {
    let (root, fake, mgr) = manager().await;
    let reg = registration(&root, Some("tm-arch-poll"));
    let poll_id = ManagedSessionId::for_supervisor(&reg.dir, "-poll");

    let report = mgr
        .register_supervisor(&reg, &Verifier::bound())
        .await
        .expect("the Architect registers");
    assert_eq!(report.registered.len(), 1, "{report:?}");
    assert!(report.skipped[0].contains("cannot be read"), "{report:?}");

    *fake.pane_cwd_override.lock().unwrap() = Some(root.path().to_path_buf());
    let report = mgr
        .register_supervisor(&reg, &Verifier::bound())
        .await
        .expect("the Architect registers");
    assert_eq!(report.registered.len(), 1, "{report:?}");
    assert!(report.skipped[0].contains("not in"), "{report:?}");
    assert!(mgr.get(&poll_id).await.is_err(), "no helper record");
}

/// A relative directory or a name tmux could misread is refused before the
/// binding is checked or anything is written.
#[tokio::test]
async fn register_supervisor_refuses_a_relative_dir_or_a_bad_name() {
    let (root, _fake, mgr) = manager().await;
    let verifier = Verifier::bound();
    let mut relative = registration(&root, None);
    relative.dir = "architect".into();
    let mut bad_name = registration(&root, Some("poll:0"));
    bad_name.session = "tm-arch".into();
    for reg in [relative, bad_name] {
        let err = mgr
            .register_supervisor(&reg, &verifier)
            .await
            .expect_err("refused");
        assert!(matches!(err, RegisterError::Invalid(_)), "{err:?}");
    }
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
    assert!(mgr.list().await.is_empty());
}
