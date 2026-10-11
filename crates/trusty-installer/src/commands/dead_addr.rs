//! A TCP address whose connect fails inside `connect(2)` on Linux and macOS
//! (#9613).
//!
//! Why: the confirmed-down tests need a connect that fails at once. A closed
//! loopback port does not guarantee that: under WSL2 mirrored networking a
//! connect to `127.0.0.1:1` hangs for seconds (#9613), and macOS drops a SYN to
//! a bound, unlistened socket (#9526). Both outcomes depend on how the host
//! treats a SYN to a closed port.
//! What: [`dead_addr`] names an IPv4 multicast address. The kernel refuses a
//! TCP connect to a multicast destination before it sends a packet: Linux
//! `tcp_v4_connect` returns `ENETUNREACH`, and the BSD-derived
//! `tcp_usr_connect` in XNU returns `EAFNOSUPPORT`. No listener table, loopback
//! relay or packet filter takes part. reqwest reports the failure as a connect
//! error (`is_connect()`, not `is_timeout()`), which is the error path the tests
//! assert.
//!
//! Reuse (#9695): this file needs only `std` and `tokio`. trusty-mpm's
//! `pm_guard_dispatch` no-daemon tests have the same cause and classify a
//! connect error the same way (`is_connect() && !is_timeout()`), so they can
//! copy this file unchanged.
//! Test: `dead_addr_fails_inside_connect_before_any_packet_is_sent`, plus every
//! test that calls [`dead_addr`].

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

/// The address [`dead_addr`] hands out.
///
/// `233.252.0.0/24` is MCAST-TEST-NET (RFC 5771 §9.2, RFC 6676): a multicast
/// block reserved for documentation, so no real group uses it. The port does
/// not matter; no packet is sent.
const DEAD: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(233, 252, 0, 1), 1));

/// Upper bound for the self-check in [`dead_addr`]. The kernel fails the
/// connect in microseconds; the bound only stops a broken host from hanging
/// the suite.
const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

/// A `host:port` whose TCP connect fails at once, on every host (#9613).
///
/// Why: see the module doc. The address must not depend on whether the host
/// refuses, drops or relays a SYN to a closed loopback port.
/// What: returns [`DEAD`] as `host:port` after one blocking connect to it has
/// failed without timing out. A host that accepts or hangs on the connect
/// fails here with a message that names the address. Sync (`std::net`), so it
/// works from `#[test]` and `#[tokio::test]`.
///
/// # Postconditions
/// The returned address failed a connect within [`SELF_CHECK_TIMEOUT`], and
/// the failure was not a timeout.
/// Test: `dead_addr_fails_inside_connect_before_any_packet_is_sent`,
/// `probe_http::tests::probe_distinguishes_failure_causes`,
/// `verify_tail::tests::verify_one_kickstarts_a_genuinely_down_launchd_daemon`.
pub(crate) fn dead_addr() -> String {
    match std::net::TcpStream::connect_timeout(&DEAD, SELF_CHECK_TIMEOUT) {
        Err(e) if e.kind() != std::io::ErrorKind::TimedOut => DEAD.to_string(),
        other => panic!(
            "dead_addr expected the kernel to fail a connect to multicast {DEAD} at once \
             (#9613), got {other:?}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future as _;
    use std::task::{Context, Poll, Waker};

    /// REGRESSION (#9613): the dead address fails in the connect call itself.
    ///
    /// Why: a connect that is still in flight after `connect(2)` returns
    /// depends on the host's answer to a SYN. `127.0.0.1:1` is in flight on
    /// Linux and macOS (`EINPROGRESS`), and WSL2 mirrored networking can then
    /// hang it for seconds. An address the kernel rejects outright has no
    /// in-flight state for any host to delay.
    /// What: starts a non-blocking connect to [`DEAD`] and polls it once with a
    /// no-op waker. The first poll issues `connect(2)`; it must already be
    /// `Ready(Err)` with a non-timeout error. `Pending` means a SYN was sent.
    /// Test: this function.
    #[tokio::test]
    async fn dead_addr_fails_inside_connect_before_any_packet_is_sent() {
        // #9613: `TcpStream::connect`, not `TcpSocket`, so the ADR-0032 guard
        // sees a client connect. A `SocketAddr` resolves without awaiting, so the
        // first poll still reaches `connect(2)`.
        let mut connect = std::pin::pin!(tokio::net::TcpStream::connect(DEAD));
        let mut cx = Context::from_waker(Waker::noop());
        match connect.as_mut().poll(&mut cx) {
            Poll::Ready(Err(e)) => assert_ne!(
                e.kind(),
                std::io::ErrorKind::TimedOut,
                "{DEAD} failed with a timeout, not in connect(2): {e:?}"
            ),
            Poll::Ready(Ok(_)) => panic!("{DEAD} accepted a connection"),
            Poll::Pending => panic!(
                "{DEAD} must fail inside connect(2), before a SYN leaves the host; the \
                 connect is in flight, so its outcome depends on how the host answers \
                 a SYN to a closed port (#9613)"
            ),
        }
        assert_eq!(
            dead_addr(),
            DEAD.to_string(),
            "dead_addr must hand out the address this test checks"
        );
    }
}
