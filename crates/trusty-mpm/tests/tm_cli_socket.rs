//! The sandbox-reached `tm` commands run over the daemon's unix socket alone
//! and never dial TCP (#6288 step 1, #8926).
//!
//! Why: a sandboxed session has no loopback TCP (Q2), and ADR-0032 forbids an
//! HTTP fallback — a stale address can name an unrelated process. So each
//! command is driven against a daemon that serves ONLY a unix socket, with
//! every TCP discovery source (`TRUSTY_MPM_URL` and the console gateway's
//! discovery file) pointed at a canary listener that must see no connection.
//! What: an in-process daemon bound with `daemon::socket::bind` and served by
//! `serve_until_shutdown` — no TCP listener exists — at a socket path
//! containing a space (as macOS's `Application Support` default does), handed
//! to the real `tm` binary through `TRUSTY_MPM_SOCKET`.
//! Test: this file IS the test.

#![cfg(feature = "daemon")]

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::daemon::state::DaemonState;

use super::common;

/// A TCP listener every TCP discovery source points at. Nothing may reach it.
struct Canary {
    listener: TcpListener,
}

impl Canary {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind canary");
        listener.set_nonblocking(true).expect("nonblocking");
        Self { listener }
    }

    fn addr(&self) -> String {
        self.listener.local_addr().expect("addr").to_string()
    }

    /// How many connections reached the canary (drains the accept queue).
    fn connections(&self) -> usize {
        let mut n = 0;
        loop {
            match self.listener.accept() {
                Ok(_) => n += 1,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return n,
                Err(e) => panic!("canary accept: {e}"),
            }
        }
    }
}

/// A scratch `$HOME`, a data dir whose console discovery file names the
/// canary, and a socket path with a space in it.
struct Scratch {
    dir: tempfile::TempDir,
    canary: Canary,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let canary = Canary::new();
        let console = dir.path().join("data").join("trusty-console");
        std::fs::create_dir_all(&console).expect("console data dir");
        std::fs::write(console.join("http_addr"), canary.addr()).expect("console addr");
        std::fs::create_dir_all(dir.path().join("home")).expect("home");
        Self { dir, canary }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("App Support").join("trusty-mpm.sock")
    }

    /// Run `tm args…` with every TCP source aimed at the canary.
    async fn tm(&self, args: &[&str]) -> Output {
        let mut cmd = common::tm_command_in(&self.dir.path().join("home"));
        cmd.current_dir(self.dir.path())
            .env("TRUSTY_MPM_SOCKET", self.socket())
            .env("TRUSTY_MPM_URL", format!("http://{}", self.canary.addr()))
            .env("TRUSTY_DATA_DIR_OVERRIDE", self.dir.path().join("data"))
            .args(args);
        tokio::task::spawn_blocking(move || cmd.output())
            .await
            .expect("join")
            .expect("run tm")
    }
}

/// Serve a daemon on `socket` only, until the returned sender fires.
async fn serve_socket_only(root: &Path, socket: &Path) -> tokio::sync::oneshot::Sender<()> {
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `tm health`, `tm status`, `tm doctor` and the daemon-reaching `tm sessions`
/// verbs answer from a socket-only daemon, and the canary sees no connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tm_health_status_and_doctor_work_over_the_socket_alone() {
    let scratch = Scratch::new();
    let _stop = serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;

    let health = scratch.tm(&["health"]).await;
    assert!(health.status.success(), "health: {}", text(&health.stderr));
    let out = text(&health.stdout);
    assert!(out.contains("resolved: unix socket"), "{out}");
    assert!(
        out.contains("App Support"),
        "the spaced path is used whole: {out}"
    );

    let status = scratch.tm(&["status"]).await;
    assert!(status.status.success(), "status: {}", text(&status.stderr));
    assert!(
        text(&status.stdout).contains("daemon: reachable"),
        "{}",
        text(&status.stdout)
    );

    let doctor = scratch.tm(&["doctor"]).await;
    let out = text(&doctor.stdout);
    assert!(
        out.contains("trusty-mpm daemon: reachable (via unix socket"),
        "{out}"
    );

    for args in [
        &["sessions", "list"][..],
        &["sessions", "breakers"],
        &["sessions", "clean"],
        &["sessions", "ls", "--json"],
        &["sessions", "reconcile-worktrees", "--json"],
    ] {
        let run = scratch.tm(args).await;
        assert!(run.status.success(), "{args:?}: {}", text(&run.stderr));
    }
    let info = scratch.tm(&["sessions", "info", "nope"]).await;
    assert!(!info.status.success());
    assert!(
        text(&info.stderr).contains("not found"),
        "{}",
        text(&info.stderr)
    );

    assert_eq!(
        scratch.canary.connections(),
        0,
        "a socket command dialled TCP"
    );
}

/// Fail-open check (#6288): with the socket absent and a TCP listener on
/// `TRUSTY_MPM_URL`, `tm health` and `tm status` fail naming the socket — never
/// "healthy", never exit 0, and never a TCP connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tm_health_over_an_absent_socket_fails_and_never_dials_tcp() {
    let scratch = Scratch::new();
    let socket = scratch.socket().display().to_string();

    for verb in ["health", "status"] {
        let run = scratch.tm(&[verb]).await;
        assert!(!run.status.success(), "{verb} exited 0 with no socket");
        let err = text(&run.stderr);
        assert!(err.contains(&socket), "{verb} must name the socket: {err}");
        let out = text(&run.stdout);
        assert!(!out.contains("daemon: ok"), "{verb}: {out}");
        assert!(!out.contains("reachable ("), "{verb}: {out}");
    }
    assert_eq!(scratch.canary.connections(), 0, "fell back to TCP");
}
