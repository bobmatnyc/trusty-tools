//! `tm ls` machine-readable and piped output against a stub daemon (#8506).
//!
//! Why: #8506 paints each `tm ls` row in its state color. The unit tests in
//! `commands::managed_tests` and `session_tui::tests` pin the colored bytes;
//! none of them proves that the BINARY keeps `--json` a byte-for-byte echo of
//! the daemon's body, or that a piped table — the shape every script reads —
//! carries no escape, `NO_COLOR` or not.
//! What: serves a fixed `GET /api/v1/sessions/managed` body holding one
//! session per colored state, points the binary at it with `TRUSTY_MPM_URL`,
//! and asserts on stdout. `--no-prune` keeps each listing a pure read.
//! Test: this file; run with `cargo test -p trusty-mpm --test tm_ls_state_colors`.

mod common;

use std::future::IntoFuture;

use axum::Router;
use axum::http::header;
use axum::routing::get;

/// The exact daemon body — spacing and key order included — that `--json`
/// must echo unchanged.
const BODY: &str = r#"{"sessions": [
  {"id": "00000000-0000-0000-0000-000000000001", "name": "tm-active-01", "state": "active"},
  {"id": "00000000-0000-0000-0000-000000000002", "name": "tm-stopped-02", "state": "stopped"},
  {"id": "00000000-0000-0000-0000-000000000003", "name": "tm-errored-03", "state": "errored"}
]}"#;

/// Serve [`BODY`] on the managed-session list route of an ephemeral port.
async fn serve_stub() -> String {
    let router = Router::new().route(
        "/api/v1/sessions/managed",
        get(|| async { ([(header::CONTENT_TYPE, "application/json")], BODY) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("read the bound address");
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// Run `tm ls <args>` against `url` with a piped stdout, `NO_COLOR` as given.
fn run_ls(url: &str, args: &[&str], no_color: Option<&str>) -> std::process::Output {
    let cwd = tempfile::tempdir().expect("temp cwd");
    let mut cmd = common::tm_command();
    cmd.env("TRUSTY_MPM_URL", url)
        .env_remove("NO_COLOR")
        .arg("ls")
        .args(args)
        .current_dir(cwd.path())
        .stdin(std::process::Stdio::null());
    if let Some(value) = no_color {
        cmd.env("NO_COLOR", value);
    }
    cmd.output().expect("spawn the tm binary")
}

/// `tm ls --json` prints the daemon's body byte-for-byte, colored states and all.
#[tokio::test(flavor = "multi_thread")]
async fn ls_json_echoes_the_daemon_body_byte_for_byte() {
    let url = serve_stub().await;
    let out = run_ls(&url, &["--json", "--no-prune"], None);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{BODY}\n"));
}

/// A piped `tm ls` table carries no escape, with `NO_COLOR` unset, empty, or set.
#[tokio::test(flavor = "multi_thread")]
async fn ls_piped_table_has_no_escapes_with_or_without_no_color() {
    let url = serve_stub().await;
    for no_color in [None, Some(""), Some("1")] {
        let out = run_ls(&url, &["--plain", "--no-prune"], no_color);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        for name in ["tm-active-01", "tm-stopped-02", "tm-errored-03"] {
            assert!(stdout.contains(name), "NO_COLOR={no_color:?}: {stdout}");
        }
        assert!(
            !stdout.contains('\u{1b}'),
            "NO_COLOR={no_color:?}: escape in piped table: {stdout:?}"
        );
    }
}
