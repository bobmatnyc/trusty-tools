//! #10476: the analyze health probe over real sockets — "nothing listening"
//! is on demand, every other failure is Down.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

use super::probe::{classify, on_demand_when_idle, uds_health};
use super::services::ServiceId;
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
