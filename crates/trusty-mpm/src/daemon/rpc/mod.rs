//! The JSON-RPC methods the daemon serves on its Unix socket (#6288).
//!
//! Why: `daemon::socket` owns the transport — bind, peer check, framing, accept
//! loop — and nothing about which methods exist. This module is the other half,
//! split by route family so the slices that move the remaining ~35k SLOC of
//! HTTP surface across can each add one file rather than editing one.
//!
//! What: a module list. Every family registers itself into the router
//! `socket::build_router` assembles.
//!
//! Test: each family's own `*_tests.rs`.

/// Registration for the core request/response families: health, doctor,
/// errors, report-bug, breakers, optimizer, overseer, llm-chat, tmux, and
/// claude-config (#6288 slice 2).
pub mod core;

/// The transport-neutral bodies `core`'s methods and `daemon::api`'s HTTP
/// handlers both call, so one route has one implementation.
pub mod core_ops;

/// #7968: the bounded-age cache `core_ops::health` serves the catalog-staleness
/// report from, so `/health` never walks the catalog inline per request.
pub(crate) mod health_catalog;

/// Registration for the legacy session registry, hook ingestion, and the polled
/// event feeds (#6288 slice 3). The SSE stream legs stay on HTTP until slice 6.
pub mod sessions_legacy;

/// The transport-neutral bodies `sessions_legacy`'s methods and `daemon::api`'s
/// HTTP handlers both call, so one route has one implementation.
pub mod sessions_legacy_ops;

/// Registration for the managed-session lifecycle, the SESSCTL control plane,
/// and the L2 proxy (#6288 slice 4).
pub mod managed;

/// Registration for the record-keeping families: registry-B projects,
/// deliverables/milestones, the L3 manager, the peer bus, pairing, and the
/// delegation query (#6288 slice 5).
pub mod registry;

/// #6288 step 1: the build-lease decision log, the retired builder-slot routes,
/// and adopt-worktree — the HTTP-only routes the sandboxed `tm` CLI reaches.
pub mod cli_socket;

/// #6288 step 2a: MCP dispatch, the socket form of `POST /rpc`.
pub mod mcp;

/// #6288 client cutover: the egress verdict and the managed file view.
pub mod client_cutover;

/// #6288 step 2a: the coordinator's context snapshot and chat turn.
pub mod coordinator;

/// #9758: the PM ledger writer, `mpm.dbatg.init`, `batch` and `show`.
pub mod dbatg;

// #10063: the dashboard methods' socket tests, ahead of the methods.
#[cfg(test)]
#[path = "architect_dashboard_tests.rs"]
mod architect_dashboard_tests;
