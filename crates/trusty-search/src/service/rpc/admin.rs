//! The operational remainder, served as JSON-RPC methods (#6285 slice 5.5).
//!
//! Why: slices 1–5 moved health, the reads, the queries, the writes and the two
//! streams. Four routes with a named consumer were still HTTP-only, and the
//! retire slice cannot move that consumer onto a method the socket does not
//! serve. This slice closes the gap the #6285 consumer map recorded: the two
//! config WRITES the dashboard and `trusty-search config set` drive,
//! `GET /logs/tail` (which `trusty-common`'s monitor dials), and
//! `GET /registry/orphans` (which `trusty-console`'s cleanup tab dials, #6371).
//!
//! What: the method-to-route table below, the params each method decodes, and
//! [`register`]. Every handler is a thin adapter over a `*_report` core in
//! `service::server` or `service::orphan_report` — this file decides WHICH
//! method exists and what it decodes, never what it does.
//!
//! ## Method → route
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.index.config.set` | `PATCH /indexes/{id}/config` | free |
//! | `search.config.set` | `PATCH /config` | free |
//! | `search.logs.tail` | `GET /logs/tail` | free |
//! | `search.registry.orphans` | `GET /registry/orphans` | free |
//!
//! All four HTTP routes sit in `service::server::build_router_on`'s `free`
//! group — no admission limiter, no query deadline — and all four methods copy
//! that. The two writes are `free` on HTTP for the same reason
//! `DELETE /indexes/{id}` is (slice 4's lane table): they are registry-level,
//! and putting them behind the limiter here would make a config edit queue
//! behind a running reindex that the HTTP route sails past.
//!
//! ## Two of these are WRITES, so each carries a failure arm that checks state
//!
//! `search.index.config.set` re-registers a handle, rewrites an `indexes.toml`
//! row and can start a background catch-up; `search.config.set` moves two
//! process-global memory limits. The slice-4 bar applies to both: every refusal
//! case asserts the refusal AND re-reads the thing that must not have moved —
//! the handle's config view, the persisted row, the resolved limits.
//!
//! ## What guards these methods
//!
//! Everything slice 2's `reads` module documents — the peer-uid check over a
//! `0600` socket in a `0700` directory. `with_guarded_middleware`, the #3304
//! same-origin write guard the axum router wraps the two PATCH routes in, does
//! not carry over and does not need to: it is browser-CSRF defence for a
//! listener a page can reach, and a Unix socket has no origin and no browser
//! (#6277 design review, the same conclusion slices 1 and 4 recorded).
//!
//! Test: `admin_tests.rs` — one `*_over_the_socket_matches_the_http_body` per
//! method plus, for each of the two writes, a failure arm proving the refusal is
//! identical AND that neither in-memory nor on-disk state advanced behind it.
//!
//! [`register`]: crate::service::rpc::admin::register

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::service::orphan_report::OrphanCensus;
use crate::service::server::{
    ConfigResponse, LogsTailParams, PatchConfigRequest, PatchIndexConfigRequest, SearchAppState,
};

use super::as_http_body;
use super::error::rpc_error_from_http;

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

/// `PATCH /indexes/{id}/config` — update one index's hygiene and component config.
pub const METHOD_INDEX_CONFIG_SET: &str = "search.index.config.set";
/// `PATCH /config` — retune the daemon's two memory limits without a restart.
pub const METHOD_CONFIG_SET: &str = "search.config.set";
/// `GET /logs/tail` — the most recent N lines from the in-memory log ring.
pub const METHOD_LOGS_TAIL: &str = "search.logs.tail";
/// `GET /registry/orphans` — the `indexes.toml` census of dead roots (#6371).
pub const METHOD_REGISTRY_ORPHANS: &str = "search.registry.orphans";

/// Every method this slice registers, in registration order.
///
/// Why: the same contract `reads::METHODS`, `queries::METHODS`,
/// `writes::METHODS` and `streams::METHODS` carry — `service::socket::METHODS`
/// splices these in by reference rather than restating the literals, so a rename
/// is a compile error there rather than a drift only a running consumer would
/// find.
/// Test: `rpc_router_registers_every_documented_method` and
/// `every_family_method_is_spliced_into_the_socket_method_list` in
/// `socket_tests.rs`.
pub const METHODS: &[&str] = &[
    METHOD_INDEX_CONFIG_SET,
    METHOD_CONFIG_SET,
    METHOD_LOGS_TAIL,
    METHOD_REGISTRY_ORPHANS,
];

/// The params of [`METHOD_INDEX_CONFIG_SET`].
///
/// Why nested rather than flattened: the same reason `writes::IndexBody` is —
/// the body is one JSON document on HTTP and stays one here, so
/// [`PatchIndexConfigRequest`]'s own `Deserialize` runs unmodified and cannot
/// decode differently because a field now shares a namespace with `index_id`.
/// That matters more here than anywhere else on the surface: every field of that
/// body is `Option`, so a field silently shadowed by the wrapper would read as
/// "leave it alone" rather than as an error.
/// What: `{"index_id": "x", "body": { … }}`, where `body` is byte-for-byte the
/// JSON a caller would PATCH.
/// Test: `index_config_set_over_the_socket_matches_the_http_body`.
#[derive(Debug, Deserialize)]
pub struct IndexConfigSet {
    /// The index the call is about — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// The request body, exactly as the HTTP route's JSON extractor takes it.
    pub body: PatchIndexConfigRequest,
}

/// The params of [`METHOD_LOGS_TAIL`].
///
/// Why its own type rather than decoding [`LogsTailParams`] directly: `n` is a
/// query parameter with a serde default on HTTP, and a JSON-RPC call to a method
/// with no arguments carries `params: null` — which `LogsTailParams` refuses
/// outright, so every default-shaped tail would answer `invalid_params`.
/// What: `{}` means [`DEFAULT_LOGS_TAIL_N`] lines; `{"n": 500}` is the same
/// request `?n=500` makes. The clamp lives in the core, not here, so both
/// transports refuse an over-large `n` identically.
///
/// A derived `Deserialize` still refuses `null`, so [`register`] decodes this as
/// `Option<LogsTail>` and reads `None` as the default — an absent `params` and an
/// explicit `null` arrive here as the same `Value::Null` (#6285).
/// Test: `logs_tail_over_the_socket_matches_the_http_body`,
/// `logs_tail_clamps_n_on_the_socket_too`.
///
/// [`DEFAULT_LOGS_TAIL_N`]: crate::service::server::LogsTailParams
#[derive(Debug, Default, Deserialize)]
pub struct LogsTail {
    /// How many lines to return. Absent means the route's own default.
    #[serde(default)]
    pub n: Option<usize>,
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why: the route-specific half of the last four routes with a named consumer,
/// kept beside `reads`, `queries`, `writes` and `streams` so each slice adds one
/// file rather than editing one.
/// What: one `typed` registration per method, each cloning the `Arc` handle to
/// the shared [`SearchAppState`] and running the SAME `*_report` core its axum
/// handler wraps. [`METHOD_REGISTRY_ORPHANS`] takes no state at all — it reads
/// the registry file, which is the entire reason #6371 added the route.
/// Test: `rpc_router_registers_every_documented_method` in `socket_tests.rs`;
/// the `*_over_the_socket_matches_the_http_body` cases and their failure-arm
/// siblings drive each registration against its HTTP twin.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    use crate::service::orphan_report::registry_orphans_report;
    use crate::service::server::{
        logs_tail_report, patch_config_report, patch_index_config_report,
    };

    // ---- the two config writes (axum's `free` group) ------------------------
    let held = Arc::clone(state);
    let router = router.typed::<IndexConfigSet, serde_json::Value, _, _>(
        METHOD_INDEX_CONFIG_SET,
        move |p| {
            let state = Arc::clone(&held);
            async move {
                patch_index_config_report(&state, &p.index_id, p.body)
                    .await
                    .map_err(|(status, body)| rpc_error_from_http(status, &body))
            }
        },
    );

    let router = router.typed::<PatchConfigRequest, ConfigResponse, _, _>(
        METHOD_CONFIG_SET,
        move |req| async move { Ok(patch_config_report(req)) },
    );

    // ---- the two operational reads ------------------------------------------
    let held = Arc::clone(state);
    // #6285: `Option<LogsTail>` so a call with no arguments decodes — the router
    // hands a handler `Value::Null` for both an absent and an explicit `params`.
    let router =
        router.typed::<Option<LogsTail>, serde_json::Value, _, _>(METHOD_LOGS_TAIL, move |p| {
            let state = Arc::clone(&held);
            async move {
                let params = match p.and_then(|p| p.n) {
                    Some(n) => LogsTailParams { n },
                    None => LogsTailParams::default(),
                };
                Ok(logs_tail_report(&state, &params))
            }
        });

    router.typed::<super::super::socket::NoParams, serde_json::Value, _, _>(
        METHOD_REGISTRY_ORPHANS,
        move |_params| async move {
            let census: OrphanCensus = registry_orphans_report()
                .await
                .map_err(|(status, body)| rpc_error_from_http(status, &body))?;
            as_http_body(census)
        },
    )
}
