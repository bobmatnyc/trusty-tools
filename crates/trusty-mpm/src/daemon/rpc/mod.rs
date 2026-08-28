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

/// Registration for the legacy session registry, hook ingestion, and the polled
/// event feeds (#6288 slice 3). The SSE stream legs stay on HTTP until slice 6.
pub mod sessions_legacy;

/// The transport-neutral bodies `sessions_legacy`'s methods and `daemon::api`'s
/// HTTP handlers both call, so one route has one implementation.
pub mod sessions_legacy_ops;

/// Registration for the managed-session lifecycle, the SESSCTL control plane,
/// and the L2 proxy (#6288 slice 4).
pub mod managed;
