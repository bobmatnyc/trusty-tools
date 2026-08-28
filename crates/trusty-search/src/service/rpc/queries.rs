//! The query surface, served as JSON-RPC methods (#6285 slice 3).
//!
//! Why: slice 2 moved the READ routes onto the socket. This slice moves the six
//! routes that actually ASK something of an index — the per-index and global
//! hybrid searches, the two greps, code-to-code similarity, and typeahead. HTTP
//! is untouched and still serves every one of them; the socket is an ADDITIONAL
//! way to reach the same body until the retire slice.
//!
//! What: the method-to-route table below, the params each method decodes,
//! [`guarded`], and [`register`].
//!
//! ## Method → route
//!
//! | Method | HTTP route |
//! |---|---|
//! | `search.query` | `POST /indexes/{id}/search` |
//! | `search.query.all` | `POST /search` |
//! | `search.grep` | `POST /indexes/{id}/grep` |
//! | `search.grep.all` | `POST /grep` |
//! | `search.similar` | `POST /indexes/{id}/search_similar` |
//! | `search.typeahead` | `GET /indexes/{id}/typeahead` |
//!
//! ## Two param shapes, because the routes have two argument shapes
//!
//! `search.typeahead` is the only one whose HTTP form is a path segment plus a
//! QUERY STRING, so it carries slice 2's [`reads::IndexScoped`]: the `{id}`
//! becomes an `index_id` field beside the flattened query fields.
//!
//! The other five take a request BODY, and a body is ONE JSON document axum
//! hands to ONE `Deserialize` impl. [`IndexBody`] keeps it one document, nested
//! under `body`, rather than flattening its fields alongside `index_id`. That
//! is not cosmetic: `SearchQuery` and `GlobalSearchRequest` both carry
//! `deny_unknown_fields`, which #3401 added because a silently-ignored FILTER
//! field means "returns too much data". An unmodified decode is what keeps that
//! refusal identical on both transports. The two global methods have no path
//! segment at all, so they take their request as `params` directly.
//!
//! ## What guards these methods
//!
//! Everything slice 2's `reads` module documents — the peer-uid check over a
//! `0600` socket in a `0700` directory — plus the two guards this route family
//! alone carries on HTTP. These six routes are exactly axum's
//! `interactive_limited` group: admission-limited (#2845) and bounded by the
//! interactive query deadline (#907). Both live on [`SearchAppState`] so the
//! socket gates on the SAME semaphore and the SAME deadline rather than a
//! second copy of each — see [`guarded`].
//!
//! ## One limit that does NOT match HTTP yet
//!
//! A response frame is bounded by `RpcServeOptions::max_frame_bytes` (8 MiB by
//! default), in both directions; HTTP bounds only the REQUEST body (2 MiB) and
//! streams responses of any size. So a `search.query` with `full_content` and a
//! large `top_k`, or a grep with a large `max_results`, can produce a body HTTP
//! serves and this socket refuses. The refusal is loud rather than a truncation,
//! and no consumer dials these names yet. The retire slice raises the budget on
//! the listener and the client together — raising it here alone would only move
//! which end refuses.
//!
//! Test: `queries_tests.rs` — one `*_over_the_socket_matches_the_http_body` per
//! family, driving the real axum router and the real RPC router against one
//! shared state, over an index holding a real corpus.

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::core::indexer::SearchQuery;
use crate::service::grep::GrepRequest;
use crate::service::server::{GlobalSearchRequest, SearchAppState, SearchSimilarRequest};

use super::error::rpc_error_from_http;
use super::reads::IndexScoped;

#[cfg(test)]
#[path = "queries_tests.rs"]
mod tests;

/// `POST /indexes/{id}/search` — hybrid BM25 + vector + KG search over one index.
pub const METHOD_QUERY: &str = "search.query";
/// `POST /search` — the same search fanned out across every registered index.
pub const METHOD_QUERY_ALL: &str = "search.query.all";
/// `POST /indexes/{id}/grep` — ripgrep-parity regex search over one index.
pub const METHOD_GREP: &str = "search.grep";
/// `POST /grep` — the same grep fanned out across every registered index.
pub const METHOD_GREP_ALL: &str = "search.grep.all";
/// `POST /indexes/{id}/search_similar` — chunks near a known file/function.
pub const METHOD_SIMILAR: &str = "search.similar";
/// `GET /indexes/{id}/typeahead` — per-keystroke autocomplete.
pub const METHOD_TYPEAHEAD: &str = "search.typeahead";

/// Every method this slice registers, in registration order.
///
/// Why: same contract as `reads::METHODS` — `service::socket::METHODS` lists
/// these by reference rather than by a second set of literals, so a rename here
/// is a compile error there rather than a drift only a running consumer would
/// find.
/// Test: `rpc_router_registers_every_documented_method` and
/// `every_family_method_is_spliced_into_the_socket_method_list` in
/// `socket_tests.rs`.
pub const METHODS: &[&str] = &[
    METHOD_QUERY,
    METHOD_QUERY_ALL,
    METHOD_GREP,
    METHOD_GREP_ALL,
    METHOD_SIMILAR,
    METHOD_TYPEAHEAD,
];

/// An index-scoped method whose HTTP form takes a request BODY.
///
/// Why nested rather than [`IndexScoped`]'s flatten: the body is one JSON
/// document on HTTP and stays one here, so `B`'s own `Deserialize` runs
/// unmodified. `SearchQuery` and `GlobalSearchRequest` reject unknown fields
/// (#3401) precisely because a misspelled filter returns too much data, and an
/// unmodified decode is what keeps that refusal identical on both transports.
/// What: `{"index_id": "x", "body": { … }}`, where `body` is byte-for-byte the
/// JSON a caller would POST.
/// Test: `an_unknown_search_field_reports_invalid_params_on_both_transports`.
#[derive(Debug, Deserialize)]
pub struct IndexBody<B> {
    /// The index the call is about — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// The request body, exactly as the HTTP route's JSON extractor takes it.
    pub body: B,
}

/// Run one query body behind the two guards HTTP puts in front of these routes.
///
/// Why: these six are axum's `interactive_limited` group, and the guards are
/// tower middleware a second transport cannot reach. Without this a socket
/// search would bypass the admission limit that exists to stop the #2845 storm,
/// and would hang forever against a stalled embedder where the HTTP route for
/// the same query answers 408 (#907).
/// What: the limiter first and the deadline inside it — the same nesting the
/// axum router builds, so a caller waits for admission and only then starts
/// spending its query budget. Both are read from [`SearchAppState`], which is
/// the daemon's ONE of each. The permit is held until the body finishes.
/// Test: `queries_are_refused_when_the_shared_limiter_is_saturated`,
/// `a_query_that_outlasts_the_deadline_reports_the_same_refusal_on_both_transports`.
async fn guarded<T, F>(state: &Arc<SearchAppState>, body: F) -> Result<T, RpcError>
where
    F: std::future::Future<Output = Result<T, (axum::http::StatusCode, serde_json::Value)>>,
{
    let _permit = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .map_err(|(status, refusal)| rpc_error_from_http(status, &refusal))?;
    crate::service::query_timeout::with_query_deadline(&state.query_timeout, body)
        .await
        .unwrap_or_else(Err)
        .map_err(|(status, refusal)| rpc_error_from_http(status, &refusal))
}

/// A typed report as the JSON axum would have written for it.
///
/// Why: `serde_json::to_value`, which the router applies to a typed response,
/// WIDENS an `f32` field to `f64` — a typeahead hit's `score` of `0.001f32`
/// becomes `0.0010000000474974513` — while axum serialises the struct straight
/// to text and writes `0.001`. Both name the same `f32`, but they are different
/// digits on the wire, and this surface's whole contract is that the two
/// transports answer identically. HTTP's encoding is the one eleven crates read
/// today, so the socket matches it rather than the reverse.
/// What: serialise to text, then parse — the same two steps HTTP takes, in the
/// same order, so the `f32` shortest-representation is chosen once.
///
/// Only the two families whose report is a TYPED struct need this.
/// `search_report` and its two siblings already return `serde_json::Value`, and
/// a `Value` holds the widened `f64` on BOTH transports, so they agree already.
///
/// # Errors
///
/// Never in practice: `T` is a report this daemon just built, and a report that
/// cannot serialise cannot be sent over HTTP either. Reported as an internal
/// error rather than unwrapped, because a panic here would take the connection
/// down instead of answering the caller.
/// Test: `typeahead_over_the_socket_matches_the_http_body`,
/// `grep_over_the_socket_matches_the_http_body`.
fn as_http_body<T: serde::Serialize>(report: T) -> Result<serde_json::Value, RpcError> {
    let text = serde_json::to_string(&report).map_err(|e| {
        tracing::error!(error = %e, "rpc: a query report could not be serialised");
        RpcError::new(
            trusty_common::uds::server::CODE_INTERNAL_ERROR,
            format!("could not serialise the report: {e}"),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| {
        tracing::error!(error = %e, "rpc: a serialised query report could not be re-read");
        RpcError::new(
            trusty_common::uds::server::CODE_INTERNAL_ERROR,
            format!("could not re-read the report: {e}"),
        )
    })
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why: the route-specific half of the query surface, kept beside `reads` so
/// each slice adds one file rather than editing one.
/// What: one `typed` registration per method, each cloning the `Arc` handle to
/// the shared [`SearchAppState`] and running its `*_report` core through
/// [`guarded`]. Every core is the SAME function the axum handler wraps, so a
/// body cannot be one thing over HTTP and another here.
/// Test: `rpc_router_registers_every_documented_method` in `socket_tests.rs`;
/// the `*_over_the_socket_matches_the_http_body` cases drive each registration
/// against its HTTP twin.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    /// One method whose core already answers a `serde_json::Value`.
    macro_rules! query {
        ($router:expr, $name:expr, $req:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, serde_json::Value, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { guarded(&$s, async { $call }).await }
            })
        }};
    }

    /// One method whose core answers a TYPED report, re-encoded the way axum
    /// writes it — see [`as_http_body`].
    macro_rules! typed_query {
        ($router:expr, $name:expr, $req:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, serde_json::Value, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { as_http_body(guarded(&$s, async { $call }).await?) }
            })
        }};
    }

    use crate::service::server::{
        global_grep_report, global_search_report, grep_report, search_report,
        search_similar_report, typeahead_report,
    };

    let r = router;

    // ---- hybrid search -----------------------------------------------------
    let r = query!(r, METHOD_QUERY, IndexBody<SearchQuery>, |s, p| {
        search_report(&s, &p.index_id, p.body).await
    });
    let r = query!(r, METHOD_QUERY_ALL, GlobalSearchRequest, |s, p| {
        global_search_report(&s, p).await
    });

    // ---- grep --------------------------------------------------------------
    let r = typed_query!(r, METHOD_GREP, IndexBody<GrepRequest>, |s, p| {
        grep_report(&s, &p.index_id, p.body).await
    });
    let r = typed_query!(r, METHOD_GREP_ALL, GrepRequest, |s, p| {
        global_grep_report(&s, p).await
    });

    // ---- similarity --------------------------------------------------------
    let r = query!(
        r,
        METHOD_SIMILAR,
        IndexBody<SearchSimilarRequest>,
        |s, p| search_similar_report(&s, &p.index_id, p.body).await
    );

    // ---- typeahead ---------------------------------------------------------
    typed_query!(
        r,
        METHOD_TYPEAHEAD,
        IndexScoped<crate::service::server::TypeaheadParams>,
        |s, p| typeahead_report(&s, &p.index_id, p.params).await
    )
}
