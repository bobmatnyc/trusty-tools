//! Layer-3 chat-based portfolio manager surface — `tm manager` (epic #2109,
//! DOC-36, phases 1-2).
//!
//! Why: DOC-35 §1.1's three-layer model tops out at a Layer-3 "single agent with
//! FULL SCOPE of the user's activities". DOC-36 designs that layer as a
//! daemon-owned component exposing an API-first `/api/v1/manager/*` surface
//! (§3.1/§3.2). This module does NOT reimplement #2108's per-project control
//! plane — it COMPOSES it (cross-project rollup, the resolver, the launch verb,
//! `SessionProxy`). The read-only boundary (DOC-36 §2.1: never mutates a
//! Deliverable/Milestone, never sets an autonomy tier) still holds for
//! `status`/`digest`, and the CHAT LOOP itself never mutates on a PLAIN message —
//! but DOC-36 §6 phase 2 explicitly SUPERSEDES phase 1's "chat is structurally
//! read-only" framing: [`chat`] now supports an in-conversation propose→confirm
//! action flow (#2586), so "the manager never acts on a session without an
//! explicit driving call" is satisfied by the EXPLICIT CONFIRMATION TURN, not by
//! chat being incapable of acting at all. Everything here is curl-testable
//! locally with no channel/bot token and no live LLM (§4 local-testability bar).
//! What: phase-1 submodules —
//! - [`state`] — the daemon-owned [`ManagerState`] threaded through every handler
//!   (WI-1, #2578), holding the portfolio palace, the inference seam, the chat
//!   turn store, the pending-proposal store (#2586), and the actuator override;
//! - [`memory`] — portfolio `trusty-memory` palace provisioning, auto-created at
//!   startup and degrade-graceful (WI-5, #2582);
//! - [`status`] — the deterministic `GET /manager/status` cross-project rollup,
//!   NO LLM (WI-2, #2579);
//! - [`inference`] — the `trusty_common::inference` resolution seam shared by the
//!   digest/chat LLM calls (WI-3/WI-4, §3.3);
//! - [`digest`] — `GET /manager/digest`, the LLM-authored portfolio narrative with
//!   a deterministic fallback (WI-3, #2580);
//! - [`chat`] + [`chat_store`] + [`proposal`] — `POST /manager/chat`, the
//!   conversation-keyed portfolio chat loop (WI-4, #2581) plus its phase-2
//!   in-conversation propose→confirm action flow (#2586);
//! - [`version`] — the `GET /manager/version` capabilities stub that makes the
//!   surface self-describing and curl-observable (WI-1, #2578);
//!
//! and phase-2 submodules —
//!
//! - [`route_task`] — `POST /manager/route-task`, advisory task→project routing
//!   with disambiguation judgment (WI-8, #2585);
//! - [`act`] + [`actuator`] — `POST /manager/act`, the API-first
//!   propose→confirm session launch/inject/summarize flow (WI-9, #2586) and its
//!   execution seam, ALSO the seam `chat.rs`'s confirm turn drives (never
//!   duplicated).
//!
//! The handlers are mounted into the daemon's axum router by
//! [`crate::daemon::api::router`] via [`manager_router`]; [`ManagerState`] is
//! owned by [`crate::daemon::state::DaemonState`] and reached via
//! [`DaemonState::manager_state`](crate::daemon::state::DaemonState::manager_state),
//! mirroring how the L2 proxy focus store is owned.
//! Test: `tests/manager_routes.rs` (real-HTTP contract, mirrors
//! `tests/proxy_routes.rs`), `tests/manager_routing.rs` (route-task, act, and the
//! chat propose→confirm suite) plus the in-module unit tests in each submodule.

pub mod act;
pub mod actuator;
pub mod chat;
pub mod chat_store;
pub mod digest;
pub mod inference;
pub mod memory;
pub mod proposal;
pub mod route_task;
pub mod state;
pub mod status;
pub mod version;

pub use act::{ActRequest, ActResponse, ProposedAction, execute_action, manager_act_route};
pub use actuator::{
    DaemonLauncher, LaunchOutcome, ManagerActuator, ProxyActuator, SessionLauncher,
    resolve_actuator,
};
pub use chat::{ChatReplyBody, ChatRequestBody, manager_chat_route};
pub use chat_store::{ChatStore, ChatTurn, TurnRole};
pub use digest::{DigestResponse, DigestScope, manager_digest_route};
pub use inference::{InferenceUnavailable, ManagerInference};
pub use memory::{PORTFOLIO_PALACE_ID, PortfolioPalace};
pub use proposal::{ProposalStore, extract_proposed_action, is_confirmation};
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
