//! Coverage for establishing a repair caller from the kernel's peer (#8531).
//!
//! The walk and the owner lookup are injected for the refusal arms. The
//! regression and end-to-end cases run a real process chain under `/bin/bash`
//! exec'd as `claude`, so the live process table — not a stub — names
//! the `claude` every walk stops at.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::core::agent::DelegationStatus;
use crate::core::paths::FrameworkPaths;
use crate::core::session::{ControlModel, Session, SessionStatus};

const OWNER: ClaudeProcess = ClaudeProcess {
    pid: 4100,
    start_time: 100,
};
const SIBLING: ClaudeProcess = ClaudeProcess {
    pid: 4200,
    start_time: 100,
};

fn kernel(pid: u32) -> RepairPeer {
    RepairPeer::Kernel {
        pid,
        seen_at: 1_000,
    }
}

/// An owner lookup that names `OWNER` for `owner` and fails for any other
/// session.
fn owner_lookup(owner: SessionId) -> impl Fn(SessionId) -> Result<ClaudeProcess, String> {
    move |s| {
        if s == owner {
            Ok(OWNER)
        } else {
            Err("no record".to_string())
        }
    }
}

fn unestablished(caller: &RepairCaller) -> &str {
    match caller {
        RepairCaller::Unestablished(why) => why,
        RepairCaller::Session(s) => panic!("expected no caller, got session {}", s.0),
    }
}

/// #8531: a process under the owner's own `claude` is the owner.
#[test]
fn the_owner_process_establishes_the_owner_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        kernel(9001),
        &[SessionId::new(), owner],
        |_, _| Ok(OWNER),
        owner_lookup(owner),
    );
    assert_eq!(caller, RepairCaller::Session(owner));
}

/// #8531, the issue's acceptance case: a process under ANOTHER session's
/// `claude` is refused, whatever id it could have named.
#[test]
fn a_sibling_session_process_is_not_the_owner_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        kernel(9002),
        &[owner],
        |_, _| Ok(SIBLING),
        owner_lookup(owner),
    );
    let why = unestablished(&caller);
    assert!(why.contains(&SIBLING.pid.to_string()), "{why}");
    assert!(
        !why.contains(&owner.0.to_string()),
        "names no owner id: {why}"
    );
}

/// #8531: the owner's pid, reused by a process with another start time, is
/// not the owner's process.
#[test]
fn a_reused_owner_pid_with_another_start_time_is_not_the_owner_8531() {
    let owner = SessionId::new();
    let reused = ClaudeProcess {
        pid: OWNER.pid,
        start_time: OWNER.start_time + 1,
    };
    let caller = establish_caller_with(
        kernel(9006),
        &[owner],
        |_, _| Ok(reused),
        owner_lookup(owner),
    );
    unestablished(&caller);
}

/// #8531 Fail-Open Check: no kernel pid (HTTP, or a socket that could not
/// read one) establishes nobody, and nothing else is consulted.
#[test]
fn an_unproven_peer_is_unestablished_8531() {
    let owner = SessionId::new();
    for peer in [RepairPeer::http(), RepairPeer::from_socket(None)] {
        let caller = establish_caller_with(
            peer,
            &[owner],
            |_, _| panic!("no walk without a pid"),
            owner_lookup(owner),
        );
        unestablished(&caller);
    }
    // #8531 MEDIUM: `seen_at` is the server's accept instant, not "now".
    let accepted_at = std::time::UNIX_EPOCH + Duration::from_secs(1_234);
    assert_eq!(
        RepairPeer::from_socket(Some(RequestPeer {
            pid: 7,
            accepted_at
        })),
        RepairPeer::Kernel {
            pid: 7,
            seen_at: 1_234
        }
    );
}

/// The peer of a socket request from `pid`, accepted now.
fn socket_peer(pid: u32) -> RepairPeer {
    RepairPeer::from_socket(Some(RequestPeer {
        pid,
        accepted_at: std::time::SystemTime::now(),
    }))
}

/// #8531 Fail-Open Check: a peer walk that fails — an unreadable ancestry, a
/// reused peer pid, no `claude` above — refuses, and says why.
#[test]
fn a_failed_peer_walk_is_unestablished_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        kernel(9003),
        &[owner],
        |_, _| Err("ps failed".to_string()),
        owner_lookup(owner),
    );
    assert!(unestablished(&caller).contains("ps failed"), "{caller:?}");
}

/// #8531 Fail-Open Check: an owner whose process cannot be found refuses,
/// and the reason says so rather than reading the lookup as "no match".
#[test]
fn an_owner_process_that_cannot_be_found_is_unestablished_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        kernel(9005),
        &[owner],
        |_, _| Ok(OWNER),
        |_| Err("never announced".to_string()),
    );
    assert!(
        unestablished(&caller).contains("never announced"),
        "{caller:?}"
    );
}

/// #8531 Fail-Open Check: a session record from before this fix — with a
/// `pid` and a tmux name, but announced by no socket `SessionStart` — has no
/// owner process. Bound, it has.
#[test]
fn an_unbound_owner_has_no_process_8531() {
    let state = DaemonState::new();
    let owner = SessionId::new();
    let mut old = serde_json::to_value(Session::new(owner, "/repo", ControlModel::Tmux, None))
        .expect("serialize");
    old["tmux_name"] = serde_json::json!("tm-brave-otter");
    old["pid"] = serde_json::json!(4100);
    let record: Session = serde_json::from_value(old).expect("an old record deserializes");
    state.register_session(record);
    let got = owner_claude(&state, owner);
    assert!(
        got.as_ref().is_err_and(|e| e.contains("never announced")),
        "{got:?}"
    );
    state.bind_session_claude(owner, OWNER).expect("vacant");
    assert_eq!(owner_claude_with(&state, owner, live_owner), Ok(OWNER));
}

/// A process table in which `OWNER` runs, with its recorded start time.
fn live_owner(pid: u32) -> Result<crate::core::twin_arming::ProcessFacts, String> {
    if pid == OWNER.pid {
        Ok(crate::core::twin_arming::ProcessFacts {
            parent: Some(1),
            start_time: OWNER.start_time,
        })
    } else {
        Err(format!("no entry for {pid}"))
    }
}

/// #8531 Fail-Open Check: a persisted binding outlives its process. Once the
/// bound `claude` has exited — or its pid names a later process — the
/// binding grants nothing.
#[test]
fn a_bound_owner_whose_claude_exited_has_no_process_8531() {
    let state = DaemonState::new();
    let owner = SessionId::new();
    state.bind_session_claude(owner, OWNER).expect("vacant");
    let gone = owner_claude_with(&state, owner, |pid| Err(format!("no entry for {pid}")));
    assert!(
        gone.as_ref()
            .is_err_and(|e| e.contains("no longer running")),
        "{gone:?}"
    );
    let reused = owner_claude_with(&state, owner, |_| {
        Ok(crate::core::twin_arming::ProcessFacts {
            parent: Some(1),
            start_time: OWNER.start_time + 60,
        })
    });
    assert!(
        reused
            .as_ref()
            .is_err_and(|e| e.contains("no longer running")),
        "{reused:?}"
    );
}

/// #8531 LOW: a sealed registry refuses every owner and says it is sealed,
/// rather than blaming the owner for never announcing itself.
#[test]
fn a_sealed_registry_names_the_seal_not_the_owner_8531() {
    let root = tempfile::tempdir().expect("tempdir");
    let paths = FrameworkPaths::under(root.path());
    let file = DaemonState::with_paths(&paths)
        .framework_root()
        .join(crate::daemon::state::session_claudes::SESSION_CLAUDES_FILE);
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
    std::fs::write(&file, b"{\"version\":1,\"sessions\":").expect("corrupt it");
    // Owner-only, so the seal is the parse failure, not the mode.
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let state = DaemonState::with_paths(&paths);
    let got = owner_claude_with(&state, SessionId::new(), live_owner);
    assert!(
        got.as_ref()
            .is_err_and(|e| e.starts_with("the session-claude registry is sealed: ")
                && e.contains("does not parse")
                && !e.contains("never announced")),
        "{got:?}"
    );
}

/// #8531 Fail-Open Check: a `SessionStart` with no kernel pid binds nothing.
#[tokio::test]
async fn a_session_start_without_a_peer_pid_binds_nothing_8531() {
    let state = Arc::new(DaemonState::new());
    let session = SessionId::new();
    let got = bind_announcing_claude(&state, session, RepairPeer::from_socket(None)).await;
    assert!(got.is_err_and(|e| e.contains("could not read the calling process's pid")));
    assert_eq!(state.session_claudes().get(session), None);
}

/// #8531 Fail-Open Check: a `SessionStart` whose peer cannot be walked — it
/// has exited — binds nothing.
#[tokio::test]
async fn a_session_start_whose_walk_fails_binds_nothing_8531() {
    let mut gone = Command::new("true").spawn().expect("spawn true");
    let pid = gone.id();
    gone.wait().expect("reap true");
    let state = Arc::new(DaemonState::new());
    let session = SessionId::new();
    let got = bind_announcing_claude(&state, session, socket_peer(pid)).await;
    assert!(got.is_err_and(|e| e.contains("could not be read")));
    assert_eq!(state.session_claudes().get(session), None);
}

/// `/bin/bash` exec'd under the name `claude`, so the process table names it
/// so. A symlink, not a copy: macOS kills a copied system binary.
fn fake_claude(dir: &Path) -> PathBuf {
    let path = dir.join("claude");
    std::os::unix::fs::symlink("/bin/bash", &path).expect("symlink /bin/bash");
    path
}

/// Kills its child on drop, so a failed assertion leaves no process behind.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A fake `claude` running one `sleep`: (the `claude`, the sleep's pid).
fn claude_with_a_child(dir: &Path) -> (Reaped, u32) {
    let mut child = Command::new(fake_claude(dir))
        .arg("-c")
        .arg("sleep 30 & echo $!; wait")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the fake claude");
    let mut line = String::new();
    let out = child.stdout.take().expect("stdout");
    std::io::BufReader::new(out)
        .read_line(&mut line)
        .expect("read the child pid");
    let pid = line.trim().parse().expect("a pid");
    (Reaped(child), pid)
}

/// A live hook-registered owner with one open record, in a hermetic state.
fn owner_with_a_record(
    tmux_name: Option<&str>,
) -> (Arc<DaemonState>, SessionId, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::with_paths(&FrameworkPaths::under(root.path())));
    let owner = SessionId::new();
    let mut s = Session::new(owner, String::new(), ControlModel::Tmux, None);
    s.status = SessionStatus::Active;
    if let Some(name) = tmux_name {
        s.tmux_name = name.to_string();
    }
    state.register_session(s);
    let mut d = Delegation::observed(owner, "version-control", "task", Some("toolu-8531".into()));
    d.agent_id = Some("a8531".to_string());
    state.upsert_delegation(d);
    (state, owner, root)
}

/// #8531 CRITICAL regression: a pid written through `PATCH
/// /sessions/{id}/pid` / `mpm.sessions.set_pid` — here naming the caller's
/// own `claude` — does not make the caller the owner. The walk itself
/// succeeds; the owner has no kernel-bound process.
#[test]
fn a_patched_pid_does_not_establish_the_owner_8531() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (claude, caller_pid) = claude_with_a_child(dir.path());
    let (state, owner, _root) = owner_with_a_record(None);
    crate::daemon::rpc::sessions_legacy_ops::set_session_pid(
        &state,
        &owner.0.to_string(),
        claude.0.id(),
    )
    .expect("the shared set_pid body writes the pid");

    let caller = establish_caller(&state, socket_peer(caller_pid), |_| true);
    assert!(
        unestablished(&caller).contains("never announced"),
        "{caller:?}"
    );
}

/// #8531 CRITICAL regression: a tmux session squatted under the owner's tmux
/// name, running a `claude` the caller runs under, does not make the caller
/// the owner.
#[test]
fn a_squatted_tmux_name_does_not_establish_the_owner_8531() {
    use crate::test_support::tmux_session::{ScratchTmuxSession, reserved_session_name};
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = fake_claude(dir.path());
    let pid_file = dir.path().join("pid");
    let name = reserved_session_name("8531squat");
    let pane = format!(
        "'{}' -c 'sleep 30 & echo $! > {}; wait'; true",
        claude.display(),
        pid_file.display()
    );
    let _squat = ScratchTmuxSession::spawn("tmux", &name, &pane);
    let mut caller_pid = None;
    for _ in 0..200 {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            caller_pid = Some(pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let caller_pid = caller_pid.expect("the squatted pane started its claude");
    let (state, _owner, _root) = owner_with_a_record(Some(&name));

    let caller = establish_caller(&state, socket_peer(caller_pid), |_| true);
    assert!(
        unestablished(&caller).contains("never announced"),
        "{caller:?}"
    );
}

/// The daemon socket, served from `state` until the returned sender fires.
async fn serve(
    state: &Arc<DaemonState>,
    socket: &Path,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let bound = crate::daemon::socket::bind(socket).await.expect("bind");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn({
        let state = Arc::clone(state);
        async move {
            let _ = crate::daemon::socket::serve_until_shutdown(bound, state, async {
                let _ = stopped.await;
            })
            .await;
        }
    });
    (stop, server)
}

/// #8531 end to end over the real socket: this test process asserts the
/// owner's id in the old `caller_session` param and is refused — no socket
/// `SessionStart` bound the owner to a `claude` it runs under.
#[tokio::test]
async fn a_socket_caller_without_a_claude_owner_process_is_refused_8531() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, owner, _root) = owner_with_a_record(None);
    let socket = dir.path().join("mpm.sock");
    let (stop, server) = serve(&state, &socket).await;
    let client = crate::client::DaemonClient::over_socket(&socket);
    let mut answer = None;
    for _ in 0..200 {
        let sent = client
            .post("/api/v1/delegations/a8531/repair")
            .json(&serde_json::json!({ "force": true, "caller_session": owner.0.to_string() }))
            .send()
            .await;
        if let Ok(resp) = sent {
            answer = Some(resp.json::<serde_json::Value>().await.expect("json"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = stop.send(());
    let _ = server.await;

    let answer = answer.expect("the socket answered");
    assert_eq!(answer["outcome"], serde_json::json!("refused"), "{answer}");
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Running,
        "a refused repair writes nothing"
    );
}

const HELPER: &str = "daemon::services::delegation_repair_caller::delegation_repair_caller_tests::socket_client_helper_8531";
const HELPER_DIR_ENV: &str = "TM_TEST_8531_HELPER_DIR";
const HELPER_SESSION_ENV: &str = "TM_TEST_8531_SESSION";

/// Poll for `path` for up to 30 s.
async fn appears(path: &Path) -> bool {
    for _ in 0..1200 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// The process [`the_bound_owner_ends_its_own_record_over_the_socket_8531`]
/// runs under its fake `claude`. A no-op unless that test set its env.
///
/// What: announces the session with a socket `SessionStart`, writes
/// `bound`, waits for `go`, asks for the repair, and writes the answer to
/// `outcome`.
#[test]
#[ignore = "a child-process helper; a no-op when run directly"]
fn socket_client_helper_8531() {
    let (Some(dir), Some(session)) = (
        std::env::var_os(HELPER_DIR_ENV).map(PathBuf::from),
        std::env::var(HELPER_SESSION_ENV).ok(),
    ) else {
        return;
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        let client = crate::client::DaemonClient::over_socket(dir.join("mpm.sock"));
        let start = serde_json::json!({
            "session_id": session,
            "event": "SessionStart",
            "payload": {},
        });
        client
            .post("/hooks")
            .json(&start)
            .send()
            .await
            .expect("SessionStart over the socket");
        std::fs::write(dir.join("bound"), b"").expect("bound marker");
        assert!(appears(&dir.join("go")).await, "the parent never said go");
        let answer = client
            .post("/api/v1/delegations/a8531/repair")
            .json(&serde_json::json!({ "force": false }))
            .send()
            .await
            .expect("repair over the socket")
            .text()
            .await
            .expect("answer");
        std::fs::write(dir.join("outcome"), answer).expect("outcome");
    });
}

/// #8531 positive end to end: a session announced by a socket `SessionStart`
/// from under its `claude` ends its own live record, over the real socket,
/// from a process under that same `claude`.
///
/// Real: the socket, the kernel's peer pids, the process walk, start times,
/// the `claude` name read from the process table. The `claude` itself is
/// `/bin/bash` exec'd as `claude`, not Claude Code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bound_owner_ends_its_own_record_over_the_socket_8531() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::with_paths(&FrameworkPaths::under(root.path())));
    let (stop, server) = serve(&state, &dir.path().join("mpm.sock")).await;
    let owner = SessionId::new();
    let exe = std::env::current_exe().expect("test binary");
    let claude = Command::new(fake_claude(dir.path()))
        .arg("-c")
        .arg("\"$0\" --exact \"$1\" --ignored --test-threads=1; true")
        .arg(&exe)
        .arg(HELPER)
        .env(HELPER_DIR_ENV, dir.path())
        .env(HELPER_SESSION_ENV, owner.0.to_string())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn the fake claude");
    let claude = Reaped(claude);

    assert!(appears(&dir.path().join("bound")).await, "no SessionStart");
    let bound = state
        .session_claudes()
        .get(owner)
        .expect("SessionStart bound the owner");
    assert_eq!(
        bound.pid,
        claude.0.id(),
        "bound to the claude above the hook"
    );
    let mut d = Delegation::observed(owner, "version-control", "task", Some("toolu-8531".into()));
    d.agent_id = Some("a8531".to_string());
    state.upsert_delegation(d);
    std::fs::write(dir.path().join("go"), b"").expect("go");
    assert!(
        appears(&dir.path().join("outcome")).await,
        "no repair answer"
    );
    let _ = stop.send(());
    let _ = server.await;
    drop(claude);

    let answer = std::fs::read_to_string(dir.path().join("outcome")).expect("outcome");
    let answer: serde_json::Value = serde_json::from_str(&answer).expect("json");
    assert_eq!(answer["outcome"], serde_json::json!("ended"), "{answer}");
    assert!(state.all_delegations()[0].status.is_terminal());
}

/// #8531 HIGH regression: a daemon restart does not hand a bound owner's id
/// to a sibling. The owner binds; the daemon restarts on the same root; a
/// process under a sibling `claude` sends a `SessionStart` naming the owner's
/// id before the owner has any record. The binding stays the owner's, the
/// owner's process is granted its record, and the sibling's is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_does_not_hand_the_owner_id_to_a_sibling_8531() {
    let (owner_dir, sibling_dir) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    let (owner_claude, owner_peer) = claude_with_a_child(owner_dir.path());
    let (_sibling_claude, sibling_peer) = claude_with_a_child(sibling_dir.path());
    let root = tempfile::tempdir().expect("tempdir");
    let paths = FrameworkPaths::under(root.path());
    let owner = SessionId::new();

    let before = Arc::new(DaemonState::with_paths(&paths));
    bind_announcing_claude(&before, owner, socket_peer(owner_peer))
        .await
        .expect("the owner's SessionStart binds");
    drop(before);

    // The restart: a fresh daemon over the same framework root.
    let after = Arc::new(DaemonState::with_paths(&paths));
    let _ = bind_announcing_claude(&after, owner, socket_peer(sibling_peer)).await;
    let bound = after.session_claudes().get(owner).expect("still bound");
    assert_eq!(
        bound.pid,
        owner_claude.0.id(),
        "the sibling took the binding"
    );

    let mut d = Delegation::observed(owner, "version-control", "task", Some("toolu-8531".into()));
    d.agent_id = Some("a8531".to_string());
    after.upsert_delegation(d);
    let caller = establish_caller(&after, socket_peer(owner_peer), |_| true);
    assert_eq!(caller, RepairCaller::Session(owner), "the owner is granted");
    let caller = establish_caller(&after, socket_peer(sibling_peer), |_| true);
    unestablished(&caller);
}
