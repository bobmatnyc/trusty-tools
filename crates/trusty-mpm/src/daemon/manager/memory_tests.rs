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

/// Why (issue #4911): the portfolio `ensure_palace` carries the identical
/// `if let Ok(handle) = open_palace(..) { return }` shape as the SM one, so a
/// palace that exists but cannot be READ fell through to `create_palace`, which
/// rewrites `palace.json` and destroys `created_at` before re-failing against
/// the same unreadable store. Two files, one defect — this is the second call
/// site, covered independently so a fix to one cannot be mistaken for both.
/// What: seeds a present-but-unopenable portfolio palace on disk — a valid
/// `palace.json` plus a DIRECTORY where the KG store file (`kg.redb`) belongs,
/// which fails to open at the OS layer on every platform and redb version.
/// Building it by hand rather than provisioning-then-corrupting matters: the
/// store keeps a process-wide cache of open databases keyed by path, so a palace
/// this process already opened would be served from cache and never read the
/// corruption. Asserts the open fails AND that `palace.json` is byte-identical —
/// reaching `create_palace` rewrites it with a fresh `created_at`, so this cannot
/// pass while that call happens.
/// Test: this is the test.
#[cfg(feature = "manager-memory")]
#[test]
fn portfolio_ensure_palace_never_rewrites_metadata_of_a_present_but_unopenable_palace() {
    use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let data_root = dir.path().join("palace");
    let palace_dir = data_root.join(PORTFOLIO_PALACE_ID);
    std::fs::create_dir_all(&palace_dir).expect("create palace dir");

    let metadata = palace_dir.join("palace.json");
    std::fs::write(
        &metadata,
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": PORTFOLIO_PALACE_ID,
            "name": PORTFOLIO_PALACE_ID,
            "description": serde_json::Value::Null,
            "created_at": "2020-01-02T03:04:05Z",
            "data_dir": palace_dir,
            "schema_version": 1,
        }))
        .expect("serialise palace.json"),
    )
    .expect("write palace.json");
    std::fs::create_dir(palace_dir.join("kg.redb")).expect("directory where the KG store belongs");

    let metadata_before = std::fs::read(&metadata).expect("read palace.json");
    let before: serde_json::Value =
        serde_json::from_slice(&metadata_before).expect("parse palace.json");

    // `PortfolioMemory` is not `Debug`, so `expect_err` is unavailable here.
    let err = match PortfolioMemory::open(data_root) {
        Ok(_) => {
            panic!("opening a present-but-unopenable palace must fail, not silently recreate it")
        }
        Err(e) => e,
    };

    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metadata).expect("read palace.json"))
            .expect("parse palace.json");
    assert_eq!(
        before["created_at"], after["created_at"],
        "created_at was rewritten, so `ensure_palace` fell through to \
         `create_palace` for a palace that exists but could not be read; the \
         open error was: {err}"
    );
    assert_eq!(
        metadata_before,
        std::fs::read(&metadata).expect("read palace.json"),
        "palace.json must be byte-identical — any rewrite means `create_palace` ran"
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
