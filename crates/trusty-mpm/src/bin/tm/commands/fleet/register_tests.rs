//! Tests for `tm fleet init`'s #8942 registration and sidecar prune. No
//! daemon, no tmux: answers are built by hand, tmux is the stub probe.

use trusty_mpm::client::http_client::supervisor::RegisterCallError;
use trusty_mpm::session_manager::{RegisteredSession, RegistrationReport, SessionKind};

use super::super::session_name::SessionNames;
use super::super::tests::NO_TMUX;
use super::super::{Probe, Step};
use super::{prune_step, register_step, registration};

fn reg() -> trusty_mpm::session_manager::SupervisorRegistration {
    registration(
        std::path::Path::new("/arch"),
        &SessionNames::default_names(),
    )
}

fn record(name: &str, kind: SessionKind) -> RegisteredSession {
    RegisteredSession {
        id: "0000".into(),
        tmux_name: name.into(),
        kind,
    }
}

/// The request carries the names init used, helpers included.
#[test]
fn the_registration_names_the_real_helper_sessions() {
    let names = SessionNames::new("tm-supervisor").expect("valid");
    let reg = registration(std::path::Path::new("/arch"), &names);
    assert_eq!(reg.session, "tm-supervisor");
    assert_eq!(reg.poll_session.as_deref(), Some("tm-supervisor-poll"));
    assert_eq!(
        reg.collector_session.as_deref(),
        Some("tm-supervisor-collector")
    );
}

/// Fail-Open Check: only an answer that names the Architect's record is
/// "registered"; no daemon is a warning; a refusal, a failed call and an
/// answer without the record are failed steps.
#[test]
fn a_registration_answer_maps_to_its_step() {
    let reg = reg();
    let ok = RegistrationReport {
        registered: vec![
            record("tm-architect", SessionKind::Supervisor),
            record("tm-architect-poll", SessionKind::SupervisorAux),
        ],
        ..Default::default()
    };
    match register_step(&reg, Ok(ok)) {
        Step::Changed(t) => assert!(t.contains("helpers: tm-architect-poll"), "{t}"),
        other => panic!("{other:?}"),
    }
    let unreachable = register_step(
        &reg,
        Err(RegisterCallError::Unreachable("no socket".into())),
    );
    assert!(
        matches!(&unreachable, Step::Skipped(t) if t.contains("NOT registered")),
        "{unreachable:?}"
    );
    let failures = [
        Err(RegisterCallError::Refused {
            status: 403,
            reason: "not bound".into(),
        }),
        Err(RegisterCallError::Failed("predates the route".into())),
        Ok(RegistrationReport::default()),
        Ok(RegistrationReport {
            registered: vec![record("tm-architect-poll", SessionKind::SupervisorAux)],
            ..Default::default()
        }),
    ];
    for outcome in failures {
        let step = register_step(&reg, outcome);
        assert!(matches!(step, Step::Failed(_)), "{step:?}");
    }
}

/// Ruling 2 through `init`'s step: a dead launch's sidecar is pruned, a
/// live one (this test process) is kept, and a session tmux reports keeps
/// its sidecar.
#[test]
fn fleet_init_prunes_only_a_proven_stale_sidecar() {
    let home = tempfile::tempdir().expect("scratch home");
    let dir = home.path().join(".trusty-mpm").join("architect-launch");
    std::fs::create_dir_all(&dir).expect("sidecar dir");
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    let write = |pid: u32, session: &str| {
        let body = serde_json::json!({"pid": pid, "start_time": 100, "session": session});
        std::fs::write(
            dir.join(format!("{pid}.architect-session")),
            body.to_string(),
        )
        .expect("sidecar");
    };
    write(dead, "tm-gone");
    write(std::process::id(), "tm-live");

    let live_tmux = Probe {
        pane: |_| super::super::launch::PaneState::Unknown("stub".into()),
        ..NO_TMUX
    };
    let kept = prune_step(home.path(), live_tmux).expect("a step");
    assert!(
        matches!(&kept, Step::Unchanged(t) if t.contains("tm-gone")),
        "{kept:?}"
    );
    assert!(dir.join(format!("{dead}.architect-session")).exists());

    let pruned = prune_step(home.path(), NO_TMUX).expect("a step");
    assert!(
        matches!(&pruned, Step::Changed(t) if t.contains("tm-gone")),
        "{pruned:?}"
    );
    assert!(!dir.join(format!("{dead}.architect-session")).exists());
    assert!(
        dir.join(format!("{}.architect-session", std::process::id()))
            .exists()
    );
}
