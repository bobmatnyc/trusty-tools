//! The read surface, served as JSON-RPC methods (#6285 slice 2).
//!
//! Why: slice 1 bound a hardened Unix socket beside the daemon's HTTP listener
//! and registered `search.health` on it. This slice puts the READ routes there
//! too. HTTP is untouched and still serves every one of them — the socket is an
//! ADDITIONAL way to reach the same body, not a replacement, until the retire
//! slice deletes the axum surface and moves the eleven dialling crates.
//!
//! What: the method-to-route table below, the params each method decodes, and
//! [`register`], which mounts every name onto the router `service::socket`
//! builds. Each handler is a thin adapter over a `*_report` function in
//! `service::server` — this file decides WHICH method exists and what it
//! decodes, never what it does.
//!
//! ## Method → route
//!
//! | Method | HTTP route |
//! |---|---|
//! | `search.indexes.list` | `GET /indexes` |
//! | `search.index.status` | `GET /indexes/{id}/status` |
//! | `search.index.config.get` | `GET /indexes/{id}/config` |
//! | `search.config.get` | `GET /config` |
//! | `search.chunks.list` | `GET /indexes/{id}/chunks` |
//! | `search.graph.get` | `GET /indexes/{id}/graph` |
//! | `search.graph.stats` | `GET /indexes/{id}/graph/stats` |
//! | `search.graph.neighbors` | `GET /indexes/{id}/graph/neighbors` |
//! | `search.call_chain` | `GET /indexes/{id}/call_chain` |
//!
//! A route whose HTTP form splits its arguments across a path segment and a
//! query string carries them as ONE `params` object here: the path's `{id}`
//! becomes an `index_id` field beside the query fields, which are otherwise the
//! same names the axum `Query` extractor decodes. The one difference a caller
//! sees is typing — a query string delivers `limit=100` as text and this
//! delivers it as a JSON number.
//!
//! ## What guards these methods
//!
//! The socket runs `ensure_peer_is_self` on every accepted connection before a
//! byte is read, over a `0600` socket in a `0700` directory. HTTP layers the
//! router-wide same-origin write guard (#3304), which is a browser-CSRF defence
//! that allows every request carrying no `Origin` header and authenticates
//! nobody. Nothing is dropped by moving across: there is no `Origin` header on a
//! Unix socket, and the peer-uid check refuses callers the origin guard waves
//! through. Every method here is a read, so neither guard was load-bearing for
//! them in the first place.
//!
//! Test: `reads_tests.rs` — one `*_over_the_socket_matches_the_http_body` per
//! family, driving the real axum router and the real socket router against one
//! shared state.

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::service::server::{
    CallChainParams, ChunksParams, ConfigResponse, GraphQueryParams, IndexConfigView,
    ListIndexesParams, NeighborsParams, SearchAppState,
};
use crate::service::socket::NoParams;

use super::error::rpc_error_from_http;

#[cfg(test)]
#[path = "reads_tests.rs"]
mod tests;

/// `GET /indexes` — every registered index, flat / tree / details.
pub const METHOD_INDEXES_LIST: &str = "search.indexes.list";
/// `GET /indexes/{id}/status` — one index's stages, capabilities and footprint.
pub const METHOD_INDEX_STATUS: &str = "search.index.status";
/// `GET /indexes/{id}/config` — one index's hygiene and component config.
pub const METHOD_INDEX_CONFIG_GET: &str = "search.index.config.get";
/// `GET /config` — the daemon's resolved memory limits.
pub const METHOD_CONFIG_GET: &str = "search.config.get";
/// `GET /indexes/{id}/chunks` — a page of one index's chunk corpus.
pub const METHOD_CHUNKS_LIST: &str = "search.chunks.list";
/// `GET /indexes/{id}/graph` — the whole symbol graph as D3/Cytoscape JSON.
pub const METHOD_GRAPH_GET: &str = "search.graph.get";
/// `GET /indexes/{id}/graph/stats` — node/edge counts and the per-kind breakdown.
pub const METHOD_GRAPH_STATS: &str = "search.graph.stats";
/// `GET /indexes/{id}/graph/neighbors` — bounded BFS from one node.
pub const METHOD_GRAPH_NEIGHBORS: &str = "search.graph.neighbors";
/// `GET /indexes/{id}/call_chain` — the annotated call tree, as plain text.
pub const METHOD_CALL_CHAIN: &str = "search.call_chain";

/// Every method this slice registers, in registration order.
///
/// Why: `service::socket::METHODS` is the array a consumer contract test
/// compares the router against, and it lists these by reference rather than by
/// a second set of literals — so a rename here is a compile error there rather
/// than a drift only a running consumer would find.
/// Test: `rpc_router_registers_every_documented_method` in `socket_tests.rs`.
pub const METHODS: &[&str] = &[
    METHOD_INDEXES_LIST,
    METHOD_INDEX_STATUS,
    METHOD_INDEX_CONFIG_GET,
    METHOD_CONFIG_GET,
    METHOD_CHUNKS_LIST,
    METHOD_GRAPH_GET,
    METHOD_GRAPH_STATS,
    METHOD_GRAPH_NEIGHBORS,
    METHOD_CALL_CHAIN,
];

/// The one argument an index-scoped method with no query string carries.
///
/// Why a struct: JSON-RPC carries one `params` value, so a bare string would
/// give these methods a shape none of their siblings has.
#[derive(Debug, Deserialize)]
pub struct IndexRef {
    /// The index the call is about — the `{id}` path segment on HTTP.
    pub index_id: String,
}

/// An index-scoped method's params: the path's `{id}` beside the query fields.
///
/// Why generic: five methods have this exact shape and differ only in `P`, so
/// one type is what keeps `index_id` spelled the same on all of them.
/// What: `#[serde(flatten)]` puts `P`'s fields at the top level of `params`, so
/// a caller sends `{"index_id": "x", "limit": 50}` rather than nesting the
/// query half under a second key.
#[derive(Debug, Deserialize)]
pub struct IndexScoped<P> {
    /// The index the call is about — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// The fields the axum `Query` extractor decodes for this route.
    #[serde(flatten)]
    pub params: P,
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why: the only route-specific half of the socket server. Hardening, framing,
/// the JSON-RPC envelope and the accept loop are all
/// [`trusty_common::uds::server`]'s.
/// What: one `typed` registration per method, each cloning the `Arc` handle to
/// the shared [`SearchAppState`] — the daemon's ONE state, the same value the
/// axum router was built on, so the two transports read one registry rather
/// than two copies of it. Every fallible body returns the `(status, body)` pair
/// its axum handler returns, and [`rpc_error_from_http`] turns that into the
/// error frame, so a refusal cannot be one thing over HTTP and another here.
/// Test: `rpc_router_registers_every_documented_method` in `socket_tests.rs`;
/// the `*_over_the_socket_matches_the_http_body` cases drive each registration
/// against its HTTP twin.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    /// One method whose body cannot fail.
    macro_rules! ok {
        ($router:expr, $name:expr, $req:ty, $resp:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, $resp, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { Ok($call) }
            })
        }};
    }

    /// One method whose body returns `Result<_, (StatusCode, Value)>`.
    macro_rules! fallible {
        ($router:expr, $name:expr, $req:ty, $resp:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, $resp, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { $call.map_err(|(status, body)| rpc_error_from_http(status, &body)) }
            })
        }};
    }

    use crate::service::server::{
        call_chain_report, config_report, graph_neighbors_report, graph_report, graph_stats_report,
        index_chunks_report, index_config_report, index_status_report, list_indexes_report,
    };

    let r = router;

    // ---- indexes -----------------------------------------------------------
    let r = ok!(
        r,
        METHOD_INDEXES_LIST,
        ListIndexesParams,
        serde_json::Value,
        |s, p| list_indexes_report(&s, &p)
    );
    let r = fallible!(
        r,
        METHOD_INDEX_STATUS,
        IndexRef,
        serde_json::Value,
        |s, p| index_status_report(&s, &p.index_id).await
    );

    // ---- config ------------------------------------------------------------
    let r = fallible!(
        r,
        METHOD_INDEX_CONFIG_GET,
        IndexRef,
        IndexConfigView,
        |s, p| index_config_report(&s, &p.index_id)
    );
    let r = ok!(r, METHOD_CONFIG_GET, NoParams, ConfigResponse, |_s, _p| {
        config_report()
    });

    // ---- chunks ------------------------------------------------------------
    let r = fallible!(
        r,
        METHOD_CHUNKS_LIST,
        IndexScoped<ChunksParams>,
        serde_json::Value,
        |s, p| index_chunks_report(&s, &p.index_id, &p.params).await
    );

    // ---- graph -------------------------------------------------------------
    let r = fallible!(
        r,
        METHOD_GRAPH_GET,
        IndexScoped<GraphQueryParams>,
        serde_json::Value,
        |s, p| graph_report(&s, &p.index_id, &p.params).await
    );
    let r = fallible!(
        r,
        METHOD_GRAPH_STATS,
        IndexRef,
        serde_json::Value,
        |s, p| graph_stats_report(&s, &p.index_id).await
    );
    let r = fallible!(
        r,
        METHOD_GRAPH_NEIGHBORS,
        IndexScoped<NeighborsParams>,
        serde_json::Value,
        |s, p| graph_neighbors_report(&s, &p.index_id, &p.params).await
    );

    // ---- call chain --------------------------------------------------------
    fallible!(
        r,
        METHOD_CALL_CHAIN,
        IndexScoped<CallChainParams>,
        String,
        |s, p| call_chain_report(&s, &p.index_id, p.params).await
    )
}
