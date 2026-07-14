//! Layer-3 chat-based portfolio manager surface — `tm manager` (epic #2109,
//! DOC-36 phase 1a).
//!
//! Why: DOC-35 §1.1's three-layer model tops out at a Layer-3 "single agent with
//! FULL SCOPE of the user's activities". DOC-36 designs that layer as a
//! daemon-owned component exposing an API-first `/api/v1/manager/*` surface
//! (§3.1/§3.2). This module is the phase-1a scaffold of that surface: it does NOT
//! reimplement #2108's per-project control plane — it COMPOSES it (cross-project
//! rollup) and stays strictly read-only (DOC-36 §2.1 boundary: the manager never
//! mutates a Deliverable/Milestone, never sets an autonomy tier, never acts on a
//! session without an explicit driving call). Everything here is curl-testable
//! locally with no channel/bot token and no live LLM (§4 local-testability bar).
//! What: three phase-1a work items land here as focused submodules —
//! - [`state`] — the daemon-owned [`ManagerState`] threaded through every handler
//!   (WI-1, #2578), holding the portfolio palace (and, later, the inference
//!   adapter + poll loop);
//! - [`memory`] — portfolio `trusty-memory` palace provisioning, auto-created at
//!   startup and degrade-graceful (WI-5, #2582);
//! - [`status`] — the deterministic `GET /manager/status` cross-project rollup,
//!   NO LLM (WI-2, #2579);
//! - [`inference`] — the `trusty_common::inference` resolution seam shared by the
//!   digest/chat LLM calls (WI-3/WI-4, §3.3);
//! - [`digest`] — `GET /manager/digest`, the LLM-authored portfolio narrative with
//!   a deterministic fallback (WI-3, #2580);
//! - [`chat`] + [`chat_store`] — `POST /manager/chat`, the read-only
//!   conversation-keyed portfolio chat loop (WI-4, #2581);
//! - [`version`] — the `GET /manager/version` capabilities stub that makes the
//!   scaffold self-describing and curl-observable (WI-1, #2578).
//!
//! The handlers are mounted into the daemon's axum router by
//! [`crate::daemon::api::router`]; [`ManagerState`] is owned by
//! [`crate::daemon::state::DaemonState`] and reached via
//! [`DaemonState::manager_state`](crate::daemon::state::DaemonState::manager_state),
//! mirroring how the L2 proxy focus store is owned.
//! Test: `tests/manager_routes.rs` (real-HTTP contract, mirrors
//! `tests/proxy_routes.rs`) plus the in-module unit tests in each submodule.

pub mod act;
pub mod actuator;
pub mod chat;
pub mod chat_store;
pub mod digest;
pub mod inference;
pub mod memory;
pub mod route_task;
pub mod state;
pub mod status;
pub mod version;

pub use act::{ActRequest, ActResponse, ProposedAction, manager_act_route};
pub use actuator::{
    DaemonLauncher, LaunchOutcome, ManagerActuator, ProxyActuator, SessionLauncher,
};
pub use chat::{ChatReplyBody, ChatRequestBody, manager_chat_route};
pub use chat_store::{ChatStore, ChatTurn, TurnRole};
pub use digest::{DigestResponse, DigestScope, manager_digest_route};
pub use inference::{InferenceUnavailable, ManagerInference};
pub use memory::{PORTFOLIO_PALACE_ID, PortfolioPalace};
pub use route_task::{ResolvedBy, RouteTaskRequest, RouteTaskResponse, manager_route_task_route};
pub use state::ManagerState;
pub use status::{
    PortfolioStatusResponse, PortfolioTotals, aggregate_portfolio_status, manager_status_route,
};
pub use version::{
    ManagerEndpoint, ManagerPalaceStatus, ManagerVersionResponse, manager_version_route,
};

/// Build the sub-router for the Layer-3 portfolio manager surface
/// (`/api/v1/manager/*`, epic #2109, DOC-36 §3.2).
///
/// Why: co-locating every manager route registration with the module that owns
/// the handlers (rather than inlining them in the daemon's monolithic
/// `api::router`) keeps this cohesive cluster self-contained and lets
/// `api::router` compose it with a single `.merge` — mirroring how the L2
/// [`crate::daemon::managed_routes::proxy::proxy_router`] is composed. Every route
/// binds the same [`DaemonState`](crate::daemon::state::DaemonState) the parent
/// router carries, so merging is state-preserving.
/// What: registers the phase-1 read-only triad (`version`/`status`/`digest`/`chat`)
/// plus the phase-2 `route-task` (advisory) and `act` (propose→confirm) verbs.
/// Test: the real-HTTP `tests/manager_routes.rs` and `tests/manager_routing.rs`
/// exercise these routes exactly as before this extraction.
pub fn manager_router() -> axum::Router<std::sync::Arc<crate::daemon::state::DaemonState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/v1/manager/version", get(manager_version_route))
        .route("/api/v1/manager/status", get(manager_status_route))
        .route("/api/v1/manager/digest", get(manager_digest_route))
        .route("/api/v1/manager/chat", post(manager_chat_route))
        .route("/api/v1/manager/route-task", post(manager_route_task_route))
        .route("/api/v1/manager/act", post(manager_act_route))
}
