//! `search.file.get` — one indexed file and, optionally, its diff (#9029).
//!
//! Why: the search dashboard's file viewer opens a hit as the whole file with a
//! diff view, and trusty-console reaches this daemon over its socket only.
//! What: [`register`] mounts the one method. Socket-only, like
//! `search.project.resolve`: #6285 is retiring the HTTP listener.
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.file.get` | none | free |
//!
//! Free: one file read capped at 1 MiB and at most two git runs, each bounded
//! by a deadline; no index lock is taken, so it cannot queue behind a query.
//!
//! Params: `{index_id, path, diff?: "none" | "head"}`. A refusal is an error
//! frame whose `data` is the refusal body (`error`, `index_id`, and `reason`
//! where one applies).
//!
//! Test: `rpc/file_tests.rs`.
//!
//! [`register`]: crate::service::rpc::file::register

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use crate::service::server::{FileGetParams, SearchAppState};

use super::error::rpc_error_from_http;

/// Read one indexed file, with its diff against `HEAD` when asked.
pub const METHOD_FILE_GET: &str = "search.file.get";

/// Every method this family registers. Spliced into `service::socket::METHODS`.
/// Test: `every_family_method_is_spliced_into_the_socket_method_list`.
pub const METHODS: &[&str] = &[METHOD_FILE_GET];

/// Mount `search.file.get` onto `router`.
///
/// Why: the socket entry point for the dashboard file viewer (#9029).
/// What: decodes [`FileGetParams`] and runs
/// [`crate::service::server::file_get_report`]. A refusal becomes the code
/// [`rpc_error_from_http`] picks for its status, with the whole body as `data`
/// so a caller can branch on `error` and `reason` for every status, not only
/// a 503.
/// Test: `file_get_over_the_daemon_socket_returns_content_and_head_diff`,
/// `file_get_refusals_keep_their_codes_over_the_socket`,
/// `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<FileGetParams, serde_json::Value, _, _>(METHOD_FILE_GET, move |params| {
        let state = Arc::clone(&held);
        async move {
            crate::service::server::file_get_report(&state, params)
                .await
                .map_err(|(status, body)| rpc_error_from_http(status, &body).with_data(body))
        }
    })
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
