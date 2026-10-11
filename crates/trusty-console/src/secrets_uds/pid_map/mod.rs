//! The pid behind a loopback TCP client, read when the connection is accepted
//! (#9066, DOC-74 §15.6, A1 and A9).
//!
//! Why: the bridge asserts whether its HTTP client descends from an agent.
//! Only the console can see which process holds the other end of a loopback
//! connection, and a pid read later could already name another process.
//! What: [`OsPidMap`], the per-OS [`PidResolver`]: Linux finds the client's
//! socket inode in `/proc/net/tcp{,6}` and then the process holding it under
//! `/proc/*/fd`; macOS lists this user's pids and reads each socket
//! descriptor's `socket_fdinfo`. Any other target finds nothing. The
//! [`Connected`] impl makes [`PeerLookup`] axum's connect info, so the
//! loopback listener starts resolving the pid and its start time once per
//! connection, at accept, on tokio's blocking pool; accepting does no lookup.
//! A secrets request waits for it, so its cost is measured by
//! `accept_time_lookup_stays_bounded`.
//! Test: `pid_map/tests.rs` (T23); `mount_tests.rs` drives the real accept.

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
pub(crate) mod tests;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use tokio::net::TcpListener;
use trusty_secrets::ancestry::OsProcessTable;

use super::verdict::{AcceptedPeer, PeerLookup, PidResolver};

/// The host's own socket table.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsPidMap;

impl PidResolver for OsPidMap {
    /// Test: `a_spawned_client_resolves_to_its_pid`,
    /// `a_closed_peer_resolves_to_none`.
    fn pid_of(&self, local: SocketAddr, peer: SocketAddr) -> Option<u32> {
        #[cfg(test)]
        lookups::record(local);
        #[cfg(test)]
        lookups::stall(local);
        os_pid_of(local, peer)
    }
}

#[cfg(target_os = "linux")]
fn os_pid_of(local: SocketAddr, peer: SocketAddr) -> Option<u32> {
    linux::pid_of(local, peer)
}

#[cfg(target_os = "macos")]
fn os_pid_of(local: SocketAddr, peer: SocketAddr) -> Option<u32> {
    macos::pid_of(local, peer)
}

// #9066 A1: no socket table to read, so every client is `unmappable`.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn os_pid_of(_local: SocketAddr, _peer: SocketAddr) -> Option<u32> {
    None
}

// #9066: only the Linux and macOS lookups compare endpoints.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", test)),
    allow(dead_code)
)]
/// Whether `a` and `b` name the same endpoint, an IPv4-mapped IPv6 address
/// equal to its IPv4 form.
fn same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && a.ip().to_canonical() == b.ip().to_canonical()
}

impl Connected<IncomingStream<'_, TcpListener>> for PeerLookup {
    /// Start resolving the client when axum accepts the connection (A9).
    ///
    /// What: constant-time work only; the lookup runs on the blocking pool
    /// (#9066 F1). An unreadable local address gives [`AcceptedPeer::Unfound`].
    /// Test: `a_loopback_request_is_judged_from_the_pid_found_at_accept`,
    /// `a_slow_lookup_delays_no_accept_and_no_other_route`.
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        let peer = *stream.remote_addr();
        match stream.io().local_addr() {
            Ok(local) => Self::start(OsPidMap, Arc::new(OsProcessTable), local, peer),
            Err(_) => Self::ready(AcceptedPeer::Unfound),
        }
    }
}

/// Every local address [`OsPidMap`] was asked about, so a test can prove a
/// listener never looked a client up (A2); and per-listener stalls, so a test
/// can make the real lookup slow (#9066 F1).
#[cfg(test)]
pub(crate) mod lookups {
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use std::time::Duration;

    static SEEN: Mutex<Vec<SocketAddr>> = Mutex::new(Vec::new());
    static SLOW: Mutex<Vec<(SocketAddr, Duration)>> = Mutex::new(Vec::new());

    /// Make every later lookup on the listener `local` sleep `by` first.
    pub(crate) fn slow_down(local: SocketAddr, by: Duration) {
        if let Ok(mut slow) = SLOW.lock() {
            slow.push((local, by));
        }
    }

    pub(super) fn stall(local: SocketAddr) {
        let by = SLOW.lock().ok().and_then(|slow| {
            slow.iter()
                .find(|(addr, _)| *addr == local)
                .map(|(_, by)| *by)
        });
        if let Some(by) = by {
            std::thread::sleep(by);
        }
    }

    pub(super) fn record(local: SocketAddr) {
        if let Ok(mut seen) = SEEN.lock() {
            seen.push(local);
        }
    }

    /// How many lookups named `local` as the listener.
    pub(crate) fn count(local: SocketAddr) -> usize {
        SEEN.lock()
            .map(|seen| seen.iter().filter(|addr| **addr == local).count())
            .unwrap_or(0)
    }
}
