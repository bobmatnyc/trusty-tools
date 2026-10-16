//! #6288 client cutover: commands that reached the daemon over HTTP 7880 now
//! reach it over its unix socket only, and fail closed naming the socket.
//!
//! Why: before the cutover `tm events`, `tm sessctl`, `tm hook` and the MCP
//! stdio bridge resolved a TCP URL — `--url`/`TRUSTY_MPM_URL`, the console
//! gateway, the lock file, then `127.0.0.1:7880`. Each case here was red on
//! 7eafeef585: the command dialled the canary TCP listener, or failed naming
//! a URL rather than the socket.
//! What: the real `tm` binary with `TRUSTY_MPM_SOCKET` aimed at a socket in a
//! scratch directory (served by an in-process daemon, absent, stale, behind a
//! directory it may not search, or refusing the peer) and `TRUSTY_MPM_URL`
//! aimed at a canary TCP listener that must see no connection.
//! Test: this file IS the test.

#![cfg(feature = "daemon")]

use std::io::{BufRead as _, BufReader, ErrorKind, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use trusty_mpm::core::hook::HookEvent;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::session::SessionId;
use trusty_mpm::daemon::state::DaemonState;

use super::common;

/// A TCP listener every TCP source points at. Nothing may reach it.
struct Canary(TcpListener);

impl Canary {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind canary");
        listener.set_nonblocking(true).expect("nonblocking");
        Self(listener)
    }

    fn url(&self) -> String {
        format!("http://{}", self.0.local_addr().expect("addr"))
    }

    /// How many connections reached the canary (drains the accept queue).
    fn connections(&self) -> usize {
        let mut n = 0;
        loop {
            match self.0.accept() {
                Ok(_) => n += 1,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return n,
                Err(e) => panic!("canary accept: {e}"),
            }
        }
    }
}

/// A scratch home, an isolated data dir, and a socket path with a space in it.
struct Scratch {
    dir: tempfile::TempDir,
    canary: Canary,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("home")).expect("home");
        std::fs::create_dir_all(dir.path().join("data")).expect("data");
        Self {
            dir,
            canary: Canary::new(),
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("App Support").join("trusty-mpm.sock")
    }

    /// `tm args…` with the socket pinned and `TRUSTY_MPM_URL` set to `url`.
    fn command(&self, args: &[&str], url: &str) -> Command {
        let mut cmd = common::tm_command_in(&self.dir.path().join("home"));
        cmd.current_dir(self.dir.path())
            .env("TRUSTY_MPM_SOCKET", self.socket())
            .env("TRUSTY_MPM_URL", url)
            .env("TRUSTY_DATA_DIR_OVERRIDE", self.dir.path().join("data"))
            .args(args);
        cmd
    }

    /// Run `tm args…` with every TCP source aimed at the canary.
    async fn tm(&self, args: &[&str]) -> Output {
        self.tm_with_url(args, &self.canary.url()).await
    }

    async fn tm_with_url(&self, args: &[&str], url: &str) -> Output {
        let mut cmd = self.command(args, url);
        cmd.stdin(Stdio::null());
        tokio::task::spawn_blocking(move || cmd.output())
            .await
            .expect("join")
            .expect("run tm")
    }
}

/// Serve a daemon on `socket` only, until the returned sender fires.
async fn serve_socket_only(
    root: &Path,
    socket: &Path,
) -> (Arc<DaemonState>, tokio::sync::oneshot::Sender<()>) {
    let paths = FrameworkPaths::under(root);
    for dir in [&paths.hooks, &paths.instructions, &paths.agents] {
        std::fs::create_dir_all(dir).expect("framework dir");
    }
    let state = Arc::new(DaemonState::with_paths(&paths));
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("socket dir");
    let bound = trusty_mpm::daemon::socket::bind(socket)
        .await
        .expect("bind socket");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(trusty_mpm::daemon::socket::serve_until_shutdown(
        bound,
        Arc::clone(&state),
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
    (state, stop)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Assert `run` failed, named `socket`, and that the canary saw nothing.
fn assert_fails_naming(run: &Output, socket: &Path, canary: &Canary, what: &str) {
    let err = text(&run.stderr);
    assert!(!run.status.success(), "{what}: exited 0\n{err}");
    assert!(
        err.contains(&socket.display().to_string()),
        "{what}: the error must name the socket\n{err}"
    );
    assert_eq!(canary.connections(), 0, "{what}: fell back to TCP");
}

/// Fail-open check: with no socket, a command that used HTTP fails naming the
/// socket and never dials the TCP URL it was given.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_missing_fails_naming_the_socket_and_never_dials_tcp() {
    let scratch = Scratch::new();
    for args in [&["events"][..], &["project", "info"], &["sessctl", "list"]] {
        let run = scratch.tm(args).await;
        assert_fails_naming(&run, &scratch.socket(), &scratch.canary, &args.join(" "));
    }
}

/// A socket the client may not reach (an ancestor it cannot search) is an
/// error naming the socket and the permission refusal — never a TCP retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_denied_fails_naming_the_socket() {
    if trusty_common::uds::self_uid() == 0 {
        eprintln!("root ignores directory modes; nothing to deny");
        return;
    }
    let scratch = Scratch::new();
    let (_state, _stop) =
        serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;
    let ancestor = scratch.dir.path().join("App Support");
    // The socket's own directory must stay 0700; the refusal comes from above.
    let locked = scratch.dir.path().join("locked");
    std::fs::create_dir_all(locked.join("s")).expect("dirs");
    std::fs::set_permissions(locked.join("s"), std::fs::Permissions::from_mode(0o700))
        .expect("0700");
    std::fs::rename(&ancestor, locked.join("s").join("App Support")).expect("move socket dir");
    let socket = locked.join("s").join("App Support").join("trusty-mpm.sock");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");

    let mut cmd = scratch.command(&["events"], &scratch.canary.url());
    cmd.env("TRUSTY_MPM_SOCKET", &socket).stdin(Stdio::null());
    let run = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .expect("join")
        .expect("run tm");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock");

    assert_fails_naming(&run, &socket, &scratch.canary, "permission denied");
    assert!(
        text(&run.stderr)
            .to_lowercase()
            .contains("permission denied"),
        "{}",
        text(&run.stderr)
    );
}

/// A stale socket file (nothing serves it) is an error naming the socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_socket_fails_naming_the_socket() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.socket().parent().expect("parent")).expect("dir");
    drop(trusty_common::uds::bind_hardened(&scratch.socket()).expect("bind"));
    assert!(scratch.socket().exists(), "fixture: a stale socket file");
    let run = scratch.tm(&["events"]).await;
    assert_fails_naming(&run, &scratch.socket(), &scratch.canary, "stale socket");
}

/// The daemon drops a connection whose peer uid is not its own before reading
/// a byte (`ensure_peer_is_self`). A foreign uid cannot be produced without
/// root, so the listener here does exactly what the daemon does to one:
/// accept and close. The client reports that, naming the socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_refused_by_the_daemon_fails_naming_the_socket() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.socket().parent().expect("parent")).expect("dir");
    let listener = trusty_common::uds::bind_hardened(&scratch.socket()).expect("bind");
    let refuse = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let run = scratch.tm(&["events"]).await;
    refuse.abort();
    assert_fails_naming(&run, &scratch.socket(), &scratch.canary, "peer refused");
}

/// Q3: a loopback `TRUSTY_MPM_URL` is ignored with one notice, and the command
/// runs over the socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loopback_url_is_ignored_with_a_notice() {
    let scratch = Scratch::new();
    let (_state, _stop) =
        serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;
    let run = scratch.tm(&["events"]).await;
    assert!(run.status.success(), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("ignoring --url/TRUSTY_MPM_URL"),
        "{}",
        text(&run.stderr)
    );
    assert_eq!(scratch.canary.connections(), 0, "dialled TCP");
}

/// Q3: a remote `TRUSTY_MPM_URL` is refused before the command runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remote_url_is_refused() {
    let scratch = Scratch::new();
    let (_state, _stop) =
        serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;
    let run = scratch
        .tm_with_url(&["events"], "http://100.64.0.1:7880")
        .await;
    let err = text(&run.stderr);
    assert!(!run.status.success(), "a remote URL ran: {err}");
    assert!(
        err.contains("another host") && err.contains("#6288"),
        "{err}"
    );
}

/// A hook's daemon post travels over the socket: the daemon ingests it, and
/// the canary named by the hook's `TRUSTY_MPM_URL` sees nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_reaches_the_daemon_over_the_socket_only() {
    let scratch = Scratch::new();
    let (state, _stop) = serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;
    let session = SessionId::new();
    let payload = serde_json::json!({
        "session_id": session.0.to_string(),
        "hook_event_name": "Stop",
        "cwd": scratch.dir.path().display().to_string(),
    });
    let mut cmd = scratch.command(&["hook"], &scratch.canary.url());
    cmd.env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_NOTIFY_INBOX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = tokio::task::spawn_blocking(move || {
        let mut child = cmd.spawn().expect("spawn tm hook");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.to_string().as_bytes())
            .expect("payload");
        child.wait_with_output().expect("wait")
    })
    .await
    .expect("join");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let ingested = state
        .recent_hook_events()
        .into_iter()
        .any(|r| r.session == session && r.event == HookEvent::Stop);
    assert!(ingested, "the hook post never reached the socket daemon");
    assert_eq!(scratch.canary.connections(), 0, "the hook dialled TCP");
}

/// The MCP stdio bridge answers a request through the socket daemon and opens
/// no TCP connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_bridge_answers_over_the_socket_only() {
    let scratch = Scratch::new();
    let (_state, _stop) =
        serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;
    let mut cmd = scratch.command(&["serve", "--stdio"], &scratch.canary.url());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let line = tokio::task::spawn_blocking(move || {
        let mut child = cmd.spawn().expect("spawn the bridge");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/list\"}\n")
            .expect("request");
        let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        line
    })
    .await
    .expect("join");
    let reply: serde_json::Value = serde_json::from_str(&line).expect("one JSON-RPC reply");
    assert_eq!(reply["id"], 7, "{reply}");
    assert!(reply.get("result").is_some(), "{reply}");
    assert_eq!(scratch.canary.connections(), 0, "the bridge dialled TCP");
}

/// With no socket, the isolated bridge refuses at start, naming the socket,
/// and neither spawns a daemon nor dials TCP.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_bridge_refuses_a_missing_socket_without_spawning() {
    let scratch = Scratch::new();
    let mut cmd = scratch.command(&["serve", "--stdio"], &scratch.canary.url());
    cmd.env("TRUSTY_SANDBOX", "1").stdin(Stdio::null());
    let run = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .expect("join")
        .expect("run the bridge");
    assert_fails_naming(&run, &scratch.socket(), &scratch.canary, "bridge");
    assert!(!scratch.socket().exists(), "the bridge spawned a daemon");
}

/// `tm sessctl connect` reads the `mpm.control.connect` stream over the
/// socket; an unknown session is the daemon's own refusal, not a TCP call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessctl_connect_streams_over_the_socket() {
    let scratch = Scratch::new();
    let (_state, _stop) =
        serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;
    let run = scratch.tm(&["sessctl", "connect", "ghost"]).await;
    let err = text(&run.stderr);
    assert!(!run.status.success(), "{err}");
    assert!(err.contains("session ghost not found"), "{err}");
    assert_eq!(scratch.canary.connections(), 0, "dialled TCP");
}
