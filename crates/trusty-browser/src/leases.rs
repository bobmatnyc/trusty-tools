//! Tab leases and the request journal (`leases.py`, `Session.journal`).
//!
//! Why: one controller per tab. A lease lives [`TTL`] seconds past its last
//! use, an in-flight command keeps it alive, and a retried mutating op must
//! answer from the journal instead of running twice.
//! What: [`Inner::active`] is `Leases.active` (an expired, idle lease is
//! dropped on lookup); [`Inner::drop_claim`] is `Pages.drop`; [`Journal`]
//! maps a `request_id` to the request's journal copy and its reply, capped at
//! [`JOURNAL_LIMIT`]. [`indicator`] and [`clear`] send `lease_show` and
//! `lease_clear` with the route expectations of `Peer.command`. Code-session
//! leases carry `indicator = false`, so in 0.7.0, as in Python, [`clear`]
//! sends nothing for them; the workflow-session lease ops that set it are
//! unreachable since #11239 removed the ticket `open`.
//! Test: `an_idle_lease_expires_and_a_busy_one_does_not`,
//! `indicator_commands_carry_the_route_and_python_reply_rules` below; the
//! lease paths of `page_ops_replay_byte_identically` in `tests/parity_pages.rs`.

use std::collections::HashMap;

use crate::broker::{Claim, Core, Inner};
use crate::peer_conn::Peer;
use crate::pyjson::Json;
use crate::refusal::Refusal;

/// Lease lifetime in seconds (`leases.TTL`).
pub const TTL: f64 = 60.0;
/// Journal entries a session may hold before `SessionFull`.
pub const JOURNAL_LIMIT: usize = 4096;

/// `request_id` to (journal copy of the request, reply).
pub type Journal = HashMap<String, (Json, Json)>;

impl Inner {
    /// The live lease on `(generation, tab)`; an expired idle one is dropped first.
    pub fn active(&mut self, generation: &str, tab: i64, now: f64) -> Option<&Claim> {
        self.claims.retain(|c| {
            !(c.generation == generation && c.tab == tab && !c.busy && c.deadline <= now)
        });
        self.claims
            .iter()
            .find(|c| c.generation == generation && c.tab == tab)
    }

    /// Drop the lease with `token` (`Pages.drop`).
    pub fn drop_claim(&mut self, token: &str) {
        self.claims.retain(|c| c.token != token);
    }

    /// Apply `f` to the lease with `token`, if it is still held.
    pub fn with_claim(&mut self, token: &str, f: impl FnOnce(&mut Claim)) {
        if let Some(c) = self.claims.iter_mut().find(|c| c.token == token) {
            f(c);
        }
    }
}

/// `lease_show` / `lease_clear` for a lease (`Leases.indicator`).
pub async fn indicator(
    core: &Core,
    peer: &Peer,
    claim: &Claim,
    show: bool,
) -> Result<Json, Refusal> {
    let op = if show { "lease_show" } else { "lease_clear" };
    let workflow = peer.route.workflow.clone().map_or(Json::Null, Json::from);
    let args = vec![
        ("tab", Json::Int(claim.tab)),
        ("lease", Json::from(claim.token.as_str())),
        ("workflow", workflow),
        ("ttl_ms", Json::Int((TTL * 1000.0) as i64)),
    ];
    peer.command(&*core.entropy, op, args).await
}

/// Clear a lease's indicator when it shows one; a refusal is ignored (`Leases.clear`).
pub async fn clear(core: &Core, peer: Option<&Peer>, claim: &Claim) {
    if let (true, Some(peer)) = (claim.indicator, peer) {
        let _ = indicator(core, peer, claim, false).await;
    }
}

#[cfg(test)]
#[path = "leases_tests.rs"]
mod tests;
