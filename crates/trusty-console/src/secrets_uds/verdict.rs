//! The console's verdict on the HTTP client it forwards a call for (#9066,
//! DOC-74 §15.6, owner ruling Dd7 B).
//!
//! Why: the secrets socket sees the console, never the browser or the
//! `curl` behind it. Only the console can map its TCP client to a pid, so it
//! judges that pid's ancestry with the walk the socket uses and asserts the
//! result.
//! What: [`AcceptedPeer`], what was learned when the connection was
//! accepted (A9): no pid, or a pid and its start time if that was readable.
//! [`PidResolver`] finds the pid behind a loopback TCP peer; the per-OS
//! implementation is `pid_map`. [`judge`] turns an accepted peer into a
//! [`BridgeAssertion`] against the process table *at request time*:
//! - no pid: `unmappable` (a non-agent, Dd7 B);
//! - a pid whose start time was unreadable at accept, differs now, or whose
//!   chain cannot be read: `unreadable` (`agent_parent: true`), never
//!   `unmappable`;
//! - otherwise `mapped` with `has_agent_ancestor`'s answer.
//!
//! Test: `verdict_without_a_pid_is_unmappable_and_not_an_agent`,
//! `verdict_with_an_unreadable_chain_is_unreadable`,
//! `verdict_for_a_pid_reused_after_accept_is_unreadable`,
//! `verdict_reads_the_agent_ancestor`.

use std::net::SocketAddr;

use trusty_secrets::ancestry::{ProcessTable, StartTime, has_agent_ancestor};
use trusty_secrets::api::BridgeAssertion;

/// Finds the pid that owns the client end of a loopback TCP connection.
///
/// Why: behind a trait so the accept-time lookup is testable without a real
/// socket table.
pub trait PidResolver: Send + Sync {
    /// The pid whose socket is `peer` connected to `local`; `None` when no
    /// process can be found (another user's, gone, or not readable).
    fn pid_of(&self, local: SocketAddr, peer: SocketAddr) -> Option<u32>;
}

/// What the console learned about a connection's client when it accepted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptedPeer {
    /// No pid could be found for the client.
    Unfound,
    /// The client's pid, and its start time when that was readable.
    Found {
        /// The client process.
        pid: u32,
        /// Its start time at accept; `None` when unreadable.
        start: Option<StartTime>,
    },
}

impl AcceptedPeer {
    /// Resolve the client of the connection `peer` -> `local`, now.
    ///
    /// What: the pid from `resolver`, then its start time from `table`, so a
    /// later request can tell the same process from a reused pid.
    /// Test: `accepted_peer_records_the_start_time_at_accept`.
    pub fn resolve(
        resolver: &dyn PidResolver,
        table: &dyn ProcessTable,
        local: SocketAddr,
        peer: SocketAddr,
    ) -> Self {
        match resolver.pid_of(local, peer) {
            None => Self::Unfound,
            Some(pid) => Self::Found {
                pid,
                start: table.start_time(pid).ok(),
            },
        }
    }
}

/// The assertion for `peer`, judged against `table` now.
///
/// What: see the module docs. Never answers `{mapped, false}` for a pid
/// whose chain or start time could not be read.
/// Test: the `verdict_*` tests in `tests.rs`.
pub fn judge(table: &dyn ProcessTable, peer: AcceptedPeer) -> BridgeAssertion {
    let AcceptedPeer::Found { pid, start } = peer else {
        return BridgeAssertion::unmappable();
    };
    // #9066 A9: a pid found at accept is never downgraded to `unmappable`.
    let Some(start) = start else {
        return BridgeAssertion::unreadable();
    };
    if table.start_time(pid).ok() != Some(start) {
        return BridgeAssertion::unreadable();
    }
    match has_agent_ancestor(table, pid) {
        Ok(agent_parent) => BridgeAssertion::mapped(agent_parent),
        Err(_) => BridgeAssertion::unreadable(),
    }
}
