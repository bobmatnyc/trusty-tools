//! The request-timeout ladder every layer of the diagnostics path shares.
//!
//! Why: four timeouts sit between a client and a `cargo clippy` subprocess —
//! the dispatch's cooperative deadline, the handler's grace, the router's
//! blanket layer, and the MCP client's own request timeout. #6018 fixed the
//! innermost one and left the outer three as independent hardcoded constants,
//! which reintroduced the original bug from two directions: the MCP client's
//! 150 s cut below the 180 s deadline, so an MCP caller still got a body-less
//! transport timeout; and raising `TRUSTY_DIAGNOSTICS_DEADLINE_SECS` above
//! 270 s pushed the handler past the router's fixed 300 s, so the router's
//! empty-bodied 504 beat the handler's structured JSON — on the exact
//! remediation path the handler's own error message recommends.
//!
//! What: one module owning the whole ladder. Every rung is derived from the
//! innermost deadline, so the ordering invariant holds at every configurable
//! value rather than only at the default:
//!
//! ```text
//! diagnostics_deadline()          cooperative — dispatch stops starting work
//!   + DIAGNOSTICS_HARD_GRACE  ->  handler answers 504 with a JSON body
//!   + ROUTER_MARGIN           ->  router TimeoutLayer, empty-bodied 504
//!   + CLIENT_MARGIN           ->  MCP reqwest client gives up
//! ```
//!
//! Lives in `core` rather than `service` because `service` is behind the
//! `http-server` feature while `mcp` is not, and both need these values.
//!
//! Test: `ladder_is_strictly_increasing_across_the_configurable_range` pins
//! the ordering over deadlines from 1 s to 1 h; `mcp_timeout_never_drops_below_
//! the_deep_analysis_floor` pins the one rung that is not purely derived.

use std::time::Duration;

/// Wall-clock budget for one diagnostics request when the operator sets none.
///
/// Why: long enough for one project-scoped build (`cargo clippy` over a warm
/// workspace, a small `dotnet build`), short enough that an HTTP client with a
/// default read timeout still receives a response body. Before #6018 there was
/// no budget: a 4097-file Rust index spawned one `cargo clippy` per file at
/// ~0.155 s each and ran past 10 minutes, so every client gave up at the
/// transport layer and got zero bytes. A starting point, not a measured
/// optimum — `TRUSTY_DIAGNOSTICS_DEADLINE_SECS` overrides it.
const DEFAULT_DIAGNOSTICS_DEADLINE_SECS: u64 = 180;

/// Extra time the handler waits past the cooperative deadline before it stops
/// waiting on the blocking task.
///
/// Why: the dispatch checks its deadline BETWEEN subprocess spawns, and a
/// project-scoped tool caps each spawn at the remaining budget, so an in-flight
/// tool can overrun the deadline only by its own teardown. This window lets the
/// dispatch return a partial report — the informative answer — instead of the
/// handler racing it to a bare 504.
pub const DIAGNOSTICS_HARD_GRACE: Duration = Duration::from_secs(30);

/// Headroom between the handler's own budget and the router's blanket layer.
///
/// Why: the router layer is a net under a handler bug, never the mechanism. It
/// must lose every race against a handler that is working correctly, because
/// its 504 carries an empty body while the handler's names the files and tools
/// that were cut off.
const ROUTER_MARGIN: Duration = Duration::from_secs(30);

/// Headroom between the router layer and the MCP client's request timeout.
///
/// Why: an MCP caller must receive whatever bytes the daemon produced. A client
/// timeout at or below the router's leaves the MCP path with the pre-#6018
/// failure — a transport error with no body — even though the daemon answered.
const CLIENT_MARGIN: Duration = Duration::from_secs(30);

/// Floor under the MCP client timeout, independent of the diagnostics ladder.
///
/// Why: `deep_analysis` calls OpenRouter with up to 120 s allowed
/// (`OPENROUTER_REQUEST_TIMEOUT_SECS` in `trusty-common/src/chat.rs`) plus
/// synthesis headroom. That path predates #6018 and does not shrink when an
/// operator lowers the diagnostics deadline, so the client timeout must not
/// follow the ladder below this value.
const DEEP_ANALYSIS_FLOOR: Duration = Duration::from_secs(150);

/// Wall-clock budget for one diagnostics request.
///
/// Why: mirrors `core::tool_impls::build_tool_timeout` — a named default an
/// operator can raise on slow hardware without a rebuild.
/// What: reads `TRUSTY_DIAGNOSTICS_DEADLINE_SECS`, falling back to
/// [`DEFAULT_DIAGNOSTICS_DEADLINE_SECS`] on a missing, unparseable, or zero
/// value (zero would time out every request instantly).
/// Test: `diagnostics_deadline_default_is_180s`.
pub fn diagnostics_deadline() -> Duration {
    let secs = std::env::var("TRUSTY_DIAGNOSTICS_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_DIAGNOSTICS_DEADLINE_SECS);
    Duration::from_secs(secs)
}

/// How long the handler waits on the blocking dispatch before answering 504.
///
/// Taking `deadline` as a parameter is what makes the ladder testable: the
/// invariant can be checked across the whole configurable range without
/// mutating process-wide environment state.
pub fn handler_budget_for(deadline: Duration) -> Duration {
    deadline + DIAGNOSTICS_HARD_GRACE
}

/// How long the router's blanket `TimeoutLayer` allows a request to run.
pub fn router_timeout_for(deadline: Duration) -> Duration {
    handler_budget_for(deadline) + ROUTER_MARGIN
}

/// How long the MCP HTTP client waits for the daemon, never below the
/// `deep_analysis` floor.
pub fn mcp_client_timeout_for(deadline: Duration) -> Duration {
    let ladder = router_timeout_for(deadline) + CLIENT_MARGIN;
    // `max` keeps the ordering invariant intact: when the floor wins, the
    // ladder value it replaces was router + CLIENT_MARGIN, so the floor is
    // still strictly above the router rung.
    ladder.max(DEEP_ANALYSIS_FLOOR)
}

/// [`handler_budget_for`] at the configured deadline.
pub fn diagnostics_handler_budget() -> Duration {
    handler_budget_for(diagnostics_deadline())
}

/// [`router_timeout_for`] at the configured deadline.
pub fn router_request_timeout() -> Duration {
    router_timeout_for(diagnostics_deadline())
}

/// [`mcp_client_timeout_for`] at the configured deadline.
pub fn mcp_client_timeout() -> Duration {
    mcp_client_timeout_for(diagnostics_deadline())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the ladder existed as three independent hardcoded constants, and
    /// two of the three broke the ordering somewhere in the configurable range
    /// — the MCP client below the deadline at every value, the router below
    /// the handler for any deadline above 270 s. Pinning the ordering at the
    /// default only would have caught neither.
    /// What: walks deadlines from 1 s to 1 h and asserts each rung is strictly
    /// above the one inside it.
    /// Test: this test. Fails against hardcoded rungs at deadline >= 271 s
    /// (router) and at every deadline (MCP client, 150 s flat).
    #[test]
    fn ladder_is_strictly_increasing_across_the_configurable_range() {
        for secs in [1u64, 10, 60, 150, 180, 240, 270, 271, 300, 600, 1800, 3600] {
            let deadline = Duration::from_secs(secs);
            let handler = handler_budget_for(deadline);
            let router = router_timeout_for(deadline);
            let client = mcp_client_timeout_for(deadline);

            assert!(
                handler > deadline,
                "deadline={secs}s: handler budget {handler:?} must exceed the \
                 cooperative deadline {deadline:?}"
            );
            assert!(
                router > handler,
                "deadline={secs}s: router timeout {router:?} must exceed the \
                 handler budget {handler:?}, or the router's empty-bodied 504 \
                 beats the handler's structured JSON"
            );
            assert!(
                client > router,
                "deadline={secs}s: MCP client timeout {client:?} must exceed the \
                 router timeout {router:?}, or an MCP caller gets a transport \
                 error instead of the daemon's response"
            );
        }
    }

    /// Why: lowering the diagnostics deadline must not shorten the unrelated
    /// `deep_analysis` path below the 120 s OpenRouter ceiling it has always
    /// needed.
    /// Test: this test.
    #[test]
    fn mcp_timeout_never_drops_below_the_deep_analysis_floor() {
        for secs in [1u64, 5, 30, 60] {
            let client = mcp_client_timeout_for(Duration::from_secs(secs));
            assert!(
                client >= DEEP_ANALYSIS_FLOOR,
                "deadline={secs}s: MCP client timeout {client:?} fell below the \
                 deep_analysis floor {DEEP_ANALYSIS_FLOOR:?}"
            );
            assert!(
                client.as_secs() > 120,
                "deadline={secs}s: MCP client timeout {client:?} must stay above \
                 the 120 s OpenRouter ceiling"
            );
        }
    }

    #[test]
    fn diagnostics_deadline_default_is_180s() {
        let d = diagnostics_deadline();
        if std::env::var("TRUSTY_DIAGNOSTICS_DEADLINE_SECS").is_ok() {
            assert!(d.as_secs() > 0, "an overridden deadline must be non-zero");
        } else {
            assert_eq!(d.as_secs(), 180, "default diagnostics deadline changed");
        }
    }
}
