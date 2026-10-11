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
//! [`PeerLookup`] runs the accept-time lookup on tokio's blocking pool, and
//! [`judge_in_time`] waits for it and the judgement under [`VERDICT_BOUND`].
//!
//! Test: `verdict_without_a_pid_is_unmappable_and_not_an_agent`,
//! `verdict_with_an_unreadable_chain_is_unreadable`,
//! `verdict_for_a_pid_reused_after_accept_is_unreadable`,
//! `verdict_reads_the_agent_ancestor`.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::{self, BoxFuture, Shared};
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

/// How long a secrets request waits for its connection's lookup and the
/// ancestry walk, together.
///
/// Why: the lookup measured a median of about 10 ms on EVO and 2.7 ms on the
/// Mac (`accept_time_lookup_stays_bounded`). Two seconds leaves two orders of
/// magnitude for a loaded host or a queued blocking pool, yet a stalled
/// `/proc` or libproc read cannot hold a request open without limit. Expiry
/// is fail-closed: the request is forwarded as `unreadable`.
pub const VERDICT_BOUND: Duration = Duration::from_secs(2);

/// The client lookup for one accepted connection, axum's connect info on the
/// loopback listener (#9066 F1).
///
/// Why: the lookup reads `/proc` or libproc, which costs milliseconds. Run in
/// axum's accept loop, it serialized every connection to the console.
/// What: [`PeerLookup::start`] hands [`AcceptedPeer::resolve`] to tokio's
/// blocking pool at accept (A9) and returns at once; every clone shares the
/// one result. A lookup that panicked, was cancelled or found no runtime
/// yields `None`. Only a secrets request awaits it.
/// Test: `a_slow_lookup_delays_no_accept_and_no_other_route`,
/// `a_lookup_slower_than_the_bound_is_unreadable`.
#[derive(Clone)]
pub struct PeerLookup(Shared<BoxFuture<'static, Option<AcceptedPeer>>>);

impl PeerLookup {
    /// Start resolving the connection `peer` -> `local` now, off the async
    /// threads.
    pub fn start(
        resolver: impl PidResolver + 'static,
        table: Arc<dyn ProcessTable>,
        local: SocketAddr,
        peer: SocketAddr,
    ) -> Self {
        // #9066 F1: never resolve inline; no runtime is a failed lookup.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Self(future::ready(None).boxed().shared());
        };
        let task = runtime
            .spawn_blocking(move || AcceptedPeer::resolve(&resolver, table.as_ref(), local, peer));
        Self(task.map(Result::ok).boxed().shared())
    }

    /// A lookup that already found `peer`.
    pub fn ready(peer: AcceptedPeer) -> Self {
        Self(future::ready(Some(peer)).boxed().shared())
    }
}

impl fmt::Debug for PeerLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PeerLookup").field(&self.0.peek()).finish()
    }
}

/// The assertion for a request on the connection `lookup` describes.
///
/// Why: a secrets request must not wait on the lookup without limit, and an
/// absent lookup is a wiring fault, not Dd7 B's "no pid found" (#9066 F2).
/// What: awaits the lookup, then runs [`judge`] on the blocking pool, both
/// under [`VERDICT_BOUND`]. No lookup, a failed one, a panicked judgement
/// or the bound expiring each give `unreadable`, never `unmappable`.
/// Test: `a_lookup_slower_than_the_bound_is_unreadable`,
/// `a_request_without_a_lookup_is_unreadable`,
/// `verdicts_reach_the_wire_and_are_forwarded`.
pub async fn judge_in_time(
    table: Arc<dyn ProcessTable>,
    lookup: Option<PeerLookup>,
) -> BridgeAssertion {
    let Some(lookup) = lookup else {
        return BridgeAssertion::unreadable();
    };
    let judged = async move {
        let peer = lookup.0.await?;
        tokio::task::spawn_blocking(move || judge(table.as_ref(), peer))
            .await
            .ok()
    };
    match tokio::time::timeout(VERDICT_BOUND, judged).await {
        Ok(Some(assertion)) => assertion,
        _ => BridgeAssertion::unreadable(),
    }
}
