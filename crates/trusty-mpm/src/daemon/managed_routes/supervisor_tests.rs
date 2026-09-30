//! Tests for the #8942 registration route's status mapping. A scratch-root
//! daemon state (its tmux driver is a no-op) and a fake verifier: nothing
//! reaches tmux, the process table or the live daemon.

use super::*;

use crate::session_manager::SessionKind;

struct Fixed(Result<(), String>);

impl BindingVerifier for Fixed {
    fn verify(&self, _dir: &Path, _session: &str) -> Result<(), String> {
        self.0.clone()
    }
}

async fn state() -> (tempfile::TempDir, Arc<DaemonState>) {
    let root = crate::test_support::hermetic_temp_dir();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    (root, state)
}

fn body(dir: PathBuf) -> SupervisorRegistration {
    SupervisorRegistration {
        dir,
        session: "tm-arch".into(),
        poll_session: None,
        collector_session: None,
    }
}

/// Fail-Open Check: an unbound session is a 403 and no record.
#[tokio::test]
async fn the_route_refuses_an_unbound_session_with_403() {
    let (root, state) = state().await;
    let reg = body(root.path().to_path_buf());
    let out = register_supervisor_core(&state, &reg, &Fixed(Err("no record".into()))).await;
    assert_eq!(out.status, 403);
    assert!(state.session_manager().await.list().await.is_empty());

    let out = register_supervisor_core(&state, &reg, &Fixed(Ok(()))).await;
    assert_eq!(out.status, 200);
    let records = state.session_manager().await.list().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, SessionKind::Supervisor);
}

/// A relative directory is a 400, before any binding check.
#[tokio::test]
async fn the_route_rejects_a_relative_dir_with_400() {
    let (_root, state) = state().await;
    let out = register_supervisor_core(&state, &body("rel".into()), &Fixed(Ok(()))).await;
    assert_eq!(out.status, 400);
}
