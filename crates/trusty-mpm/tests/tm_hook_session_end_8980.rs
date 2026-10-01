//! Integration test: `tm hook` delivers `SessionEnd` over the daemon socket,
//! and only the owning session's `SessionEnd` stales its live delegations
//! (#8980).
//!
//! Why: a `SessionEnd` stales the named session's live delegation records
//! (#6797). The daemon now does that only for a socket `SessionEnd` sent from
//! under the session's own bound `claude`, so the hook's transport decides
//! whether a real session end still releases its records, and the caller
//! check decides whether a forged one can.
//! What: serves the real HTTP router and the real daemon socket from one
//! state. The built `tm` runs under fake `claude`s (`/bin/bash` exec'd under
//! that name), each sending hook payloads only when the test releases them.
//! The owner's `SessionStart` then `SessionEnd` stale its record; a
//! sibling's `SessionEnd` naming the owner's id leaves it Running.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_session_end_8980::`.

use crate::common;

use std::future::IntoFuture;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use trusty_mpm::core::agent::{Delegation, DelegationStatus};
use trusty_mpm::core::hook::HookEvent;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::session::SessionId;
use trusty_mpm::daemon::api;
use trusty_mpm::daemon::state::DaemonState;

/// Runs each payload file `f` through `tm hook` once `f.go` exists, then
/// waits for the hold file. Bounded: every wait gives up after 30 s.
const SCRIPT: &str = r#"tm="$0"; url="$1"; hold="$2"; shift 2
for f in "$@"; do
  for _ in $(seq 600); do [ -e "$f.go" ] && break; sleep 0.05; done
  "$tm" --url "$url" hook < "$f"
done
for _ in $(seq 600); do [ -e "$hold" ] && break; sleep 0.05; done
true"#;

/// A daemon state under `root`, with the framework directories it reads.
fn daemon_state(root: &Path) -> Arc<DaemonState> {
    let paths = FrameworkPaths::under(root);
    for dir in [&paths.hooks, &paths.instructions, &paths.agents] {
        std::fs::create_dir_all(dir).expect("framework dir");
    }
    Arc::new(DaemonState::with_paths(&paths))
}

/// Serve the real HTTP router and the real daemon socket from `state`.
async fn serve(
    state: &Arc<DaemonState>,
    socket: &Path,
) -> (String, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(axum::serve(listener, api::router(Arc::clone(state))).into_future());
    let bound = trusty_mpm::daemon::socket::bind(socket)
        .await
        .expect("bind socket");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(trusty_mpm::daemon::socket::serve_until_shutdown(
        bound,
        Arc::clone(state),
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
    (url, stop)
}

/// Kills the fake `claude` on drop, so a failed assertion leaves no process.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Write one hook payload for `session` to `dir/<name>.json`; its path.
fn payload(dir: &Path, name: &str, event: &str, session: SessionId) -> PathBuf {
    let path = dir.join(format!("{name}.json"));
    let body = serde_json::json!({
        "session_id": session.0.to_string(),
        "hook_event_name": event,
        "cwd": dir.display().to_string(),
    });
    std::fs::write(&path, body.to_string()).expect("write the payload");
    path
}

/// Release a payload written by [`payload`].
fn release(file: &Path) {
    std::fs::write(file.with_extension("json.go"), b"").expect("go marker");
}

/// Start a fake `claude` in its own `dir` running [`SCRIPT`] over `files`.
fn spawn_claude(dir: &Path, url: &str, socket: &Path, hold: &Path, files: &[&Path]) -> Reaped {
    let claude = dir.join("claude");
    std::os::unix::fs::symlink("/bin/bash", &claude).expect("symlink /bin/bash");
    let mut cmd = Command::new(claude);
    common::isolate_spawned_tm(&mut cmd, &dir.join("home"));
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg(common::tm_bin())
        .arg(url)
        .arg(hold)
        .args(files)
        .env("TRUSTY_MPM_SOCKET", socket)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_NOTIFY_INBOX")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Reaped(cmd.spawn().expect("spawn the fake claude"))
}

/// Poll `done` for up to 30 s.
async fn eventually(done: impl Fn() -> bool) -> bool {
    for _ in 0..1200 {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Whether `state` ingested a `SessionEnd` for `session`.
fn ended(state: &DaemonState, session: SessionId) -> bool {
    state
        .recent_hook_events()
        .iter()
        .any(|r| r.event == HookEvent::SessionEnd && r.session == session)
}

/// One Running record for `owner`, inserted once its claude is bound.
async fn bound_with_a_record(state: &DaemonState, owner: SessionId) {
    assert!(
        eventually(|| state.session_claudes().get(owner).is_some()).await,
        "the owner's SessionStart bound no claude"
    );
    state.upsert_delegation(Delegation::observed(owner, "version-control", "task", None));
}

/// The status of the one record in `state`.
fn only_status(state: &DaemonState) -> DelegationStatus {
    let all = state.all_delegations();
    assert_eq!(all.len(), 1, "{all:?}");
    all[0].status
}

/// #8980 + #6797: the owner's own `SessionEnd`, sent by `tm hook` from under
/// the `claude` its `SessionStart` bound, stales its live record.
#[tokio::test(flavor = "multi_thread")]
async fn the_owners_session_end_over_the_socket_stales_its_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = daemon_state(&dir.path().join("root"));
    let socket = dir.path().join("mpm.sock");
    let (url, stop) = serve(&state, &socket).await;
    let owner = SessionId::new();
    let hold = dir.path().join("hold");
    let start = payload(dir.path(), "start", "SessionStart", owner);
    let end = payload(dir.path(), "end", "SessionEnd", owner);
    let _claude = spawn_claude(dir.path(), &url, &socket, &hold, &[&start, &end]);

    release(&start);
    bound_with_a_record(&state, owner).await;
    release(&end);
    let seen = eventually(|| ended(&state, owner)).await;
    std::fs::write(&hold, b"").expect("hold marker");
    let _ = stop.send(());

    assert!(seen, "the SessionEnd was never ingested");
    assert_eq!(only_status(&state), DelegationStatus::Stale);
}

/// #8980 regression: a `SessionEnd` naming the owner's id, sent by `tm hook`
/// under a sibling's `claude` while the owner's runs, stales nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_siblings_session_end_leaves_the_owners_records_live() {
    let (owner_dir, sibling_dir) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    let state = daemon_state(&owner_dir.path().join("root"));
    let socket = owner_dir.path().join("mpm.sock");
    let (url, stop) = serve(&state, &socket).await;
    let owner = SessionId::new();
    let hold = owner_dir.path().join("hold");
    let start = payload(owner_dir.path(), "start", "SessionStart", owner);
    let forged = payload(sibling_dir.path(), "end", "SessionEnd", owner);
    let _owner = spawn_claude(owner_dir.path(), &url, &socket, &hold, &[&start]);
    let _sibling = spawn_claude(sibling_dir.path(), &url, &socket, &hold, &[&forged]);

    release(&start);
    bound_with_a_record(&state, owner).await;
    release(&forged);
    let seen = eventually(|| ended(&state, owner)).await;
    std::fs::write(&hold, b"").expect("hold marker");
    let _ = stop.send(());

    assert!(seen, "the forged SessionEnd was never ingested");
    assert_eq!(only_status(&state), DelegationStatus::Running);
}
