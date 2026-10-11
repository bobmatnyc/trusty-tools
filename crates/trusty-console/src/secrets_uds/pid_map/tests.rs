//! T23: the OS socket table maps a real client to its pid (#9066).
//!
//! The client is a child process: this test binary re-run with only
//! [`client_child`] selected, so no external tool is needed. The child
//! connects, optionally writes a request, and reads until the socket closes.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};

use super::OsPidMap;
use crate::secrets_uds::verdict::PidResolver;

const CHILD_ADDR: &str = "TRUSTY_CONSOLE_PID_MAP_CHILD_ADDR";
const CHILD_REQUEST: &str = "TRUSTY_CONSOLE_PID_MAP_CHILD_REQUEST";

/// The client child's body; a no-op in an ordinary test run.
#[test]
fn client_child() {
    let Ok(addr) = std::env::var(CHILD_ADDR) else {
        return;
    };
    let mut stream = TcpStream::connect(addr).expect("connect");
    if let Ok(request) = std::env::var(CHILD_REQUEST) {
        stream.write_all(request.as_bytes()).expect("write");
    }
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
}

/// Spawn a child process that connects to `addr` and sends `request`.
pub(crate) fn spawn_client(addr: SocketAddr, request: Option<&str>) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("test exe"));
    command
        .args([
            "--exact",
            "secrets_uds::pid_map::tests::client_child",
            "--test-threads=1",
            "-q",
        ])
        .env(CHILD_ADDR, addr.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(request) = request {
        command.env(CHILD_REQUEST, request);
    }
    command.spawn().expect("spawn client child")
}

/// Accept one connection from a fresh child; the child is still connected.
fn connected_child() -> (TcpListener, TcpStream, SocketAddr, SocketAddr, Child) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let child = spawn_client(local, None);
    let (stream, peer) = listener.accept().expect("accept");
    (listener, stream, local, peer, child)
}

#[test]
fn a_spawned_client_resolves_to_its_pid() {
    let (_listener, _stream, local, peer, mut child) = connected_child();
    let found = OsPidMap.pid_of(local, peer);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(found, Some(child.id()), "the child holds {peer}");
}

#[test]
fn a_closed_peer_resolves_to_none() {
    let (_listener, stream, local, peer, mut child) = connected_child();
    assert_eq!(OsPidMap.pid_of(local, peer), Some(child.id()), "control");
    child.kill().expect("kill");
    child.wait().expect("wait");
    drop(stream);
    assert_eq!(
        OsPidMap.pid_of(local, peer),
        None,
        "nobody holds {peer} now"
    );
    let nobody: SocketAddr = "127.0.0.1:1".parse().expect("addr");
    assert_eq!(OsPidMap.pid_of(local, nobody), None);
}

#[cfg(target_endian = "little")]
#[test]
fn proc_net_rows_parse_both_families() {
    use super::linux::{Row, parse_row};
    let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";
    assert_eq!(parse_row(header), None);
    let v4 = "   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 123456 1 0000000000000000 20 4 30 10 -1";
    assert_eq!(
        parse_row(v4),
        Some(Row {
            local: "127.0.0.1:8080".parse().expect("addr"),
            remote: "127.0.0.1:54321".parse().expect("addr"),
            uid: 1000,
            inode: 123_456,
        })
    );
    let v6 = "   0: 00000000000000000000000001000000:1F90 0000000000000000FFFF00000100007F:D431 01 00000000:00000000 00:00000000 00000000  501        0 77 1";
    let row = parse_row(v6).expect("v6 row");
    assert_eq!(row.local, "[::1]:8080".parse::<SocketAddr>().expect("addr"));
    assert_eq!(
        row.remote,
        "[::ffff:127.0.0.1]:54321"
            .parse::<SocketAddr>()
            .expect("addr")
    );
    assert!(super::same_endpoint(
        row.remote,
        "127.0.0.1:54321".parse().expect("addr")
    ));
    assert_eq!((row.uid, row.inode), (501, 77));
}

/// The accept loop runs this lookup once per loopback connection, so its
/// cost bounds how fast the console accepts. Prints the measurement
/// (`--nocapture`); fails only on a pathological stall.
#[test]
fn accept_time_lookup_stays_bounded() {
    use std::time::{Duration, Instant};
    use trusty_secrets::ancestry::OsProcessTable;

    use crate::secrets_uds::verdict::AcceptedPeer;
    let (_listener, _stream, local, peer, mut child) = connected_child();
    let mut took: Vec<Duration> = (0..20)
        .map(|_| {
            let start = Instant::now();
            let found = AcceptedPeer::resolve(&OsPidMap, &OsProcessTable, local, peer);
            assert!(matches!(found, AcceptedPeer::Found { start: Some(_), .. }));
            start.elapsed()
        })
        .collect();
    let _ = child.kill();
    let _ = child.wait();
    took.sort();
    eprintln!(
        "accept-time lookup over 20 runs: min {:?}, median {:?}, max {:?}",
        took[0], took[10], took[19]
    );
    assert!(took[10] < Duration::from_secs(1), "median {:?}", took[10]);
}
