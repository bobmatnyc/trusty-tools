//! trusty-search library crate.
//!
//! Why: Exposes the previously separate `trusty-search-core`, `-service`, and
//! `-mcp` sub-crate modules under a single library target so integration tests
//! (and downstream consumers) can reach the internal APIs after the workspace
//! was consolidated into one crate.
//! What: Re-publishes `core`, `service`, and `mcp` as `pub mod`s. The `main`
//! binary uses these via `crate::core::...`; integration tests use
//! `trusty_search::core::...`.
//! Test: `cargo build --lib` succeeds; `cargo test` runs integration tests
//! that import `trusty_search::core::*`.

pub mod allowlist;
pub mod config;
pub mod core;
pub mod mcp;
pub mod service;

// Why: surface the unified `rpc.discover` service descriptor at the crate
// root so open-mpm and other host processes can `use trusty_search::SearchMcpService`
// without traversing into the internal module layout (closes #115).
pub use service::SearchMcpService;
