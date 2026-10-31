//! `search.project.ensure` over the socket (#10531).
//!
//! Why: tm registers a project and needs its one pinned index to exist, with
//! no second index on a repeat call and no index nested in another's root.
//! What: [`register`] mounts the method onto the free lane (a registry write,
//! no limiter, like `search.index.create`). The core is
//! [`crate::service::server::ensure_project_report`]. A refusal maps through
//! `rpc_error_from_http` and also rides as the error's `data`, so a caller
//! reads `data.error` (`project_root_overlap`, `project_root_unindexable`,
//! `project_id_taken`) and `data.overlapping_index_id`.
//!
//! | Method | Lane |
//! |---|---|
//! | `search.project.ensure` | free |
//!
//! Test: `service/server/project_ensure_tests.rs`; lane pin in `lanes_tests.rs`.
//!
//! [`register`]: crate::service::rpc::project_ensure::register

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use super::error::rpc_error_from_http;
use crate::service::server::{ensure_project_report, EnsureProjectRequest, SearchAppState};

/// Find-or-create the index of one tm project.
pub const METHOD_PROJECT_ENSURE: &str = "search.project.ensure";

/// Every method this family registers.
pub const METHODS: &[&str] = &[METHOD_PROJECT_ENSURE];

/// Mount `search.project.ensure` onto `router`.
///
/// Test: `rpc_router_registers_every_documented_method` in `socket_tests.rs`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<EnsureProjectRequest, serde_json::Value, _, _>(METHOD_PROJECT_ENSURE, move |p| {
        let state = Arc::clone(&held);
        async move {
            ensure_project_report(&state, p)
                .await
                .map_err(|(status, body)| rpc_error_from_http(status, &body).with_data(body))
        }
    })
}
