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

/// Compute the tokio worker-thread count for this machine.
///
/// Why (issue #1006): raising the floor to 16 prevents accept-loop starvation
/// when embed-pool workers block on 30 s CoreML/CUDA sidecar calls; with only
/// `available_parallelism` workers (e.g. 8 on a 4-core box) and tasks blocking
/// on long embed calls, the axum accept loop starves.
/// What: returns `max(cpu_count, 16)`. The result is always `>= 16`.
/// Test: `worker_thread_count_at_least_16` in `tests_state.rs` — asserts the
/// floor with `cpu_count=1` (→ 16) and the pass-through with `cpu_count=32` (→ 32).
pub fn worker_thread_count(cpu_count: usize) -> usize {
    std::cmp::max(cpu_count, 16)
}

// Regression tests for persistence data-integrity fixes (#1088, #1089, #1090).
// Extracted from persistence.rs to keep that file under its line-cap budget.
#[cfg(test)]
#[path = "service/persistence_tests_1088.rs"]
mod persistence_tests_1088;
