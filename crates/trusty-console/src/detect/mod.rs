//! Concrete `ServiceConnector` implementations for trusty-search, trusty-memory,
//! trusty-analyze, and trusty-review.
//!
//! Why: P0 needs read-only detection only — reads discovery files written by
//! each daemon on bind, optionally probes the `/health` endpoint, and falls
//! back gracefully when the daemon or binary is absent.
//! Issue #1163 lifts the prior #1069 exclusion of trusty-review now that the
//! Review dashboard tab is implemented with a full `console_metrics` tool.
//! What: Four structs (`SearchConnector`, `MemoryConnector`, `AnalyzeConnector`,
//! `ReviewConnector`) each implementing `ServiceConnector::detect()`. Each uses
//! the same detection sequence:
//! step 1 — does the binary exist on PATH? No → `Absent`.
//! step 2 — does the `http_addr` discovery file exist with a non-empty address?
//!          Yes → TCP probe + optional `/health` fetch → `Running` or `Available`.
//! step 3 — otherwise → `Available` (binary present, no daemon).
//! Test: Unit tests live in each submodule. They inject a fake `HOME` via the
//! `with_home` constructor so they never touch the real user's files. Run with
//! `cargo test -p trusty-console`.

mod agents;
mod analyze;
mod helpers;
mod memory;
mod mpm;
mod review;
mod search;

pub use agents::AgentsConnector;
pub use analyze::AnalyzeConnector;
pub use memory::MemoryConnector;
pub use mpm::MpmConnector;
pub use review::ReviewConnector;
pub use search::SearchConnector;

use crate::connector::ServiceConnector;

/// Process-wide lock serialising every test that mutates the global
/// `TRUSTY_DATA_DIR_OVERRIDE` env var.
///
/// Why: both the mpm and agents connector tests point `resolve_data_dir` at a
/// tempdir via that ONE process-global env var. Per-module locks do NOT
/// serialise across modules, so two such tests in different modules could
/// clobber each other's override and race (#3331 regression: the agents tests
/// racing the mpm lock-file test). A single shared lock in the common parent
/// module serialises them all.
/// What: a `std::sync::Mutex<()>` locked by every env-mutating connector test.
/// Test: used by `agents::tests` and `mpm::tests`; not itself a test.
#[cfg(test)]
pub(super) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Return all connectors in display order.
///
/// Why: Centralises the connector list so the server and any future CLI
/// command iterate the same set. Issue #1163 lifts the prior #1069 exclusion
/// of trusty-review: the Review dashboard tab is now fully implemented with a
/// `console_metrics` MCP tool, so the service is included in the Overview.
/// What: Returns a `Vec<Box<dyn ServiceConnector>>` with six connectors:
/// search, memory, analyze, review, mpm (#1222 adds trusty-mpm for the Sessions
/// tab), agents (#3331 adds trusty-agents so `/api/agents/*` resolves an
/// upstream under the loopback-only doctrine).
/// Test: `test_all_connectors_returns_six` below.
pub fn all_connectors() -> Vec<Box<dyn ServiceConnector>> {
    vec![
        Box::new(SearchConnector::new()),
        Box::new(MemoryConnector::new()),
        Box::new(AnalyzeConnector::new()),
        Box::new(ReviewConnector::new()),
        Box::new(MpmConnector::new()),
        Box::new(AgentsConnector::new()),
    ]
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the registry must return exactly six connectors in order (search,
    /// memory, analyze, review, mpm — #1222; agents — #3331 for the
    /// `/api/agents/*` proxy under the loopback-only doctrine).
    /// What: calls all_connectors() and checks IDs.
    /// Test: this test itself.
    #[test]
    fn test_all_connectors_returns_six() {
        let cs = all_connectors();
        assert_eq!(cs.len(), 6);
        assert_eq!(cs[0].id(), "trusty-search");
        assert_eq!(cs[1].id(), "trusty-memory");
        assert_eq!(cs[2].id(), "trusty-analyze");
        assert_eq!(cs[3].id(), "trusty-review");
        assert_eq!(cs[4].id(), "trusty-mpm");
        assert_eq!(cs[5].id(), "trusty-agents");
    }
}
