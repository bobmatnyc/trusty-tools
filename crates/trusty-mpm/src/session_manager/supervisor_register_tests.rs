//! Tests for the #8942 Architect registration. Hermetic: a scratch manager,
//! the recording `FakeTmuxDriver`, and a fake [`BindingVerifier`]; nothing
//! reaches tmux, the process table, the daemon or `~/.trusty-mpm`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::tests::{FakeTmuxDriver, seed_record};
use super::{BindingVerifier, RegisterError, SupervisorRegistration};
use crate::session_manager::{
    KillVerdict, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionKind,
    SessionManager, SupervisorFloor,
};

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
    *fake.pane_cwd_override.lock().unwrap() = Some(reg.dir.clone());
    // The fake's `runtime_ready` is `session_exists`: this pane "runs claude".
    fake.create_session("tm-arch-poll", &reg.dir.to_string_lossy())
        .expect("fake session");

    let report = mgr
        .register_supervisor(&reg, &Verifier::bound())
        .await
        .expect("the Architect registers");

    assert_eq!(report.registered.len(), 1, "{report:?}");
    assert!(report.skipped[0].contains("claude"), "{report:?}");
    let poll_id = ManagedSessionId::for_supervisor(&reg.dir, "-poll");
    assert!(mgr.get(&poll_id).await.is_err(), "no helper record");
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
