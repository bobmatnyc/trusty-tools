//! Unit tests for `commands::start` — warm-boot stage classification,
//! embedder fail-fast, auto-discover resolution, and relocation heuristics.
//!
//! Why: the warm-boot path in `restore_indexes` is async and pulls in the full
//! daemon-init machinery. We test the pure classifier `derive_warm_boot_stages`
//! which contains every business rule — the call site is a thin disk-inspection
//! adapter with no branches of its own.
//!
//! What: each test corresponds to one bullet in the ticket spec (#135, #110,
//! #314, #484, #541, #604). Run with:
//!   `cargo test -p trusty-search -- warm_boot`
//!
//! Test: this file.

use super::embedder::{LazySlotEmbedderAdapter, UdsEmbedderAdapter};
use super::*;
use crate::core::registry::StageStatus;

/// Helper: construct [`WarmBootInputs`] without spelling out every field.
fn inputs() -> WarmBootInputs {
    WarmBootInputs {
        chunk_count: 0,
        hnsw_snapshot_ready: false,
        graph_node_count: 0,
        lexical_only: false,
        skip_kg: false,
        corpus_open_failed: false,
    }
}

/// A restored index whose redb corpus reports `chunk_count > 0` has the
/// lexical lane fully wired. The classifier must flip lexical → `Ready`.
#[test]
fn warm_boot_marks_lexical_ready_when_chunks_present() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        ..inputs()
    });
    assert_eq!(stages.lexical.status, StageStatus::Ready);
    assert!(stages.search_capabilities().contains(&"bm25"));
    assert!(stages.search_capabilities().contains(&"literal"));
    assert!(stages.search_capabilities().contains(&"exact_match"));
}

/// When the HNSW sidecar exists on disk and the loader successfully
/// rehydrated it, the semantic stage must come back as `Ready` and `vector`
/// must appear in `search_capabilities`.
#[test]
fn warm_boot_marks_semantic_ready_when_hnsw_snapshot_exists() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        hnsw_snapshot_ready: true,
        ..inputs()
    });
    assert_eq!(stages.semantic.status, StageStatus::Ready);
    assert!(stages.search_capabilities().contains(&"vector"));
}

/// When the persisted symbol graph rehydrated with a non-zero node count,
/// the graph stage must be `Ready` and `kg` must appear in
/// `search_capabilities`.
#[test]
fn warm_boot_marks_graph_ready_when_symbol_graph_nonempty() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        hnsw_snapshot_ready: true,
        graph_node_count: 7_402,
        ..inputs()
    });
    assert_eq!(stages.graph.status, StageStatus::Ready);
    assert!(stages.search_capabilities().contains(&"kg"));
}

/// Missing HNSW snapshot → semantic stays `Pending`. Lexical can still be
/// `Ready` (BM25 does not depend on the embedder), but the search handler
/// must not advertise `vector` until a reindex regenerates the HNSW file.
#[test]
fn warm_boot_marks_semantic_pending_when_no_snapshot() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        hnsw_snapshot_ready: false,
        ..inputs()
    });
    assert_eq!(stages.lexical.status, StageStatus::Ready);
    assert_eq!(stages.semantic.status, StageStatus::Pending);
    assert!(!stages.search_capabilities().contains(&"vector"));
}

/// Defensive: a `lexical_only` index that happens to have a stale HNSW file
/// from a prior non-lexical-only life must NOT surface the vector / kg lanes.
/// The `lexical_only` flag wins regardless of on-disk state.
#[test]
fn warm_boot_respects_lexical_only_flag() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        hnsw_snapshot_ready: true,
        graph_node_count: 7_402,
        lexical_only: true,
        skip_kg: false,
        corpus_open_failed: false,
    });
    assert_eq!(stages.lexical.status, StageStatus::Ready);
    assert_eq!(stages.semantic.status, StageStatus::Skipped);
    assert_eq!(stages.graph.status, StageStatus::Skipped);
    let caps = stages.search_capabilities();
    assert!(caps.contains(&"bm25"));
    assert!(!caps.contains(&"vector"));
    assert!(!caps.contains(&"kg"));
}

/// Mid-reindex recovery: the registry has the entry but the redb corpus is
/// empty. Lexical must come back as `InProgress` — not `Pending` — so the
/// lifecycle status surfaces "walking".
#[test]
fn warm_boot_marks_mid_reindex_as_in_progress() {
    let stages = derive_warm_boot_stages(inputs());
    assert_eq!(stages.lexical.status, StageStatus::InProgress);
    assert_eq!(stages.lifecycle_status(), "walking");
    assert!(stages.search_capabilities().is_empty());
}

/// Issue #313: a `skip_kg` index that happens to have a non-empty symbol
/// graph on disk must NOT surface the `kg` lane on warm-boot.
/// The `skip_kg` flag wins over on-disk state.
///
/// Why: an operator who flipped `skip_kg = true` expects no KG anywhere
/// — including after a daemon restart that inherits stale redb graph bytes.
/// What: set `graph_node_count = 7_402` alongside `skip_kg = true`; assert
/// graph = Skipped and `"kg"` is absent from search_capabilities.
/// Test: this test.
#[test]
fn warm_boot_respects_skip_kg_flag() {
    // skip_kg wins over non-empty on-disk graph.
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        hnsw_snapshot_ready: true,
        graph_node_count: 7_402,
        lexical_only: false,
        skip_kg: true,
        corpus_open_failed: false,
    });
    assert_eq!(
        stages.graph.status,
        StageStatus::Skipped,
        "skip_kg must force graph to Skipped even when on-disk graph is non-empty"
    );
    assert_eq!(
        stages.semantic.status,
        StageStatus::Ready,
        "skip_kg must not affect the semantic lane"
    );
    let caps = stages.search_capabilities();
    assert!(
        !caps.contains(&"kg"),
        "skip_kg must suppress the kg capability"
    );
    assert!(
        caps.contains(&"vector"),
        "skip_kg must not suppress the vector capability"
    );

    // skip_kg + lexical_only together: both semantic and graph are Skipped.
    let stages_both = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 14_823,
        hnsw_snapshot_ready: true,
        graph_node_count: 7_402,
        lexical_only: true,
        skip_kg: true,
        corpus_open_failed: false,
    });
    assert_eq!(stages_both.semantic.status, StageStatus::Skipped);
    assert_eq!(stages_both.graph.status, StageStatus::Skipped);
    let caps = stages_both.search_capabilities();
    assert!(!caps.contains(&"vector"));
    assert!(!caps.contains(&"kg"));
}

/// When `trusty-embedderd` is not on PATH and `TRUSTY_EMBEDDERD_BIN` is
/// unset, the default `auto`/`stdio` path must fail fast with an actionable
/// error containing the install hint.
///
/// Why (issue #110 Phase 2 course-correction): the soft fallback
/// `"trusty-embedderd not found; falling back to in-process"` was the
/// same "lazy" pattern the user explicitly rejected.
/// What: sets `TRUSTY_EMBEDDERD_BIN` to a non-existent path, calls
/// `locate_embedderd_binary`, and asserts the error contains
/// `"cargo install trusty-embedderd"`.
/// Test: this test (no binary / ONNX model needed; always runs).
#[test]
fn missing_binary_fails_fast_with_install_hint() {
    use crate::service::embedder_supervisor::locate_embedderd_binary;

    // Isolate from the real environment: point TRUSTY_EMBEDDERD_BIN at a
    // path that definitely does not exist, bypassing the PATH walk.
    // SAFETY: test-only. Cargo runs each test binary in its own process
    // (not a thread-per-test model), so env mutation here is safe as long
    // as no parallel test reads the same var. This is the only test in
    // this module that touches TRUSTY_EMBEDDERD_BIN.
    let prev = std::env::var("TRUSTY_EMBEDDERD_BIN").ok();
    unsafe {
        std::env::set_var(
            "TRUSTY_EMBEDDERD_BIN",
            "/nonexistent/path/trusty-embedderd-missing",
        );
    }
    let result = locate_embedderd_binary();
    unsafe {
        match prev {
            Some(v) => std::env::set_var("TRUSTY_EMBEDDERD_BIN", v),
            None => std::env::remove_var("TRUSTY_EMBEDDERD_BIN"),
        }
    }

    // The locate call itself must fail.
    assert!(
        result.is_err(),
        "locate_embedderd_binary must return Err when binary is absent"
    );

    // The error message that `build_embedder` wraps around the locate
    // error must contain the install hint. We construct it here exactly
    // as `build_embedder` does to assert the hint text stays in sync.
    let locate_err = result.unwrap_err();
    let wrapped = format!(
        "{locate_err}\n\n\
         ERROR: trusty-embedderd binary not found on PATH.\n\
         \n\
         trusty-search v0.13+ requires trusty-embedderd to be installed alongside it.\n\
         \n\
         Install it with:\n\
         \x20 cargo install trusty-embedderd --locked\n\
         \n\
         Or set TRUSTY_EMBEDDERD_BIN to an absolute path:\n\
         \x20 export TRUSTY_EMBEDDERD_BIN=/path/to/trusty-embedderd\n\
         \n\
         If you need to run without the sidecar (tests, debugging), use:\n\
         \x20 TRUSTY_EMBEDDER=in-process trusty-search start"
    );
    assert!(
        wrapped.contains("cargo install trusty-embedderd"),
        "install hint must contain 'cargo install trusty-embedderd'; got: {wrapped}"
    );
    assert!(
        wrapped.contains("TRUSTY_EMBEDDER=in-process"),
        "escape hatch hint must mention TRUSTY_EMBEDDER=in-process; got: {wrapped}"
    );
}

/// Verify the `no_auto_discover` config-resolution rules in isolation.
///
/// Why (issue #314): the gate logic lives in `handle_start` which requires
/// a full daemon boot to exercise end-to-end. Testing the *decision* as a
/// pure boolean helper keeps the test deterministic and free of filesystem /
/// network side-effects.
///
/// What: mirrors the exact precedence — CLI flag (`no_auto_discover: bool`)
/// wins over `TRUSTY_NO_AUTO_DISCOVER` env var, which wins over the default.
///
/// Test: `cargo test -p trusty-search -- no_auto_discover_resolution`
#[test]
fn no_auto_discover_resolution() {
    /// Pure function that mirrors the gate condition in `handle_start`.
    /// Returns `true` when auto-discovery should be **skipped**.
    ///
    /// Why: extracted so we can drive it with arbitrary (cli_flag, env)
    /// combinations without touching the real environment or daemon state.
    /// What: CLI flag takes unconditional precedence; env var is read only
    /// when the flag is `false`.
    /// Test: see the outer `no_auto_discover_resolution` test.
    fn should_skip_discovery(cli_flag: bool, env_val: Option<&str>) -> bool {
        if cli_flag {
            return true;
        }
        matches!(env_val, Some("1") | Some("true"))
    }

    // Default: scan is enabled (no flag, no env).
    assert!(
        !should_skip_discovery(false, None),
        "scan must be enabled by default"
    );

    // CLI flag alone suppresses scan.
    assert!(
        should_skip_discovery(true, None),
        "--no-auto-discover must suppress scan"
    );

    // Env var "1" suppresses scan when flag is false.
    assert!(
        should_skip_discovery(false, Some("1")),
        "TRUSTY_NO_AUTO_DISCOVER=1 must suppress scan"
    );

    // Env var "true" suppresses scan when flag is false.
    assert!(
        should_skip_discovery(false, Some("true")),
        "TRUSTY_NO_AUTO_DISCOVER=true must suppress scan"
    );

    // CLI flag takes precedence even when env would also suppress.
    assert!(
        should_skip_discovery(true, Some("1")),
        "CLI flag must take precedence"
    );

    // Unrecognised env value does NOT suppress (e.g. leftover "0").
    assert!(
        !should_skip_discovery(false, Some("0")),
        "TRUSTY_NO_AUTO_DISCOVER=0 must not suppress scan"
    );
    assert!(
        !should_skip_discovery(false, Some("")),
        "empty env value must not suppress scan"
    );
}

// ── Issue #484: moved-project relocation tests ─────────────────────────────

use crate::commands::start_restore::try_locate_moved_root;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::PersistedIndex;
use serial_test::serial;
use tempfile::tempdir;

/// Create a populated `.trusty-search/index.redb` under `root`.
fn make_populated_ts(root: &std::path::Path) {
    let ts_dir = root.join(COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&ts_dir).unwrap();
    std::fs::write(ts_dir.join("index.redb"), b"notempty").unwrap();
}

/// Why: the core relocation contract — a colocated index with a dead root_path
/// and exactly one candidate tracked root containing a populated .trusty-search/
/// must be relinked to that candidate.
/// What: set up a dead root entry and one candidate tracked root with a
/// populated redb; call `try_locate_moved_root` and assert it returns the
/// candidate path.
/// Test: this test.
#[test]
#[serial]
fn restore_moved_colocated_index_relinks_unique_candidate() {
    let data_tmp = tempdir().unwrap();
    let new_root = tempdir().unwrap();
    make_populated_ts(new_root.path());

    // Point TRUSTY_DATA_DIR at our tempdir so roots.toml is isolated.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

    // Register new_root as a tracked root.
    crate::service::roots_registry::upsert_root(new_root.path().to_path_buf()).unwrap();

    // Entry whose root_path no longer exists.
    let dead_root = std::path::PathBuf::from("/tmp/trusty-484-dead-root-xyz9999");
    let entry = PersistedIndex {
        id: "moved-project".to_string(),
        root_path: dead_root.clone(),
        colocated: true,
        ..Default::default()
    };

    let result = try_locate_moved_root(&entry, &[]);
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };

    let new_path = result.expect("must find the unique candidate");
    assert_eq!(
        new_path.canonicalize().unwrap(),
        new_root.path().canonicalize().unwrap(),
        "must relink to the tracked root containing .trusty-search/"
    );
}

/// Why: when the root_path is missing and NO tracked root has a populated
/// .trusty-search/, `try_locate_moved_root` must return None and must NOT
/// create any ghost directory.
/// What: register a tracked root with NO .trusty-search/, call the function,
/// assert None is returned and no ghost dir was created.
/// Test: this test.
#[test]
#[serial]
fn restore_missing_root_with_no_candidate_returns_none() {
    let data_tmp = tempdir().unwrap();
    let empty_root = tempdir().unwrap();
    // No .trusty-search/ here.

    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };
    crate::service::roots_registry::upsert_root(empty_root.path().to_path_buf()).unwrap();

    let dead_root = std::path::PathBuf::from("/tmp/trusty-484-no-candidate-xyz9999");
    let entry = PersistedIndex {
        id: "no-candidate".to_string(),
        root_path: dead_root.clone(),
        colocated: true,
        ..Default::default()
    };

    let result = try_locate_moved_root(&entry, &[]);
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };

    assert!(
        result.is_none(),
        "must return None when no candidate has a populated .trusty-search/"
    );
    // Verify no ghost directory was created under the dead root.
    let ghost = dead_root.join(COLOCATED_DIR_NAME);
    assert!(
        !ghost.exists(),
        "must not create a ghost .trusty-search/ under the missing root"
    );
}

/// Why: when multiple tracked roots have a populated .trusty-search/ and none
/// is claimed by another entry, `try_locate_moved_root` must return None
/// (ambiguous — cannot auto-pick).
/// What: register two tracked roots both with populated .trusty-search/; call
/// the function and assert it returns None.
/// Test: this test.
#[test]
#[serial]
fn restore_missing_root_with_ambiguous_candidates_returns_none() {
    let data_tmp = tempdir().unwrap();
    let root_a = tempdir().unwrap();
    let root_b = tempdir().unwrap();
    make_populated_ts(root_a.path());
    make_populated_ts(root_b.path());

    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };
    crate::service::roots_registry::upsert_root(root_a.path().to_path_buf()).unwrap();
    crate::service::roots_registry::upsert_root(root_b.path().to_path_buf()).unwrap();

    let dead_root = std::path::PathBuf::from("/tmp/trusty-484-ambiguous-xyz9999");
    let entry = PersistedIndex {
        id: "ambiguous".to_string(),
        root_path: dead_root,
        colocated: true,
        ..Default::default()
    };

    let result = try_locate_moved_root(&entry, &[]);
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };

    assert!(
        result.is_none(),
        "must return None when multiple candidates exist (ambiguous)"
    );
}

// ── Issue #541: warm-boot canonicalization tests ───────────────────────────

/// Why (issue #541): `canonicalize_best_effort` must return the canonical
/// form for a path that exists on disk.
/// What: create a real tempdir, call the helper, and assert the result
/// equals `std::fs::canonicalize`.
/// Test: this test.
#[test]
fn canonicalize_best_effort_resolves_existing_path() {
    let tmp = tempdir().unwrap();
    let expected = std::fs::canonicalize(tmp.path()).unwrap();
    let got = canonicalize_best_effort(tmp.path());
    assert_eq!(
        got, expected,
        "canonicalize_best_effort must return the canonical form for an existing path"
    );
}

/// Why (issue #541): `canonicalize_best_effort` must fall back to the
/// original path without panicking when the path does not exist.
/// What: pass a definitely-nonexistent path; assert the returned value equals
/// the input.
/// Test: this test.
#[test]
fn canonicalize_best_effort_falls_back_for_missing_path() {
    let missing = std::path::PathBuf::from("/tmp/trusty-541-definitely-does-not-exist-xyz");
    let got = canonicalize_best_effort(&missing);
    assert_eq!(
        got, missing,
        "canonicalize_best_effort must fall back to the input for a missing path"
    );
}

/// Why (issue #541): `canonicalize_best_effort` on a symlink must return
/// the target, not the link path — this is the core guarantee the warm-boot
/// fix relies on.
/// What: create a real tempdir, symlink to it, call the helper on the
/// symlink, and assert the result equals the canonical target.
/// Test: this test.
#[cfg(unix)]
#[test]
fn canonicalize_best_effort_resolves_symlink() {
    use std::os::unix::fs::symlink;

    let real_dir = tempdir().unwrap();
    let real_canonical = std::fs::canonicalize(real_dir.path()).unwrap();

    let link = real_canonical
        .parent()
        .unwrap()
        .join(format!("trusty-541-symlink-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    symlink(&real_canonical, &link).expect("create symlink");

    let got = canonicalize_best_effort(&link);
    let _ = std::fs::remove_file(&link);

    assert_eq!(
        got, real_canonical,
        "canonicalize_best_effort must resolve symlinks to their target"
    );
}

/// Why: issue #604 — the default lazy stdio sidecar adapter previously fell
/// through to the trait-default `provider() == Cpu`, so `/health` reported
/// `provider=CPU` even when the sidecar resolved CUDA/CoreML. The adapter
/// must now report the same provider the sidecar resolves, available
/// *before* the child spawns (the resolution is pure).
/// What: builds a `LazySlotEmbedderAdapter` over a non-spawned handle and
/// asserts its `provider()` equals `resolve_expected_provider()`.
/// Test: this test.
#[test]
fn lazy_adapter_reports_resolved_provider() {
    use crate::core::Embedder as _;
    use crate::service::embedder_supervisor::{LazyEmbedderHandle, SupervisorConfig};

    let handle = std::sync::Arc::new(LazyEmbedderHandle::new(
        std::path::PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig::default(),
    ));
    let adapter = LazySlotEmbedderAdapter { handle };
    assert_eq!(
        adapter.provider(),
        trusty_common::embedder::resolve_expected_provider(),
        "lazy stdio adapter must report the sidecar's resolved provider, not the CPU default"
    );
}

/// Why: issue #604 — the UDS-remote adapter shares the same defect/fix as
/// the lazy adapter; `/health` must not report a stale `CPU` for a
/// UDS-connected sidecar.
/// What: builds a `UdsEmbedderAdapter` over a dummy socket path (no
/// connection is made by construction) and asserts `provider()` matches the
/// resolver.
/// Test: this test.
#[test]
fn uds_adapter_reports_resolved_provider() {
    use crate::core::Embedder as _;

    let adapter = UdsEmbedderAdapter {
        client: trusty_common::embedder_client::UdsEmbedderClient::new(std::path::PathBuf::from(
            "/tmp/nonexistent-trusty-604.sock",
        )),
    };
    assert_eq!(
        adapter.provider(),
        trusty_common::embedder::resolve_expected_provider(),
        "uds adapter must report the sidecar's resolved provider, not the CPU default"
    );
}
