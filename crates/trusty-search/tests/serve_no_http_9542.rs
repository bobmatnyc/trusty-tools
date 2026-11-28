//! `trusty-search serve` binds no TCP listener, whatever flags it gets (#9542).
//!
//! Why: ADR-0032 retires every trusty-* HTTP surface but the console's. The
//! last trusty-search one was the MCP HTTP/SSE transport behind
//! `serve --with-http`, `--http <addr>` and `--port`. A retired flag that is
//! silently ignored would leave an MCP client config pointing at an address
//! nothing serves, so each one must fail closed and name the issue.
//! What: runs the real binary's `serve` with each retired flag on a scratch
//! data dir, naming a loopback port the test holds for the whole run. Before
//! #9542 `serve --with-http --port <p>` bound `p` and served forever; against
//! a held port that bind fails, so the run cannot exit 1 naming #9542.
//! Test: `cargo test -p trusty-search --test integration serve_no_http_9542::`.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use crate::test_daemon;

/// How long one `serve` run may take before the test ends it.
const RUN_BOUND: Duration = Duration::from_secs(15);

/// A loopback port this test owns until the value is dropped.
///
/// Why (#11953): a port read off a dropped listener is free for any socket in
/// the parallel test binary, so a probe of it can reach a listener another
/// test opened and report it as serve's.
/// What: keeps the `127.0.0.1:0` listener open; nothing else can bind or
/// answer on [`ReservedPort::port`] while it lives.
/// Test: `reserved_port_is_held_by_its_own_listener_11953`.
struct ReservedPort {
    listener: TcpListener,
}

impl ReservedPort {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        Self { listener }
    }

    fn port(&self) -> u16 {
        self.listener.local_addr().expect("local addr").port()
    }
}

/// Run `serve <args>` hermetically on `data_dir`.
///
/// Why: `daemon.lock` names PID 1 and this test holds its flock (#11060), so
/// the stdio path's `ensure_daemon_up` waits on a socket instead of
/// auto-starting a daemon.
/// What: returns `(exit code, combined output)`.
fn run_serve(data_dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let home = data_dir.join("home");
    std::fs::create_dir_all(&home).expect("create the fake HOME");
    let _lock = test_daemon::hold_daemon_lock(data_dir);
    let mut cmd = test_daemon::command();
    cmd.arg("serve")
        .args(args)
        .current_dir(data_dir)
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TRUSTY_SKIP_RAM_CHECK", "1");
    let out = test_daemon::run_bounded(&mut cmd, RUN_BOUND);
    (out.code, out.combined)
}

/// Why (#9542): each retired flag must exit non-zero, name the issue, and
/// never bind or advertise a TCP port.
/// What: one run each for `--with-http --port <p>`, `--http 127.0.0.1:<p>`
/// and `--port <p>`, with `p` held by the test (#11953). Each must exit 1
/// with `#9542` in its output and report no bind failure, and no
/// `mcp_http_addr` discovery file may appear. A serve that tried to bind `p`
/// would fail with "address in use" instead of the #9542 refusal.
/// Test: this function.
#[test]
fn serve_binds_no_tcp_listener_9542() {
    let cases: [fn(&str) -> Vec<String>; 3] = [
        |p| vec!["--with-http".into(), "--port".into(), p.into()],
        |p| vec!["--http".into(), format!("127.0.0.1:{p}")],
        |p| vec!["--port".into(), p.into()],
    ];
    for case in cases {
        let tmp = tempfile::tempdir().expect("tempdir");
        // #11953: hold the port until the iteration ends, so no other socket
        // can take it while serve runs; a freed port was probed before.
        let reserved = ReservedPort::new();
        let port = reserved.port();
        let args = case(&port.to_string());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out) = run_serve(tmp.path(), &argv);
        let addr_file = tmp.path().join("home/.trusty-search/mcp_http_addr");

        assert!(
            !out.to_lowercase().contains("address already in use"),
            "#9542: `serve {argv:?}` tried to bind 127.0.0.1:{port}; the MCP HTTP listener is retired:\n{out}"
        );
        assert!(
            !addr_file.exists(),
            "#9542: `serve {argv:?}` wrote {}",
            addr_file.display()
        );
        assert_eq!(
            code,
            Some(1),
            "`serve {argv:?}` must fail closed with exit 1:\n{out}"
        );
        assert!(
            out.contains("#9542"),
            "`serve {argv:?}` must name #9542:\n{out}"
        );
    }
}

/// Why (#11953): the serve test may only judge sockets it created; a rival
/// socket that takes its port mid-run made it fail on another test's listener.
/// What: while a [`ReservedPort`] lives, a connect to its port succeeds and
/// the reservation's own listener accepts that exact connection, so the port
/// is occupied by the reservation and no other socket. Probes with a client
/// connect, never a second bind (ADR-0032 allows tests only `127.0.0.1:0`).
/// Test: this function.
#[test]
fn reserved_port_is_held_by_its_own_listener_11953() {
    let reserved = ReservedPort::new();
    let port = reserved.port();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let probe = TcpStream::connect_timeout(&addr, RUN_BOUND)
        .unwrap_or_else(|e| panic!("#11953: nothing holds reserved port {port}: {e}"));
    let (_accepted, peer) = reserved
        .listener
        .accept()
        .expect("#11953: the reservation accepts the probe");
    assert_eq!(
        peer,
        probe.local_addr().expect("probe local addr"),
        "#11953: reserved port {port} answered a socket other than the reservation"
    );
}
