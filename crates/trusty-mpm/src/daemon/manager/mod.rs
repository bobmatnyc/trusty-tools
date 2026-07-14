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

pub mod memory;
pub mod state;
pub mod status;
pub mod version;

pub use memory::{PORTFOLIO_PALACE_ID, PortfolioPalace};
pub use state::ManagerState;
pub use status::{
    PortfolioStatusResponse, PortfolioTotals, aggregate_portfolio_status, manager_status_route,
};
pub use version::{
    ManagerEndpoint, ManagerPalaceStatus, ManagerVersionResponse, manager_version_route,
};
