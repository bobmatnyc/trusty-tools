//! Warm-all, served as JSON-RPC methods (#9027).
//!
//! Why: trusty-console reaches this daemon over its Unix socket only
//! (ADR-0032), so the dashboard's "Warm all indexes" control needs socket twins
//! of the two HTTP routes.
//! What: the method-to-route table below and [`register`]. Both methods run
//! the SAME `*_report` core their axum handlers wrap.
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.warm.start` | `POST /warm` | free |
//! | `search.warm.status` | `GET /warm/status` | free |
//!
//! Both are `free` on HTTP: start returns at once and status is a snapshot
//! read, so queueing either behind the admission limiter would only delay the
//! call that tells a caller why its searches are slow.
//!
//! Test: `warm_over_the_socket_matches_the_http_body`.
//!
//! [`register`]: crate::service::rpc::warm::register

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use crate::service::server::{SearchAppState, WarmStartRequest};
use crate::service::socket::NoParams;

use super::error::rpc_error_from_http;

/// `POST /warm` — start warming every registered index, or join the running warm.
pub const METHOD_WARM_START: &str = "search.warm.start";
/// `GET /warm/status` — per-index warm state, totals, window and memory.
pub const METHOD_WARM_STATUS: &str = "search.warm.status";

/// Every method this family registers, in registration order. Spliced into
/// `service::socket::METHODS` by reference.
/// Test: `every_family_method_is_spliced_into_the_socket_method_list`.
pub const METHODS: &[&str] = &[METHOD_WARM_START, METHOD_WARM_STATUS];

/// Mount both warm methods onto `router`.
///
/// Why: the socket half of the warm-all surface.
/// What: `search.warm.start` decodes `Option<WarmStartRequest>` so a call with
/// no params (`null`) is the default start, exactly as an empty `POST /warm`
/// body is; a stray field is refused on both transports (`deny_unknown_fields`).
/// Test: `warm_over_the_socket_matches_the_http_body`,
/// `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    use crate::service::server::{warm_start_report, warm_status_report};

    let held = Arc::clone(state);
    let router = router.typed::<Option<WarmStartRequest>, serde_json::Value, _, _>(
        METHOD_WARM_START,
        move |req| {
            let state = Arc::clone(&held);
            async move {
                warm_start_report(&state, req)
                    .map_err(|(status, body)| rpc_error_from_http(status, &body))
            }
        },
    );
    let held = Arc::clone(state);
    router.typed::<NoParams, serde_json::Value, _, _>(METHOD_WARM_STATUS, move |_| {
        let state = Arc::clone(&held);
        async move { Ok(warm_status_report(&state)) }
    })
}

#[cfg(test)]
#[path = "warm_tests.rs"]
mod tests;
