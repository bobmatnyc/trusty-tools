//! #10476: the analyze health probe over real sockets — "nothing listening"
//! is on demand, every other failure is Down. #10901: the Events tile's
//! `events.health` probe over the same stub sockets; it has no on-demand
//! lifecycle, so every failure is Down or Timeout.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

use super::probe::{classify, on_demand_when_idle, uds_health, with_deadline};
use super::services::{ProbeKind, ServiceId};
use super::state::TileHealth;

const DEADLINE: Duration = Duration::from_secs(2);

/// What the stub peer does with each accepted connection.
#[derive(Clone, Copy)]
enum Peer {
    /// Write these bytes, then close.
    Reply(&'static [u8]),
    /// Close without writing anything.
    HangUp,
    /// Hold the connection open and never answer.
    Hold,
}

/// A `0700` temp directory, so the dial's directory check passes and only the
/// socket itself decides the outcome.
fn socket_dir() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("narrow the tempdir");
    let socket = dir.path().join("analyze.sock");
    (dir, socket)
}

/// Bind a hardened listener at `socket` and serve `peer` on every connection.
fn serve(socket: &Path, peer: Peer) {
    let listener: UnixListener = trusty_common::uds::bind_hardened(socket).expect("bind stub");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                match peer {
                    Peer::Reply(bytes) => {
                        let _ = stream.write_all(bytes).await;
                        let _ = stream.shutdown().await;
                    }
                    Peer::HangUp => drop(stream),
                    Peer::Hold => {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        drop(stream);
                    }
                }
            });
        }
    });
}

/// The analyze tile for one `analyze.health` probe of `socket`.
async fn analyze_tile(socket: &Path, deadline: Duration) -> TileHealth {
    let observed = uds_health(socket, "analyze.health", deadline).await;
    classify(on_demand_when_idle(ServiceId::Analyze, observed), true)
}

fn assert_down(tile: TileHealth, case: &str) {
    assert!(
        matches!(tile, TileHealth::Down { .. }),
        "{case}: want Down, got {tile:?}"
    );
}

#[tokio::test]
async fn an_absent_or_refused_analyze_socket_reads_as_on_demand() {
    let (_dir, socket) = socket_dir();
    assert_eq!(
        analyze_tile(&socket, DEADLINE).await,
        TileHealth::OnDemand,
        "absent socket file"
    );

    // A socket file whose listener is gone: connect is refused.
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind stub");
    drop(listener);
    assert!(socket.exists(), "the stale socket file stays behind");
    assert_eq!(
        analyze_tile(&socket, DEADLINE).await,
        TileHealth::OnDemand,
        "refused connect"
    );
}

#[tokio::test]
async fn a_healthy_analyze_socket_reads_as_up() {
    let (_dir, socket) = socket_dir();
    serve(
        &socket,
        Peer::Reply(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"ok\",\"version\":\"0.9.0\"}}\n"),
    );
    assert_eq!(
        analyze_tile(&socket, DEADLINE).await,
        TileHealth::Up {
            version: Some("0.9.0".into())
        }
    );
}

#[tokio::test]
async fn a_broken_analyze_socket_reads_as_down_not_on_demand() {
    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::HangUp);
    assert_down(analyze_tile(&socket, DEADLINE).await, "peer hang-up");

    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::Reply(b"not json\n"));
    assert_down(analyze_tile(&socket, DEADLINE).await, "decode failure");

    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::Hold);
    let short = Duration::from_millis(200);
    assert_down(analyze_tile(&socket, short).await, "timeout");

    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::HangUp);
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o644))
        .expect("widen the socket");
    assert_down(
        analyze_tile(&socket, DEADLINE).await,
        "socket verification failure",
    );

    let (_dir, socket) = socket_dir();
    std::fs::write(&socket, b"").expect("a regular file at the socket path");
    assert_down(analyze_tile(&socket, DEADLINE).await, "not a socket");
}

// ─── #10901: the Events tile probes `events.health` ──────────────────────────

/// The method the Events tile sends, read from the roster.
fn events_method() -> &'static str {
    match ServiceId::Events.probe() {
        ProbeKind::UdsHealth(method) => method,
        other => panic!("Events must probe UdsHealth, got {other:?}"),
    }
}

/// The Events tile for one probe of `socket`, installed, the way
/// `probe_service` folds it: the outer deadline bounds the whole probe.
async fn events_tile(socket: &Path, inner: Duration, outer: Duration) -> TileHealth {
    let observed = async {
        on_demand_when_idle(
            ServiceId::Events,
            uds_health(socket, events_method(), inner).await,
        )
    };
    with_deadline(observed, true, outer).await
}

#[test]
fn events_maps_to_the_events_health_uds_probe() {
    assert_eq!(
        ServiceId::Events.probe(),
        ProbeKind::UdsHealth("events.health")
    );
    assert_eq!(
        events_method(),
        trusty_common::control_bus::METHOD_EVENTS_HEALTH
    );
    assert!(!ServiceId::Events.not_listening_is_on_demand());
}

#[tokio::test]
async fn an_events_socket_that_answers_health_reads_as_up() {
    let (_dir, socket) = socket_dir();
    serve(
        &socket,
        Peer::Reply(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"ok\",\"service\":\"trusty-events\",\"version\":\"0.1.0\",\"uptime_secs\":3,\"ingested\":0,\"deduped\":0,\"evicted\":0,\"log_dropped\":0,\"subscribers\":0,\"head_seq\":0}}\n",
        ),
    );
    assert_eq!(
        events_tile(&socket, DEADLINE, DEADLINE).await,
        TileHealth::Up {
            version: Some("0.1.0".into())
        }
    );
}

#[tokio::test]
async fn an_events_socket_that_does_not_answer_health_is_down_or_timeout() {
    let (_dir, socket) = socket_dir();
    assert_down(
        events_tile(&socket, DEADLINE, DEADLINE).await,
        "absent socket",
    );

    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind stub");
    drop(listener);
    assert_down(
        events_tile(&socket, DEADLINE, DEADLINE).await,
        "refused connect",
    );

    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::Hold);
    let short = Duration::from_millis(200);
    assert_down(events_tile(&socket, short, DEADLINE).await, "rpc timeout");
    assert_eq!(
        events_tile(&socket, DEADLINE, short).await,
        TileHealth::Timeout,
        "probe deadline"
    );

    // A connect-only peer (the pre-#10901 bus) no longer reads as Up.
    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::HangUp);
    assert_down(events_tile(&socket, DEADLINE, DEADLINE).await, "hang-up");

    let (_dir, socket) = socket_dir();
    serve(
        &socket,
        Peer::Reply(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"unknown method\"}}\n"),
    );
    assert!(
        matches!(
            events_tile(&socket, DEADLINE, DEADLINE).await,
            TileHealth::Degraded { .. }
        ),
        "an RPC error is an answer, not health"
    );
}
