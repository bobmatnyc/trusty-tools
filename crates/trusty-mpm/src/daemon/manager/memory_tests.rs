//! Tests for the portfolio manager palace provisioning (WI-5, #2582).
//!
//! Why: prove the DOC-36 §3.4 / §7 Q3 contract — the portfolio palace is
//! auto-provisioned idempotently at startup, is scoped to a single stable id,
//! degrades gracefully (never panics) when the memory engine is unavailable, and
//! offers a working remember→recall round-trip for later phases to build on. The
//! feature-gated tests point the engine at a `tempdir` (never `~/.trusty-mpm`)
//! and seed the MOCK embedder so they run without the ONNX model.
//! What: a default-build degrade assertion plus, under `manager-memory`,
//! idempotency and remember/recall round-trip assertions.

use super::*;

/// The palace id is stable regardless of build/availability — it is the DOC-36
/// §3.4 convention, not a runtime-derived value.
#[test]
fn portfolio_palace_exposes_stable_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = PortfolioPalace::provision(dir.path());
    assert_eq!(palace.id(), PORTFOLIO_PALACE_ID);
    assert_eq!(palace.id(), "tm-manager-portfolio");
}

/// In a default build (no `manager-memory` feature) the palace is constructed
/// but reports unavailable with an actionable reason — the daemon still starts,
/// the manager surface still works (DOC-36 §4 degrade bar).
#[cfg(not(feature = "manager-memory"))]
#[test]
fn portfolio_palace_reports_unavailable_without_feature() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = PortfolioPalace::provision(dir.path());
    assert!(!palace.is_available());
    let reason = palace
        .unavailable_reason()
        .expect("unavailable palace must name a reason");
    assert!(
        reason.contains("manager-memory"),
        "reason should point at the missing feature: {reason}"
    );
}

/// Under the feature, provisioning creates exactly one palace and is idempotent
/// across repeated startups against the same root (create-if-absent, never
/// clobber) — the headline §7 Q3 guarantee.
#[cfg(feature = "manager-memory")]
#[test]
fn portfolio_palace_provisions_and_is_idempotent() {
    use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");

    // First startup: palace is created and available.
    let first = PortfolioPalace::provision(dir.path());
    assert!(first.is_available(), "first provision must be available");
    let mem = first.memory().expect("live memory under feature");
    assert_eq!(mem.persisted_palace_count().expect("count"), 1);
    assert_eq!(mem.palace_id().as_str(), PORTFOLIO_PALACE_ID);

    // Second startup against the same root: still exactly ONE palace.
    let second = PortfolioPalace::provision(dir.path());
    assert!(second.is_available());
    assert_eq!(
        second
            .memory()
            .expect("live memory")
            .persisted_palace_count()
            .expect("count"),
        1,
        "provisioning twice must yield exactly ONE portfolio palace"
    );
}

/// Under the feature, a remembered observation round-trips through recall —
/// proving the read-write wiring later phases (digest history, chat turns) use
/// is functional, not a stub.
#[cfg(feature = "manager-memory")]
#[tokio::test]
async fn portfolio_memory_remember_then_recall_round_trips() {
    use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = PortfolioPalace::provision(dir.path());
    let mem = palace.memory().expect("live memory under feature");

    mem.remember(
        "Portfolio digest 2026-07-14: widget shipped, gizmo stalled on review.",
        vec!["digest".to_string()],
    )
    .await
    .expect("remember");

    let hits = mem
        .recall("what shipped in the portfolio")
        .await
        .expect("recall");
    assert!(
        !hits.is_empty(),
        "a remembered portfolio observation must be recallable"
    );
}
