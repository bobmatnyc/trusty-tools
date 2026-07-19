//! Integration tests for the session RENAME endpoint and the `tm ls`
//! live-tmux state RECONCILIATION (split out of `session_manager_mvp.rs`, which
//! is at the 1500-SLOC test-file cap).
//!
//! Why: these exercise the daemon HTTP handlers end-to-end against a hermetic
//! `DaemonState` — the rename PATCH route (`rename_managed_session`) and the
//! list handler's reconciliation of a stale `Stopped` record against a live
//! tmux session.
//! What: `rename_route_*` (rename success + collision/invalid rejection) and
//! `list_reconciles_live_session_state_not_stopped`.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm`.

use std::sync::Arc;

use tempfile::TempDir;

use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::runtime::RuntimeKind;
use trusty_mpm::session_manager::{ManagedError, ManagedSessionId, ManagedTmuxDriver};

/// Decode an axum response into `(status, json)` for assertions.
async fn decode_response(
    resp: impl axum::response::IntoResponse,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = resp.into_response();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// A tmux driver whose session stays LIVE even after `kill_session` — models
/// the `tm ls` reconciliation bug where a record is marked `Stopped` but its
/// tmux session is actually still alive (a daemon-restart stale-state race).
///
/// Why: `create_with_id` starts a session `Provisioning`, and the only public
/// path to `Stopped` is `stop`, which calls `kill_session`; a driver that
/// tracks-but-never-removes lets `stop` land the RECORD in `Stopped` while the
/// tmux session keeps reporting live — the exact stale scenario the list
/// handler must reconcile. It also reports every live session as attached so
/// the `attached` flag can be asserted.
/// What: `create_session` records the name as live; `kill_session` is a no-op;
/// `list_sessions`/`attached_session_names` keep reporting every created name.
/// Test: `list_reconciles_live_session_state_not_stopped`.
struct StickyLiveTmux {
    live: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl StickyLiveTmux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            live: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }
}

impl ManagedTmuxDriver for StickyLiveTmux {
    fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.live.lock().unwrap().insert(name.to_owned());
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(()) // NO-OP: the tmux session stays live after a stop.
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.live.lock().unwrap().iter().cloned().collect())
    }
    fn attached_session_names(&self) -> Vec<String> {
        self.live.lock().unwrap().iter().cloned().collect()
    }
}

/// Seed a managed session through the daemon's manager and return its id.
async fn seed(state: &Arc<DaemonState>, root: &TempDir, tag: &str) -> ManagedSessionId {
    let mgr = state.session_manager().await;
    let id = ManagedSessionId::new();
    let ws = root.path().join(format!("{id}-{tag}"));
    mgr.create_with_id(
        id,
        format!("lifecycle test {tag}"),
        Some(ws.clone()),
        None,
        Some(ws),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session");
    id
}

/// GET list reconciles a stale `Stopped` record against LIVE tmux → `active`.
///
/// Why: after a daemon restart a record can persist `stopped` even though its
/// tmux session is alive — the `tm ls` picker then wrongly showed every session
/// `(stopped)` and offered a destructive `restart`. The list handler now
/// reconciles the displayed state against real tmux.
/// What: with a `StickyLiveTmux` driver (whose session stays live after a
/// `stop`), creates a session and stops it — landing the RECORD in `Stopped`
/// while the tmux session keeps reporting live — then asserts the LIST endpoint
/// reports the row as `active` (reconciled) and `attached`, not `stopped`.
/// Test: this function IS the test.
#[tokio::test]
async fn list_reconciles_live_session_state_not_stopped() {
    use std::collections::HashMap;
    use trusty_mpm::daemon::managed_routes::list_managed_sessions;

    let root = TempDir::new().unwrap();
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            root.path().to_path_buf(),
            StickyLiveTmux::new(),
        )
        .await,
    );
    let id = seed(&state, &root, "reconcile").await;
    // Stop lands the RECORD in Stopped, but StickyLiveTmux keeps the tmux
    // session live — the exact post-daemon-restart stale-state scenario.
    state.session_manager().await.stop(&id).await.expect("stop");

    let (_status, body) = decode_response(
        list_managed_sessions(
            axum::extract::State(state.clone()),
            axum::extract::Query(HashMap::new()),
        )
        .await,
    )
    .await;
    let row = body["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|s| s["id"] == serde_json::json!(id.to_string()))
        .expect("our session in the list");
    assert_eq!(
        row["state"].as_str(),
        Some("active"),
        "a stopped record with a LIVE tmux session must reconcile to active, body={body}"
    );
    assert_eq!(
        row["attached"].as_bool(),
        Some(true),
        "an attached live session must set the attached flag, body={body}"
    );
}

/// PATCH …/{id} renames a managed session.
///
/// Why: `tm sessions rename` must update the record's `tmux_name` (and the live
/// tmux session, when one exists) so `tm ls`/`tmux attach` show the new name.
/// What: creates a session, stops it (so no live tmux rename is needed on this
/// test driver), PATCHes the rename endpoint with a fresh name, asserts `200`
/// and that the record now carries the new name.
/// Test: this function IS the test.
#[tokio::test]
async fn rename_route_renames() {
    use trusty_mpm::daemon::managed_routes::{RenameRequest, rename_managed_session};

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let id = seed(&state, &root, "rename").await;
    // Stop for real so no live tmux rename is required (the noop driver reports
    // no live tmux, so `rename` skips the tmux-side rename).
    state
        .session_manager()
        .await
        .stop(&id)
        .await
        .expect("stop before rename");

    let (status, body) = decode_response(
        rename_managed_session(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.to_string()),
            axum::extract::Json(RenameRequest {
                name: "tm-renamed-99".to_string(),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK, "body={body}");
    assert_eq!(body["name"], serde_json::json!("tm-renamed-99"));
    assert_eq!(
        state
            .session_manager()
            .await
            .get(&id)
            .await
            .expect("get")
            .tmux_name,
        "tm-renamed-99",
        "the record must carry the new name"
    );
}

/// PATCH …/{id} rename REJECTS a name already held by another session (409).
#[tokio::test]
async fn rename_route_rejects_collision() {
    use trusty_mpm::daemon::managed_routes::{RenameRequest, rename_managed_session};

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let id_a = seed(&state, &root, "rn-a").await;
    let id_b = seed(&state, &root, "rn-b").await;
    let b_name = state
        .session_manager()
        .await
        .get(&id_b)
        .await
        .expect("get b")
        .tmux_name;

    let (status, _body) = decode_response(
        rename_managed_session(
            axum::extract::State(state.clone()),
            axum::extract::Path(id_a.to_string()),
            axum::extract::Json(RenameRequest {
                name: b_name.clone(),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_ne!(
        state
            .session_manager()
            .await
            .get(&id_a)
            .await
            .expect("get a")
            .tmux_name,
        b_name,
        "A must keep its own name after a refused rename"
    );
}

/// PATCH …/{id} rename REJECTS an invalid name (400).
#[tokio::test]
async fn rename_route_rejects_invalid_name() {
    use trusty_mpm::daemon::managed_routes::{RenameRequest, rename_managed_session};

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let id = seed(&state, &root, "rn-bad").await;

    let (status, _body) = decode_response(
        rename_managed_session(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.to_string()),
            axum::extract::Json(RenameRequest {
                name: "bad name!".to_string(),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}
