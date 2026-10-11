//! `trusty-search serve` binds no TCP listener, whatever flags it gets (#9542).
//!
//! Why: ADR-0032 retires every trusty-* HTTP surface but the console's. The
//! last trusty-search one was the MCP HTTP/SSE transport behind
//! `serve --with-http`, `--http <addr>` and `--port`. A retired flag that is
//! silently ignored would leave an MCP client config pointing at an address
//! nothing serves, so each one must fail closed and name the issue.
//! What: runs the real binary's `serve` with each retired flag on a scratch
//! data dir, polling the port it names while the process runs. Before #9542
//! `serve --with-http --port <p>` bound `p` and served forever.
//! Test: `cargo test -p trusty-search --test integration serve_no_http_9542::`.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use crate::test_daemon;

/// How long one `serve` run may take before the test ends it.
const RUN_BOUND: Duration = Duration::from_secs(15);

/// A loopback port nothing is bound to right now.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Run `serve <args>` hermetically on `data_dir`, polling `port` while it runs.
///
/// Why: `daemon.lock` names PID 1, which is always alive, so the stdio path's
/// `ensure_daemon_up` waits on a socket instead of auto-starting a daemon.
/// What: returns `(exit code, combined output, whether port answered)`.
fn run_serve(data_dir: &Path, args: &[&str], port: u16) -> (Option<i32>, String, bool) {
    let home = data_dir.join("home");
    std::fs::create_dir_all(&home).expect("create the fake HOME");
    std::fs::write(data_dir.join("daemon.lock"), b"1").expect("write daemon.lock");
    let mut cmd = test_daemon::command();
    cmd.arg("serve")
        .args(args)
        .current_dir(data_dir)
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TRUSTY_SKIP_RAM_CHECK", "1");
    let run = std::thread::spawn(move || test_daemon::run_bounded(&mut cmd, RUN_BOUND));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut answered = false;
    while !run.is_finished() {
        answered |= TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok();
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = run.join().expect("the serve run thread");
    (out.code, out.combined, answered)
}

/// Why (#9542): each retired flag must exit non-zero, name the issue, and
/// never bind or advertise a TCP port.
/// What: one run each for `--with-http --port <p>`, `--http 127.0.0.1:<p>`
/// and `--port <p>`. Each must exit 1 with `#9542` in its output, `p` must
/// never answer, and no `mcp_http_addr` discovery file may appear.
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
        let port = free_port();
        let args = case(&port.to_string());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out, answered) = run_serve(tmp.path(), &argv, port);
        let addr_file = tmp.path().join("home/.trusty-search/mcp_http_addr");

        assert!(
            !answered,
            "#9542: `serve {argv:?}` bound 127.0.0.1:{port}; the MCP HTTP listener is retired"
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
