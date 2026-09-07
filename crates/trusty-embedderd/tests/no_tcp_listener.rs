//! `trusty-embedderd` opens no TCP listener, in any configuration (#6289).
//!
//! Why: ADR-0032 says no trusty-* service owns an HTTP surface. This daemon's
//! `--http` mode bound `127.0.0.1:7890` and, worse, bound it by DEFAULT — a
//! bare `trusty-embedderd` with no flags at all took the port. These tests are
//! the standing proof that both are gone: that the retired flag is refused by
//! name rather than silently ignored, that the default configuration reaches no
//! listener at all, and that the one listener the daemon does open is the
//! hardened Unix socket.
//!
//! What: drives [`trusty_embedderd::resolve_transport`] over the parsed CLI
//! surface, then stands up the real accept loop against a real `BatchQueue`
//! (backed by `MockEmbedder`, so no ONNX model is needed) in a scratch
//! directory and asserts the socket's modes, its reachability through the one
//! shared client in `trusty-common`, and its two failure modes.
//!
//! Against `origin/main` the first three tests fail on behaviour, not on a
//! missing symbol: `Args::default()` there carries `http_addr =
//! "127.0.0.1:7890"` and `run_with_args` binds it.
//!
//! Test: this file. `cargo test -p trusty-embedderd --test no_tcp_listener`.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser as _;
use trusty_common::embedder::{Embedder, MockEmbedder, EMBED_DIM};
use trusty_common::embedder_client::{EmbedderClient, UdsEmbedderClient};
use trusty_embedderd::batch_queue::{BatchConfig, BatchQueue};
use trusty_embedderd::{uds_server, Args, Transport};

/// The port `--http` defaulted to before #6289 retired it.
///
/// Why: named once so the assertion and its failure message cannot drift apart.
const RETIRED_HTTP_PORT: u16 = 7890;

/// Parse an argv the way the binary does.
///
/// Why: every transport test starts from real argument parsing rather than a
/// hand-built `Args`, so a clap-level regression (a resurrected default, a
/// changed flag name) is caught too.
fn parse(argv: &[&str]) -> Args {
    Args::try_parse_from(argv).expect("argv parses")
}

/// A real `BatchQueue` over `MockEmbedder` — the production type, no ONNX.
fn mock_queue() -> Arc<BatchQueue> {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
    Arc::new(BatchQueue::new(embedder, BatchConfig::default()))
}

#[test]
fn bare_invocation_configures_no_transport() {
    // Why (#6289): the default configuration is the one that mattered — before
    // this change, `trusty-embedderd` with no flags bound 127.0.0.1:7890.
    // What: resolve the transport for a bare argv and assert it is refused
    // with an actionable message rather than resolving to any listener.
    // Test: this test.
    let err = trusty_embedderd::resolve_transport(&parse(&["trusty-embedderd"]))
        .expect_err("a bare invocation must not resolve to a transport");
    let msg = err.to_string();
    assert!(
        msg.contains("--stdio") && msg.contains("--socket"),
        "the refusal must name both usable transports, got: {msg}"
    );
    assert!(
        msg.contains("ADR-0032"),
        "the refusal must say why there is no TCP default, got: {msg}"
    );
}

#[test]
fn http_flag_is_refused_naming_the_adr() {
    // Why (#6289): a silently ignored `--http` would leave an operator
    // believing the daemon answers on the address they passed.
    // What: both spellings the old flag accepted are refused, and the message
    // names ADR-0032 and the replacement flag.
    // Test: this test.
    for argv in [
        vec!["trusty-embedderd", "--http", "127.0.0.1:7890"],
        vec!["trusty-embedderd", "--http"],
        vec!["trusty-embedderd", "--http", "127.0.0.1:7890", "--stdio"],
    ] {
        let err =
            trusty_embedderd::resolve_transport(&parse(&argv)).expect_err("--http must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("ADR-0032"),
            "argv {argv:?}: the refusal must cite ADR-0032, got: {msg}"
        );
        assert!(
            msg.contains("--socket"),
            "argv {argv:?}: the refusal must name the replacement flag, got: {msg}"
        );
    }
}

#[test]
fn stdio_flag_selects_the_stdio_transport() {
    assert_eq!(
        trusty_embedderd::resolve_transport(&parse(&["trusty-embedderd", "--stdio"]))
            .expect("--stdio resolves"),
        Transport::Stdio
    );
}

#[test]
fn socket_flag_selects_the_uds_transport() {
    let args = parse(&["trusty-embedderd", "--socket", "/tmp/x.sock"]);
    assert_eq!(
        trusty_embedderd::resolve_transport(&args).expect("--socket resolves"),
        Transport::Uds("/tmp/x.sock".into())
    );
}

#[tokio::test]
async fn daemon_serves_a_hardened_socket_and_no_tcp_port() {
    // Why (#6289): the closure condition for retiring `--http` is that the
    // remaining transport is both reachable and hardened, and that nothing is
    // left listening on the port the retired mode held.
    // What: binds the daemon's real socket in a scratch directory, serves it
    // with the real accept loop, embeds through the one shared UDS client in
    // trusty-common, asserts 0700/0600, and asserts 127.0.0.1:7890 refuses a
    // connection.
    // Test: this test.
    let tmp = tempfile::tempdir().expect("tempdir");
    // Short path: `sockaddr_un.sun_path` holds 104 bytes on macOS.
    let dir = tmp.path().join("s");
    let sock = dir.join("e.sock");
    std::fs::create_dir_all(&dir).expect("create socket dir");

    let listener = uds_server::bind_uds_listener(&sock).expect("bind hardened socket");
    tokio::spawn(uds_server::run_uds_accept_loop(listener, mock_queue()));

    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(&sock), 0o600, "socket must be 0600");
    assert_eq!(mode(&dir), 0o700, "socket directory must be 0700");

    let client = UdsEmbedderClient::new(&sock);
    let vectors = client
        .embed_batch(vec!["a".to_owned(), "b".to_owned()])
        .await
        .expect("embed over the socket");
    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors[0].len(), EMBED_DIM);

    // The retired listener: nothing answers on it. A connect (rather than a
    // bind) keeps this assertion from taking a resource a sibling test target
    // might want.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], RETIRED_HTTP_PORT));
    let connected = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500));
    assert!(
        connected.is_err(),
        "something is listening on 127.0.0.1:{RETIRED_HTTP_PORT}; trusty-embedderd retired \
         that listener in #6289, so either a stale pre-#6289 daemon is still running on this \
         host or the listener came back"
    );
}

#[tokio::test]
async fn a_missing_socket_is_a_typed_error_naming_the_path() {
    // Why (#6289): the failure an operator actually hits after the retire is
    // "I pointed at a socket that is not there". It has to say which path.
    // What: dial a path inside a real directory that holds no socket.
    // Test: this test.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("s");
    std::fs::create_dir_all(&dir).expect("create dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let missing = dir.join("absent.sock");

    let err = UdsEmbedderClient::new(&missing)
        .embed_batch(vec!["a".to_owned()])
        .await
        .expect_err("a missing socket must not embed");

    assert!(
        matches!(err, trusty_common::embedder_client::EmbedderError::Uds(_)),
        "a missing socket must map to the typed UDS variant, got: {err:?}"
    );
    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "the error must name the socket path, got: {err}"
    );
}

#[test]
fn crate_source_binds_no_tcp_listener() {
    // Why (#6289): the tests above prove the *configured* transports; this one
    // proves there is no other path to a TCP bind hiding in the daemon's
    // source. Same shape as `uds_accept_loop_sizes_the_accepted_socket`, which
    // this crate already uses for a call-graph property no behavioural test can
    // reach.
    // What: scans the three source files that own the daemon's startup and its
    // listeners for a TCP bind or an axum serve.
    // Test: this test.
    const SOURCES: &[(&str, &str)] = &[
        ("src/lib.rs", include_str!("../src/lib.rs")),
        ("src/uds_server.rs", include_str!("../src/uds_server.rs")),
        (
            "src/stdio_server.rs",
            include_str!("../src/stdio_server.rs"),
        ),
    ];
    for (name, source) in SOURCES {
        for forbidden in ["TcpListener", "axum::serve"] {
            assert!(
                !source.contains(forbidden),
                "{name} must not reference `{forbidden}` — trusty-embedderd binds no TCP \
                 port (#6289, ADR-0032)"
            );
        }
    }
}
