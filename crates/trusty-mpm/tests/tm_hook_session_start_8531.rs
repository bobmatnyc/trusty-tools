//! Integration test: `tm hook` delivers `SessionStart` over the daemon socket
//! (#8531).
//!
//! Why: the daemon binds a session to its `claude` only from a socket
//! `SessionStart`, so the hook's transport choice decides whether a session
//! can ever end its own delegation record. `post_session_start_via` is that
//! choice, and only the real binary proves the production wiring reaches it.
//! What: serves the real HTTP router on loopback and, in the first case, the
//! real daemon socket from the same state. The built `tm` runs under a fake
//! `claude` (`/bin/bash` exec'd under that name) with a `SessionStart` on
//! stdin. Served socket: the session is bound to that `claude`, and ingested
//! once. Nobody on the socket path: the HTTP fallback registers the session,
//! which stays unbound.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_session_start_8531::`.

use crate::common;

use std::future::IntoFuture;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use trusty_mpm::core::hook::HookEvent;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::session::SessionId;
use trusty_mpm::daemon::api;
use trusty_mpm::daemon::state::DaemonState;

/// A daemon state under `root`, with the framework directories it reads.
fn daemon_state(root: &Path) -> Arc<DaemonState> {
    let paths = FrameworkPaths::under(root);
    for dir in [&paths.hooks, &paths.instructions, &paths.agents] {
        std::fs::create_dir_all(dir).expect("framework dir");
    }
    Arc::new(DaemonState::with_paths(&paths))
}

/// Serve the real HTTP router on an ephemeral loopback port; its base URL.
async fn serve_http(state: Arc<DaemonState>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(axum::serve(listener, api::router(state)).into_future());
    format!("http://{addr}")
}

/// Serve the real daemon socket from `state` until the sender fires.
async fn serve_socket(state: Arc<DaemonState>, socket: &Path) -> tokio::sync::oneshot::Sender<()> {
    let bound = trusty_mpm::daemon::socket::bind(socket)
        .await
        .expect("bind socket");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(trusty_mpm::daemon::socket::serve_until_shutdown(
        bound,
        state,
        async {
            let _ = shutdown.await;
        },
    ));
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop
}

/// `/bin/bash` under the name `claude`. A symlink: macOS kills a copied
/// system binary.
fn fake_claude(dir: &Path) -> PathBuf {
    let path = dir.join("claude");
    std::os::unix::fs::symlink("/bin/bash", &path).expect("symlink /bin/bash");
    path
}

/// Run `tm --url <url> hook` as a child of a fake `claude`, with a
/// `SessionStart` for `session` on stdin and `TRUSTY_MPM_SOCKET=socket`;
/// returns the output and the fake `claude`'s pid.
fn run_session_start(dir: &Path, url: &str, socket: &Path, session: SessionId) -> (Output, u32) {
    // `; true` keeps bash alive as the parent instead of exec'ing `tm`.
    let mut cmd = Command::new(fake_claude(dir));
    common::isolate_spawned_tm(&mut cmd, &dir.join("home"));
    cmd.arg("-c")
        .arg("\"$0\" --url \"$1\" hook; true")
        .arg(common::tm_bin())
        .arg(url)
        .env("TRUSTY_MPM_SOCKET", socket)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_NOTIFY_INBOX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn the fake claude");
    let claude_pid = child.id();
    let payload = serde_json::json!({
        "session_id": session.0.to_string(),
        "hook_event_name": "SessionStart",
        "source": "startup",
        "cwd": dir.display().to_string(),
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write the payload");
    (child.wait_with_output().expect("wait"), claude_pid)
}

/// How many `SessionStart` events `state` ingested for `session`.
fn session_starts(state: &DaemonState, session: SessionId) -> usize {
    state
        .recent_hook_events()
        .into_iter()
        .filter(|r| r.event == HookEvent::SessionStart && r.session == session)
        .count()
}

/// #8531: with the daemon socket served, `tm hook` sends `SessionStart`
/// over it, the daemon binds the session to the `claude` above the hook,
/// and the event is ingested once — no HTTP copy.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_start_over_the_socket_binds_its_claude() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = daemon_state(&dir.path().join("root"));
    let url = serve_http(Arc::clone(&state)).await;
    let socket = dir.path().join("mpm.sock");
    let stop = serve_socket(Arc::clone(&state), &socket).await;
    let session = SessionId::new();

    let (out, claude_pid) = tokio::task::spawn_blocking({
        let (dir, url, socket) = (dir.path().to_path_buf(), url.clone(), socket.clone());
        move || run_session_start(&dir, &url, &socket, session)
    })
    .await
    .expect("join");
    let _ = stop.send(());

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bound = state
        .session_claudes()
        .get(session)
        .expect("the socket SessionStart bound the session");
    assert_eq!(bound.pid, claude_pid, "bound to the claude above the hook");
    assert_eq!(session_starts(&state, session), 1, "ingested once");
}

/// #8531: with nobody listening on the socket path, `tm hook` falls back to
/// HTTP. The session registers, and stays unbound: HTTP proves no process.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_start_with_no_socket_falls_back_to_http() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = daemon_state(&dir.path().join("root"));
    let url = serve_http(Arc::clone(&state)).await;
    let socket = dir.path().join("nobody-listens.sock");
    let session = SessionId::new();

    let (out, _) = tokio::task::spawn_blocking({
        let (dir, url, socket) = (dir.path().to_path_buf(), url.clone(), socket.clone());
        move || run_session_start(&dir, &url, &socket, session)
    })
    .await
    .expect("join");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        state.session(session).is_some(),
        "HTTP registered the session"
    );
    assert_eq!(session_starts(&state, session), 1, "ingested once");
    assert_eq!(
        state.session_claudes().get(session),
        None,
        "HTTP binds nothing"
    );
    assert!(
        state.session_claudes().is_settled(session),
        "the unproven first announcement is final"
    );
}
