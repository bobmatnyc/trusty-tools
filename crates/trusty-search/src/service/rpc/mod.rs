//! The JSON-RPC methods the daemon serves on its Unix socket (#6285).
//!
//! Why: `service::socket` owns the transport — bind, peer check, framing,
//! accept loop — and nothing about which methods exist. This module is the
//! other half, split by route family so the slices that move the remaining
//! `service::server` surface across can each add one file rather than editing
//! one.
//!
//! What: a module list. Every family registers itself into the router
//! `socket::build_router` assembles.
//!
//! Test: each family's own `*_tests.rs`.

/// One HTTP refusal becomes one JSON-RPC error frame.
pub mod error;

/// Registration for the read families: indexes, status, config, chunks, graph,
/// and call chain (#6285 slice 2).
pub mod reads;

/// Registration for the query families: hybrid search and its fan-out, grep and
/// its fan-out, code-to-code similarity, and typeahead (#6285 slice 3).
pub mod queries;
