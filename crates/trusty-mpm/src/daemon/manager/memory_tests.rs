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
    let palace = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
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
    let palace = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
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
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");

    // First startup: palace is created and available.
    let first = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
    assert!(first.is_available(), "first provision must be available");
    let mem = first.memory().expect("live memory under feature");
    assert_eq!(mem.persisted_palace_count().expect("count"), 1);
    assert_eq!(mem.palace_id().as_str(), PORTFOLIO_PALACE_ID);

    // Second startup against the same root: still exactly ONE palace.
    let second = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
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
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;

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

/// #8545: provisioning under a fenced home root is refused by a panic before
/// the registry creates any directory. The mock embedder is seeded so that,
/// were the fence missing, the test fails on the absent panic instead of
/// loading the ONNX model.
///
/// The probe targets the real armed root because the fence roots are a
/// process-wide `OnceLock` set before `main`; a temp root cannot stand in
/// without a production seam. A regression that drops the fence check leaves
/// `~/.trusty-mpm/tm-8545-fence-probe/` behind; delete it after fixing.
#[cfg(feature = "manager-memory")]
#[test]
#[should_panic(expected = "#8545")]
fn portfolio_palace_under_a_fenced_home_is_refused() {
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let root = crate::core::home_write_fence::armed_roots()
        .iter()
        .find(|r| r.ends_with(".trusty-mpm"))
        .expect("the lib binary fences ~/.trusty-mpm");
    let _ = PortfolioPalace::provision(&root.join("tm-8545-fence-probe"), PalaceAccess::Writer);
}

/// Under the feature, a remembered observation round-trips through recall —
/// proving the read-write wiring later phases (digest history, chat turns) use
/// is functional, not a stub.
#[cfg(feature = "manager-memory")]
#[tokio::test]
async fn portfolio_memory_remember_then_recall_round_trips() {
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
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

/// #9274 (PR3 critic HIGH 2), re-pointed by #9275: the daemon opens the
/// portfolio palace as writer, so an existing format-1 portfolio palace
/// migrates at open. Under read-only intent it is refused with
/// `FormatNeedsMigration`.
/// Test: this is the test.
#[cfg(feature = "manager-memory")]
#[test]
fn an_existing_format_1_portfolio_palace_migrates_at_open() {
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;
    use trusty_memory::engine::store::OpenIntent;
    use trusty_memory::engine::store::format_v1_fixture::write_format_1_palace;
    use trusty_memory::engine::store::palace_format::{MIGRATION_RECORD, PALACE_FORMAT_SUPPORTED};

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let data_root = dir.path().join("palace");
    let palace_dir = data_root.join(PORTFOLIO_PALACE_ID);
    write_format_1_palace(&palace_dir, PORTFOLIO_PALACE_ID).expect("format-1 palace");

    // #9274: the daemon opens the portfolio palace as writer, so it migrates.
    let mem = PortfolioMemory::open_with_intent(&data_root, OpenIntent::Writer)
        .expect("a format-1 portfolio palace opens");
    assert_eq!(mem.persisted_palace_count().expect("count"), 1);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(palace_dir.join("palace.json")).expect("read"))
            .expect("palace.json");
    assert_eq!(json["format_version"], PALACE_FORMAT_SUPPORTED, "{json}");
    assert!(
        !palace_dir.join(MIGRATION_RECORD).exists(),
        "migration left its record"
    );
}

/// The env var naming the framework root the #9659 portfolio daemon child
/// provisions as writer.
#[cfg(feature = "manager-memory")]
const PORTFOLIO_DAEMON_ROOT_ENV: &str = "TM_9659_DAEMON_PORTFOLIO_ROOT";

/// The portfolio palace's data root under a framework root.
#[cfg(feature = "manager-memory")]
fn portfolio_palace_dir(framework_root: &std::path::Path) -> std::path::PathBuf {
    framework_root
        .join(MANAGER_DATA_SUBDIR)
        .join("palace")
        .join(PORTFOLIO_PALACE_ID)
}

/// Child half of `sm_serve_first_then_daemon_gets_the_portfolio_writer_lock`.
///
/// What: returns at once unless `PORTFOLIO_DAEMON_ROOT_ENV` names a framework
/// root. Otherwise provisions the portfolio palace there as the daemon does
/// (writer) and asserts it is available, holds the live palace (not a
/// read-only snapshot), is at the supported format, and takes a `remember`.
/// Test: run by `sm_serve_first_then_daemon_gets_the_portfolio_writer_lock`.
#[cfg(feature = "manager-memory")]
#[test]
#[ignore = "child process of the #9659 portfolio sm-serve-first test"]
fn open_the_portfolio_palace_as_the_daemon_child() {
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;
    use trusty_memory::engine::store::palace_format::PALACE_FORMAT_SUPPORTED;

    let Some(root) = std::env::var_os(PORTFOLIO_DAEMON_ROOT_ENV).map(std::path::PathBuf::from)
    else {
        return;
    };
    seed_shared_embedder_with_mock();
    let daemon = PortfolioPalace::provision(&root, PalaceAccess::Writer);
    assert!(
        daemon.is_available(),
        "the daemon's writer provision of the portfolio palace is unavailable: {:?}",
        daemon.unavailable_reason()
    );
    let mem = daemon.memory().expect("live portfolio memory");
    let handle = mem
        .registry
        .get(&mem.palace_id)
        .expect("the daemon's portfolio palace handle");
    assert!(
        !handle.is_read_only(),
        "the daemon holds a read-only snapshot of the portfolio palace, not the live file"
    );
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(portfolio_palace_dir(&root).join("palace.json")).expect("read palace.json"),
    )
    .expect("palace.json");
    assert_eq!(json["format_version"], PALACE_FORMAT_SUPPORTED, "{json}");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime
        .block_on(mem.remember(
            "DAEMON_9659_PORTFOLIO_MARKER written by the daemon",
            vec!["9659".to_string()],
        ))
        .expect("remember as the daemon");
}

/// #9659: the portfolio analogue of `sm_serve_first_then_daemon_gets_the_writer_lock`.
/// With a format-1 portfolio palace (#9275) and `tm sm serve`'s read-only provision
/// held for the whole test, the daemon's writer provision (a separate process,
/// as in production) migrates the palace and `remember` lands.
/// Test: this is the test.
#[cfg(feature = "manager-memory")]
#[test]
fn sm_serve_first_then_daemon_gets_the_portfolio_writer_lock() {
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;
    use trusty_memory::engine::store::format_v1_fixture::write_format_1_palace;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    write_format_1_palace(&portfolio_palace_dir(dir.path()), PORTFOLIO_PALACE_ID)
        .expect("format-1 palace");

    let _serve = PortfolioPalace::provision(dir.path(), PalaceAccess::ReadOnly);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "daemon::manager::memory::tests::open_the_portfolio_palace_as_the_daemon_child",
            "--ignored",
            "--nocapture",
        ])
        .env(PORTFOLIO_DAEMON_ROOT_ENV, dir.path())
        .output()
        .expect("run the daemon child");
    assert!(
        out.status.success(),
        "the daemon's writer provision failed beside tm sm serve ({}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// #9659 Fail-Open Check: a writer provision that cannot take `kg.redb` must
/// say so in the unavailable reason, never come up on a read-only snapshot.
///
/// What: provisions a current-format portfolio palace and drops it, holds its
/// `kg.redb` with a raw `create_palace_db`, then provisions as the daemon does
/// (writer). Asserts the palace is unavailable AND carries a reason; a log
/// line alone does not reach `/manager/version`.
/// Test: this is the test.
#[cfg(feature = "manager-memory")]
#[test]
fn a_writer_provision_beside_a_kg_holder_is_unavailable_with_a_reason() {
    use trusty_memory::engine::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let first = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
        assert!(first.is_available(), "an uncontended writer provision");
    }
    let kg_path = portfolio_palace_dir(dir.path()).join("kg.redb");
    let _holder = trusty_common::redb_cache::create_palace_db(&kg_path)
        .expect("another holder takes kg.redb");

    let daemon = PortfolioPalace::provision(dir.path(), PalaceAccess::Writer);
    let read_only = daemon
        .memory()
        .and_then(|m| m.registry.get(&m.palace_id))
        .map(|h| h.is_read_only());
    assert!(
        !daemon.is_available(),
        "a writer provision beside a kg.redb holder came up available (read_only={read_only:?})"
    );
    assert!(
        daemon.unavailable_reason().is_some_and(|r| !r.is_empty()),
        "an unavailable portfolio palace must carry a reason, not only a log line"
    );
}
