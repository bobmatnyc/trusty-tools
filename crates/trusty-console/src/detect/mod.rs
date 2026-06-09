//! Concrete `ServiceConnector` implementations for trusty-search, trusty-memory,
//! trusty-analyze, and trusty-review.
//!
//! Why: P0 needs read-only detection only — reads discovery files written by
//! each daemon on bind, optionally probes the `/health` endpoint, and falls
//! back gracefully when the daemon or binary is absent.
//! What: Four structs (`SearchConnector`, `MemoryConnector`, `AnalyzeConnector`,
//! `ReviewConnector`) each implementing `ServiceConnector::detect()`. Each
//! uses the same detection sequence:
//! step 1 — does the binary exist on PATH? No → `Absent`.
//! step 2 — does the `http_addr` discovery file exist with a non-empty address?
//!          Yes → TCP probe + optional `/health` fetch → `Running` or `Available`.
//! step 3 — otherwise → `Available` (binary present, no daemon).
//! Test: Unit tests live in each submodule. They inject a fake `HOME` via the
//! `with_home` constructor so they never touch the real user's files. Run with
//! `cargo test -p trusty-console`.

mod analyze;
mod helpers;
mod memory;
mod review;
mod search;

pub use analyze::AnalyzeConnector;
pub use memory::MemoryConnector;
pub use review::ReviewConnector;
pub use search::SearchConnector;

use crate::connector::ServiceConnector;

/// Return all connectors in display order.
///
/// Why: Centralises the connector list so the server and any future CLI
/// command iterate the same set. P0 covers all four trusty daemons.
/// What: Returns a `Vec<Box<dyn ServiceConnector>>` with four P0 connectors:
/// search, memory, analyze, review.
/// Test: `test_all_connectors_returns_four` below.
pub fn all_connectors() -> Vec<Box<dyn ServiceConnector>> {
    vec![
        Box::new(SearchConnector::new()),
        Box::new(MemoryConnector::new()),
        Box::new(AnalyzeConnector::new()),
        Box::new(ReviewConnector::new()),
    ]
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the registry must return exactly four connectors in order (P0
    /// covers search, memory, analyze, review).
    /// What: calls all_connectors() and checks IDs.
    /// Test: this test itself.
    #[test]
    fn test_all_connectors_returns_four() {
        let cs = all_connectors();
        assert_eq!(cs.len(), 4);
        assert_eq!(cs[0].id(), "trusty-search");
        assert_eq!(cs[1].id(), "trusty-memory");
        assert_eq!(cs[2].id(), "trusty-analyze");
        assert_eq!(cs[3].id(), "trusty-review");
    }
}
