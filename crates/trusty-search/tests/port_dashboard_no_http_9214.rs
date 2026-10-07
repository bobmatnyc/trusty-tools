//! `port` and `dashboard` against a socket-only daemon (#9214 B2(d1)).
//!
//! Why: a `--no-http` daemon writes no `http_addr` or `daemon.port`, so a CLI
//! that reads those files reported "no daemon running" while a daemon answered
//! on its socket, printed a stale port file as the port, and let `dashboard`
//! dial whatever port discovery fell back to.
//! What: each test runs the built binary against a fresh `TRUSTY_DATA_DIR`
//! (and a fake `HOME`). A mock daemon on `<data_dir>/trusty-search.sock`
//! answers `search.health` with the transport under test and refuses every
//! other method `-32601`. Nothing here reaches the real daemon or spawns one.
//! Test: `cargo test -p trusty-search --test integration port_dashboard_no_http_9214::`.

use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::test_daemon;
use async_trait::async_trait;
use serde_json::{json, Value};
use trusty_common::uds::server::{serve_until, RpcError, RpcFallback, RpcRouter, RpcServeOptions};
use trusty_search::service::socket::METHOD_HEALTH;

/// Answers `search.health` with a fixed body; every other method is `-32601`.
struct HealthOnly(Value);

#[async_trait]
impl RpcFallback for HealthOnly {
    async fn call(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
        if method == METHOD_HEALTH {
            Ok(self.0.clone())
        } else {
            Err(RpcError::method_not_found(method, &[METHOD_HEALTH]))
        }
    }
}

/// Answers `search.health` with a JSON-RPC error; every other method is `-32601`.
struct HealthRefused;

#[async_trait]
impl RpcFallback for HealthRefused {
    async fn call(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
        Err(RpcError::method_not_found(method, &[]))
    }
}

/// A mock daemon on `socket` served by `fallback`. Dropping the returned
/// sender stops it.
async fn serve_mock(
    socket: &Path,
    fallback: impl RpcFallback + 'static,
) -> tokio::sync::oneshot::Sender<()> {
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind the mock socket");
    let router = Arc::new(RpcRouter::new().fallback(fallback));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });
    stop
}

/// A mock daemon on `socket` whose health reports `http_addr`.
async fn mock_daemon(socket: &Path, http_addr: Option<&str>) -> tokio::sync::oneshot::Sender<()> {
    let health = json!({
        "status": "ok",
        "transport": { "socket_path": socket, "http_addr": http_addr },
    });
    serve_mock(socket, HealthOnly(health)).await
}

/// The socket the binary derives from `TRUSTY_DATA_DIR`.
fn socket_in(data_dir: &Path) -> PathBuf {
    data_dir.join("trusty-search.sock")
}

/// The binary, isolated onto `data_dir`, with `args`.
fn cli(data_dir: &Path, args: &[&str]) -> Command {
    let home = data_dir.join("home");
    std::fs::create_dir_all(&home).expect("create the fake HOME");
    let mut cmd = test_daemon::command();
    cmd.args(args)
        .current_dir(data_dir)
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env_remove("TRUSTY_SEARCH_SOCKET")
        .env("RUST_LOG", "warn");
    cmd
}

/// Run a command that must not spawn anything, off the async runtime.
async fn output(mut cmd: Command) -> Output {
    tokio::task::spawn_blocking(move || {
        cmd.stdin(Stdio::null())
            .output()
            .expect("run trusty-search")
    })
    .await
    .expect("join the CLI run")
}

/// A loopback port nothing listens on.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Why (#9214): a daemon answering on its socket is running, even with no
/// HTTP listener; `port` must say which of the two is missing.
/// What: a socket-only mock daemon and no discovery files; asserts exit 1, a
/// message naming the socket, nothing on stdout, and no "no daemon running".
/// Test: this function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_names_the_socket_when_the_daemon_is_http_less() {
    let dir = tempfile::tempdir().expect("data dir");
    let socket = socket_in(dir.path());
    let _daemon = mock_daemon(&socket, None).await;

    let out = output(cli(dir.path(), &["port"])).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    let expected = format!(
        "no HTTP listener (socket-only daemon at {})",
        socket.display()
    );
    assert!(
        stderr.contains(&expected),
        "stderr must name the socket: {stderr}"
    );
    assert!(
        !stderr.contains("no daemon running"),
        "a daemon answered: {stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "nothing may print as the port: {stdout}"
    );
}

/// Why (#9214): `daemon.port` and `http_addr` outlive the daemon that wrote
/// them; a dead socket is the only proof that no daemon runs.
/// What: no socket, both discovery files naming a closed port; asserts exit 1,
/// "no daemon running" naming the socket, and the stale port printed nowhere.
/// Test: this function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_reports_no_daemon_only_when_the_socket_is_dead() {
    let dir = tempfile::tempdir().expect("data dir");
    let stale = closed_port();
    std::fs::write(dir.path().join("daemon.port"), stale.to_string()).expect("plant daemon.port");
    std::fs::write(dir.path().join("http_addr"), format!("127.0.0.1:{stale}"))
        .expect("plant http_addr");

    let out = output(cli(dir.path(), &["port"])).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {stdout} stderr: {stderr}"
    );
    assert!(stderr.contains("no daemon running"), "stderr: {stderr}");
    let socket = socket_in(dir.path()).display().to_string();
    assert!(
        stderr.contains(&socket),
        "stderr must name the socket: {stderr}"
    );
    assert!(
        !stdout.contains(&stale.to_string()),
        "a stale port file is not the port: {stdout}"
    );
}

/// Why (#9214, rule 4): with HTTP up, every `port` format keeps its output.
/// What: a mock daemon reporting `127.0.0.1:<p>`; asserts the bare, `--addr`
/// and `--json` outputs and exit 0.
/// Test: this function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_prints_the_http_address_the_daemon_reports() {
    let dir = tempfile::tempdir().expect("data dir");
    let port = closed_port();
    let addr = format!("127.0.0.1:{port}");
    let _daemon = mock_daemon(&socket_in(dir.path()), Some(&addr)).await;

    let cases = [
        (vec!["port"], port.to_string()),
        (vec!["port", "--addr"], addr.clone()),
        (
            vec!["port", "--json"],
            format!(r#"{{"addr":"127.0.0.1","port":{port}}}"#),
        ),
    ];
    for (args, expected) in cases {
        let out = output(cli(dir.path(), &args)).await;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?} stderr: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{expected}\n"),
            "{args:?}"
        );
    }
}

/// A TCP listener that counts connections and answers each `503`.
struct CountingListener {
    port: u16,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl CountingListener {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the counting listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("local addr").port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, halt) = (Arc::clone(&accepted), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !halt.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut conn, _)) => {
                        count.fetch_add(1, Ordering::SeqCst);
                        let _ = conn.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            port,
            accepted,
            stop,
        }
    }
}

impl Drop for CountingListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Why (#9214): under a socket-only daemon the dashboard has no address to
/// open. Port discovery would fall back to a discovery file or the default
/// port, and dialling that reaches whatever else listens there.
/// What: a socket-only mock daemon; `http_addr` and `daemon.port` name a live
/// listener that counts connections (the address the old fallback chain
/// resolves to), and `daemon.lock` names this test's own pid so nothing is
/// spawned. Asserts a non-zero exit naming the socket and `--no-http`, no
/// "Opening" line, and zero connections to the listener.
/// Test: this function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_never_dials_a_default_port() {
    let dir = tempfile::tempdir().expect("data dir");
    let socket = socket_in(dir.path());
    let _daemon = mock_daemon(&socket, None).await;
    let listener = CountingListener::start();
    std::fs::write(
        dir.path().join("http_addr"),
        format!("127.0.0.1:{}", listener.port),
    )
    .expect("plant http_addr");
    std::fs::write(dir.path().join("daemon.port"), listener.port.to_string())
        .expect("plant daemon.port");
    std::fs::write(
        dir.path().join("daemon.lock"),
        std::process::id().to_string(),
    )
    .expect("plant daemon.lock");

    let mut cmd = cli(dir.path(), &["dashboard"]);
    let run = tokio::task::spawn_blocking(move || {
        test_daemon::run_bounded(&mut cmd, Duration::from_secs(90))
    })
    .await
    .expect("join the dashboard run");

    assert_eq!(
        listener.accepted.load(Ordering::SeqCst),
        0,
        "dashboard dialled a TCP port: {}",
        run.combined
    );
    assert!(
        matches!(run.code, Some(code) if code != 0),
        "dashboard must fail: {:?} {}",
        run.code,
        run.combined
    );
    let expected = format!(
        "no HTTP listener (socket-only daemon at {})",
        socket.display()
    );
    assert!(run.combined.contains(&expected), "{}", run.combined);
    assert!(run.combined.contains("--no-http"), "{}", run.combined);
    assert!(
        !run.combined.contains("Opening"),
        "no browser may open: {}",
        run.combined
    );
}

/// Run `port` against `data_dir` and return (exit code, stdout, stderr).
async fn run_port(data_dir: &Path) -> (Option<i32>, String, String) {
    let out = output(cli(data_dir, &["port"])).await;
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Why (#9214): a pre-#9030 daemon reports no `transport`; with no `http_addr`
/// file either, there is no address to print, and `port` must not guess one.
/// What: a mock daemon whose health body has no `transport`; no `http_addr`;
/// `daemon.port` names a counting listener. Asserts exit 1, the "reported no
/// HTTP address" message naming the socket, empty stdout, zero connections.
/// Test: this function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_fails_closed_when_the_daemon_reports_no_http_address() {
    let dir = tempfile::tempdir().expect("data dir");
    let socket = socket_in(dir.path());
    let _daemon = serve_mock(&socket, HealthOnly(json!({ "status": "ok" }))).await;
    let listener = CountingListener::start();
    std::fs::write(dir.path().join("daemon.port"), listener.port.to_string())
        .expect("plant daemon.port");

    let (code, stdout, stderr) = run_port(dir.path()).await;

    assert_eq!(code, Some(1), "stdout: {stdout} stderr: {stderr}");
    let expected = format!(
        "the daemon at socket {} reported no HTTP address; restart it",
        socket.display()
    );
    assert!(stderr.contains(&expected), "stderr: {stderr}");
    assert!(stdout.trim().is_empty(), "stdout: {stdout}");
    assert_eq!(listener.accepted.load(Ordering::SeqCst), 0, "dialled TCP");
}

/// Why (#9214): a live socket whose `search.health` fails is neither "no
/// daemon running" nor a port; `port` must say the daemon did not answer.
/// What: a mock daemon refusing every method including `search.health`;
/// `daemon.port` names a counting listener. Asserts exit 1, the "did not
/// answer" message naming the socket, no "no daemon running", empty stdout,
/// zero connections.
/// Test: this function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_fails_closed_when_a_live_socket_refuses_health() {
    let dir = tempfile::tempdir().expect("data dir");
    let socket = socket_in(dir.path());
    let _daemon = serve_mock(&socket, HealthRefused).await;
    let listener = CountingListener::start();
    std::fs::write(dir.path().join("daemon.port"), listener.port.to_string())
        .expect("plant daemon.port");

    let (code, stdout, stderr) = run_port(dir.path()).await;

    assert_eq!(code, Some(1), "stdout: {stdout} stderr: {stderr}");
    let expected = format!("the daemon at socket {} did not answer", socket.display());
    assert!(stderr.contains(&expected), "stderr: {stderr}");
    assert!(!stderr.contains("no daemon running"), "stderr: {stderr}");
    assert!(stdout.trim().is_empty(), "stdout: {stdout}");
    assert_eq!(listener.accepted.load(Ordering::SeqCst), 0, "dialled TCP");
}
