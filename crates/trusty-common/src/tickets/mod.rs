//! Unified ticketing MCP server (GitHub / JIRA / Linear).
//!
//! Why: open-mpm and the rest of the trusty-* suite need a single MCP
//! surface that can talk to GitHub Issues, JIRA, and Linear without the
//! caller knowing which backend is configured. Absorbed from the former
//! standalone `trusty-tickets` crate (issue #5 phase consolidation).
//! What: Re-exports `api::*` plus the `server` and `tools` modules used
//! by the binary. Gated behind the `tickets` feature on `trusty-common`.
//! Test: each submodule has its own unit tests; config loading and the
//! MCP dispatch loop are covered by the module-level tests in `server`.

pub mod api;
pub mod server;
pub mod tools;
