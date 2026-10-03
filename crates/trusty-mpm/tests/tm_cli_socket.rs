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
        self.tm_with_url(args, &format!("http://{}", self.canary.addr()))
            .await
    }

    /// Run `tm args…` with `TRUSTY_MPM_URL` set to `url`.
    async fn tm_with_url(&self, args: &[&str], url: &str) -> Output {
        let mut cmd = common::tm_command_in(&self.dir.path().join("home"));
        cmd.current_dir(self.dir.path())
            .env("TRUSTY_MPM_SOCKET", self.socket())
            .env("TRUSTY_MPM_URL", url)
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

    // Not `reconcile-worktrees`: this in-process daemon resolves the workspace
    // root from the test process's own `$HOME`, so it would scan the operator's
    // real worktrees and outlive the 10 s bound on a loaded host. Its route is
    // pinned by `every_route_names_a_served_method`.
    for args in [
        &["sessions", "list"][..],
        &["sessions", "breakers"],
        &["sessions", "clean"],
        &["sessions", "ls", "--json"],
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

/// #6288 critic HIGH 1: a verb whose route takes a typed query field (`lines`,
/// `force`, `dry_run`, `record_only`) sends it typed, so the socket's params
/// struct decodes it. A stringified value answers `invalid_params` (400), which
/// is what every one of these verbs hit before the fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_query_verbs_decode_over_the_socket() {
    let scratch = Scratch::new();
    let _stop = serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;

    let ephemeral = scratch
        .tm(&["sessions", "decommission-ephemeral", "--dry-run"])
        .await;
    assert!(
        ephemeral.status.success(),
        "decommission-ephemeral --dry-run: {}",
        text(&ephemeral.stderr)
    );
    // A well-formed id nothing holds, so each verb reaches its typed query.
    let id = "00000000-0000-4000-8000-000000000000";
    for args in [
        &["sessions", "output", id][..],
        &["sessions", "delete", id],
        &["sessions", "decommission", id, "--force"],
    ] {
        let run = scratch.tm(args).await;
        let (out, err) = (text(&run.stdout), text(&run.stderr));
        assert!(
            !err.contains("params do not decode"),
            "{args:?} sent a param the socket could not decode: {err}"
        );
        // The daemon read the typed field and answered about the id itself.
        assert!(
            format!("{out}{err}").contains("not found"),
            "{args:?}: {out}{err}"
        );
    }
    assert_eq!(
        scratch.canary.connections(),
        0,
        "a socket command dialled TCP"
    );
}

/// #6288 critic HIGH 2: an explicit URL naming another host is refused before
/// anything runs — a destructive verb must not act on the LOCAL daemon in its
/// place — and a loopback URL is ignored with one stderr line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remote_url_is_refused_and_a_loopback_url_is_ignored() {
    let scratch = Scratch::new();
    let _stop = serve_socket_only(&scratch.dir.path().join("fw"), &scratch.socket()).await;

    let remote = scratch
        .tm_with_url(
            &["sessions", "delete", "00000000-0000-4000-8000-000000000000"],
            "http://100.64.0.1:7880",
        )
        .await;
    let err = text(&remote.stderr);
    assert!(
        !remote.status.success(),
        "a remote --url ran against the local daemon"
    );
    assert!(
        err.contains("another host") && err.contains("#6288"),
        "{err}"
    );
    assert!(
        !err.contains("not found"),
        "the verb ran before the refusal: {err}"
    );

    let local = scratch.tm(&["status"]).await;
    assert!(local.status.success(), "{}", text(&local.stderr));
    assert!(text(&local.stderr).contains("ignoring --url/TRUSTY_MPM_URL"));
    assert_eq!(
        scratch.canary.connections(),
        0,
        "a socket command dialled TCP"
    );
}
