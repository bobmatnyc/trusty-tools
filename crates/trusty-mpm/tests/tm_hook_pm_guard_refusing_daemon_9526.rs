//! A loopback daemon URL whose connect is refused at once on macOS and Linux
//! (#9526).
//!
//! Why: the guard classifies a refused connect as "no daemon" and a connect
//! timeout as "a daemon that did not answer" (`classify_transport_failure`),
//! so the no-daemon tests in `tm_hook_pm_guard` need a refusal, not a timeout.
//! Port 1 times out under WSL2 mirrored networking. A bound, unlistened port
//! (#9551) is refused on Linux, but macOS drops the SYN and the connect hangs.
//! What: [`RefusingDaemon`] and the test that pins its one property.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_refusing_daemon_9526::`.

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Ephemeral candidates tried before [`FALLBACK`].
const EPHEMERAL_CANDIDATES: usize = 4;

/// Bound on one candidate's probe. A loopback refusal is an RST from the local
/// kernel and returns at once; this only caps a candidate that drops the SYN.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Last candidate: nothing listens on port 1 on most hosts, but WSL2 mirrored
/// networking lets a connect to it hang, so it is tried last.
const FALLBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);

/// A loopback daemon URL whose connect is refused at once on every host.
///
/// Why: see the module doc; the three no-daemon tests rest on a refusal.
/// What: takes an ephemeral port from `bind(0)`, closes the socket, and keeps
/// the port only if a probe connect is refused. A port that is not refused is
/// replaced by a new candidate; [`FALLBACK`] comes last. No socket is held on
/// the chosen port, because a held, unlistened socket is what macOS drops SYNs
/// for. A later binder could take the closed port; the probe catches that at
/// construction, and the window after it is the guard's one short run.
/// Test: `refusing_daemon_url_is_refused_well_under_the_connect_timeout`,
/// `pm_guard_allows_a_builder_when_the_daemon_cannot_be_asked`,
/// `pm_guard_allows_a_non_builder_when_the_daemon_cannot_be_asked`,
/// `pm_guard_warns_when_no_daemon_answers_the_claim`.
pub(crate) struct RefusingDaemon {
    pub(crate) url: String,
}

impl RefusingDaemon {
    pub(crate) fn new() -> Self {
        let mut rejected = Vec::new();
        let candidates = (0..EPHEMERAL_CANDIDATES)
            .map(|_| released_ephemeral_port())
            .chain([FALLBACK]);
        for addr in candidates {
            match refused_at_once(addr) {
                Ok(()) => {
                    return Self {
                        url: format!("http://{addr}"),
                    };
                }
                Err(outcome) => rejected.push(format!("{addr}: {outcome}")),
            }
        }
        panic!("no loopback candidate refused a connect (#9526): {rejected:?}");
    }
}

/// An ephemeral loopback port with no socket left on it.
fn released_ephemeral_port() -> SocketAddr {
    // The bind stays in one statement: the no-listener gate
    // (scripts/check_no_tcp_listeners.sh) reads only that statement for the
    // ephemeral address.
    let socket = tokio::net::TcpSocket::new_v4()
        .and_then(|s| s.bind("127.0.0.1:0".parse().expect("addr")).map(|()| s))
        .expect("bind loopback socket");
    let addr = socket.local_addr().expect("bound addr");
    // #9526: close before any connect. macOS drops a SYN to a bound,
    // unlistened socket; a closed port is refused on macOS and Linux.
    drop(socket);
    addr
}

/// `Ok` when a connect to `addr` is refused within [`PROBE_TIMEOUT`]; else the
/// observed outcome, for the panic message.
fn refused_at_once(addr: SocketAddr) -> Result<(), String> {
    match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

/// The URL [`RefusingDaemon`] produces is refused in well under the 2 s
/// connect timeout the #9551 precondition used.
///
/// Why: #9551 held a bound, unlistened socket on the port. macOS dropped the
/// SYN, the 2 s connect timed out, and the three no-daemon tests failed before
/// the guard ran. Linux refuses that port too, so this test cannot go red on
/// Linux; the macOS run is its red-to-green proof.
/// What: connects to the produced address with a 2 s bound and asserts a
/// `ConnectionRefused` that arrives in under one second.
/// Test: this function.
#[test]
fn refusing_daemon_url_is_refused_well_under_the_connect_timeout() {
    let daemon = RefusingDaemon::new();
    let addr: SocketAddr = daemon
        .url
        .strip_prefix("http://")
        .expect("an http URL")
        .parse()
        .expect("a socket address");
    let started = Instant::now();
    let outcome = TcpStream::connect_timeout(&addr, Duration::from_secs(2));
    let elapsed = started.elapsed();
    assert!(
        matches!(&outcome, Err(e) if e.kind() == ErrorKind::ConnectionRefused),
        "{addr} must refuse, got: {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "{addr} refused only after {elapsed:?}; the guard needs a prompt refusal"
    );
}
