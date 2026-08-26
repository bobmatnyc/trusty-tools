//! The two methods that describe the daemon rather than a corpus.
//!
//! Why (#6287): `health` and `list_indexes` used to live in `service/routes.rs`
//! beside `build_router` and `serve`, because that file owned "the HTTP surface"
//! as a whole. ADR-0032 deleted the router and the bind it sat next to, and
//! these two handlers have nothing to do with the analysis domains the sibling
//! modules own — so they get their own file rather than being wedged into one.
//!
//! What: [`health`] probes trusty-search reachability and reports this daemon's
//! version; [`list_indexes`] proxies the search daemon's index list.
//!
//! Test: `rpc_health_answers_over_a_real_socket`,
//! `rpc_health_reports_degraded_when_search_is_unreachable`,
//! `rpc_list_indexes_reports_an_unreachable_search_daemon`.

use serde::{Deserialize, Serialize};

use crate::core::IndexSummary;
use crate::service::events::{AnalyzerAppState, ApiError};

/// What `analyze.health` answers with.
///
/// Why: `trusty-console`'s `AnalyzeConnector` and `tctl`'s health probe both
/// read `version` off this, in two crates with no Cargo edge on this one.
/// Renaming a field here breaks both silently.
/// What: `status` is `"ok"` or `"degraded"`; `search_reachable` is the fact
/// behind that verdict.
/// Test: `rpc_health_reports_degraded_when_search_is_unreachable`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub search_reachable: bool,
}

/// Why: reflects the hard runtime dependency on trusty-search — there is no
/// meaningful "ok" state when the search daemon is unreachable.
///
/// What: probes trusty-search's health, then answers `ok` or `degraded`.
///
/// #6287: degraded is a RESULT frame, not an error frame. The HTTP shape
/// answered 503 here, and an error frame is that 503's literal translation —
/// but it would take `version` and `search_reachable` away from exactly the
/// caller who needs them, since a JSON-RPC error carries a message and a code
/// and no typed body. A degraded daemon answering "I am up, my dependency is
/// not" is the whole point of the probe; `tctl` and the console distinguish the
/// two states by reading `status`, not by whether the call failed.
///
/// Test: `rpc_health_reports_degraded_when_search_is_unreachable`.
pub async fn health(state: &AnalyzerAppState) -> HealthResponse {
    let search_reachable = state.search.health().await.unwrap_or(false);
    HealthResponse {
        status: if search_reachable { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        search_reachable,
    }
}

/// Why: consumers browse the analyzable corpora through this daemon rather than
/// having to know trusty-search's own address.
/// What: proxies `TrustySearchClient::list_indexes`; an unreachable search
/// daemon is an upstream failure, never an empty list.
/// Test: `rpc_list_indexes_reports_an_unreachable_search_daemon`.
pub async fn list_indexes(state: &AnalyzerAppState) -> Result<Vec<IndexSummary>, ApiError> {
    state.search.list_indexes().await.map_err(|e| {
        tracing::warn!("list_indexes proxy failed: {e:#}");
        ApiError::bad_gateway(format!("upstream search daemon: {e:#}"))
    })
}
