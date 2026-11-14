//! Tests for daemon-side session-checkout registration (#10368).
//!
//! Every test runs a daemon state under its own temp framework root, so the
//! registry it writes is `<temp>/project-registry`, never `~/.trusty-mpm`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use super::*;
use crate::daemon::api::{RegisterSession, register_session};
use crate::project::{registered_checkout, registry_data_dir_under};

/// A git checkout at `dir` whose origin is `remote`, canonicalized.
fn checkout(dir: &Path, remote: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir checkout");
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", remote][..],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
    dir.canonicalize().expect("canonical checkout")
}

/// A `POST /sessions` body that registers (no spawn) a session in `dir`.
fn body_for(dir: &Path) -> RegisterSession {
    RegisterSession {
        project: dir.display().to_string(),
        project_path: Some(dir.to_path_buf()),
        name: None,
        workdir: None,
        tmux_socket: None,
    }
}

/// The regression: a session registered with the daemon in an unregistered
/// checkout writes the `projects.json` row `tm secrets assign` resolves
/// (`registered_checkout`, the `RegistryResolver` lookup). On the pre-fix
/// daemon nothing wrote the row and the lookup answered `NotRegistered`. A
/// second start for the same checkout keeps the file byte-identical.
#[tokio::test]
async fn a_registered_session_registers_its_checkout_as_a_project() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    let state = Arc::new(DaemonState::with_root(root.clone()));
    let repo = checkout(&tmp.path().join("widget"), "git@github.com:Acme/Widget.git");

    register_session(State(Arc::clone(&state)), Json(body_for(&repo)))
        .await
        .expect("registration succeeds");

    let registry = registry_data_dir_under(&root);
    assert_eq!(
        registered_checkout(&registry, "acme/widget").expect("assign resolves the project"),
        repo
    );
    let first = std::fs::read(registry.join("projects.json")).expect("row written");
    register_session(State(Arc::clone(&state)), Json(body_for(&repo)))
        .await
        .expect("second registration succeeds");
    assert_eq!(
        std::fs::read(registry.join("projects.json")).expect("still present"),
        first,
        "a second session start must not rewrite or duplicate the row"
    );
}

/// The error arm: a registry that cannot be written leaves the session
/// registered, the call successful, and one warn line naming the cause.
#[tokio::test]
async fn a_failed_registry_write_leaves_the_session_registered_and_warns() {
    use std::os::unix::fs::PermissionsExt as _;
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    let state = Arc::new(DaemonState::with_root(root.clone()));
    let repo = checkout(&tmp.path().join("widget"), "https://github.com/acme/widget");
    let registry = registry_data_dir_under(&root);
    std::fs::create_dir_all(&registry).expect("mkdir registry");
    std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o500))
        .expect("make the registry read-only");
    // Precondition: the write must really fail (a root run ignores the mode).
    assert!(
        std::fs::write(registry.join("probe"), b"x").is_err(),
        "the registry directory is still writable; this test cannot fail the write"
    );

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let outcome = register_session(State(Arc::clone(&state)), Json(body_for(&repo)))
        .with_subscriber(subscriber)
        .await;
    std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o700))
        .expect("restore the registry mode");

    let Json(response) = outcome.expect("a registry failure never fails the session start");
    assert!(
        state.list_sessions().iter().any(|s| s.id == response.id),
        "the session stays registered"
    );
    assert!(
        !registry.join("projects.json").exists(),
        "no row was written"
    );
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("WARN")
            && l.contains("#10368")
            && l.contains("was not registered as a project")),
        "no warn line named the failure: {lines:?}"
    );
}

/// A running session a discovery scan adopted registers its checkout.
#[tokio::test]
async fn an_adopted_session_registers_its_checkout_as_a_project() {
    use crate::core::session::{ControlModel, Session, SessionId};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    let state = DaemonState::with_root(root.clone());
    let repo = checkout(&tmp.path().join("gadget"), "https://github.com/acme/gadget");
    let mut session = Session::new(SessionId::new(), String::new(), ControlModel::Tmux, None);
    session.tmux_name = "tm-gadget".into();
    session.project_path = Some(repo.clone());
    state.register_session(session);

    register_adopted_sessions(&state, &["tm-gadget".to_string()]).await;

    assert_eq!(
        registered_checkout(&registry_data_dir_under(&root), "acme/gadget")
            .expect("the adopted session's project resolves"),
        repo
    );
}
