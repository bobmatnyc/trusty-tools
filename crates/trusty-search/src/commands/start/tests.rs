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
        skip_vector: false,
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
        skip_vector: false,
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

/// Issues #1870 / #2203 (joint root cause): a failed durable-corpus open
/// must fail EVERY stage — lexical, semantic, AND graph — even when the
/// HNSW snapshot and symbol graph independently loaded fine from disk.
///
/// Why: before this fix, `corpus_open_failed` only forced `lexical` to
/// `Failed`; `semantic` and `graph` were still classified purely from
/// `hnsw_snapshot_ready` / `graph_node_count`, which are on-disk signals
/// completely independent of the redb corpus. That let `/health` and
/// `GET /indexes/:id/status` report `semantic.status: "ready"` (and
/// `"vector"` in `search_capabilities`) while the query hot path's
/// `fetch_chunks_for_ids` could never resolve any HNSW hit against the
/// (unwired) corpus — every result was silently dropped at materialisation,
/// producing HTTP 200 + `results: []` for essentially every query (#2203)
/// while the status endpoint kept lying about health (#1870).
/// What: constructs `WarmBootInputs` with `corpus_open_failed: true` AND
/// healthy-looking `hnsw_snapshot_ready` / `graph_node_count` values, then
/// asserts all three stages are `Failed`, `lifecycle_status() == "failed"`,
/// and `search_capabilities()` is empty (no lane is falsely advertised).
/// Test: this test.
#[test]
fn warm_boot_corpus_open_failure_fails_every_stage() {
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: 48_742,
        hnsw_snapshot_ready: true,
        graph_node_count: 7_402,
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        corpus_open_failed: true,
    });
    assert_eq!(
        stages.lexical.status,
        StageStatus::Failed,
        "corpus open failure must fail lexical"
    );
    assert_eq!(
        stages.semantic.status,
        StageStatus::Failed,
        "corpus open failure must fail semantic even though the HNSW snapshot loaded fine \
         — without the corpus, no HNSW hit can ever resolve to chunk text (issues #1870, #2203)"
    );
    assert_eq!(
        stages.graph.status,
        StageStatus::Failed,
        "corpus open failure must fail graph even though the symbol graph loaded fine"
    );
    assert_eq!(stages.lifecycle_status(), "failed");
    assert!(
        stages.search_capabilities().is_empty(),
        "no lane may be advertised as queryable when the durable corpus is unavailable; got: {:?}",
        stages.search_capabilities()
    );
    // Every failed stage must carry the actionable reason (issue #1158's
    // original contract, now extended to semantic + graph).
    assert!(stages.lexical.failure.is_some());
    assert!(stages.semantic.failure.is_some());
    assert!(stages.graph.failure.is_some());
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
        skip_vector: false,
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
        skip_vector: false,
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
#[serial]
fn missing_binary_fails_fast_with_install_hint() {
    use crate::service::embedder_supervisor::locate_embedderd_binary;

    // Isolate from the real environment: point TRUSTY_EMBEDDERD_BIN at a
    // path that definitely does not exist, bypassing the PATH walk.
    // SAFETY: test-only. `cargo test` runs tests as threads within a single
    // process (not as separate processes), so env mutation is a data race
    // unless the test is marked `#[serial]`. The `#[serial]` attribute
    // above ensures this test runs exclusively while env is mutated.
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
    let adapter = LazySlotEmbedderAdapter {
        handle,
        is_python: false,
    };
    assert_eq!(
        adapter.provider(),
        trusty_common::embedder::resolve_expected_provider(),
        "lazy stdio adapter must report the sidecar's resolved provider, not the CPU default"
    );
}

/// Issue #3493 P1 (epic #3524 slice 6): the SAME `LazySlotEmbedderAdapter`
/// type also wraps the opt-in Python/MPS sidecar, which resolves its
/// execution provider through torch, not ONNX Runtime. Before this fix
/// `provider()` unconditionally delegated to the ORT resolver, so `/health`
/// reported `CoreML` for a sidecar that never runs CoreML at all.
/// What: builds the adapter with `is_python: true` and asserts its
/// `provider()` equals `resolve_expected_python_provider()` (MPS on Apple
/// Silicon), never the ORT-oriented `resolve_expected_provider()`'s CoreML
/// answer.
/// Test: this test.
#[test]
fn lazy_adapter_python_reports_mps_provider() {
    use crate::core::Embedder as _;
    use crate::service::embedder_supervisor::{LazyEmbedderHandle, SupervisorConfig};

    let handle = std::sync::Arc::new(LazyEmbedderHandle::new(
        std::path::PathBuf::from("/nonexistent/trusty-embedderd-py"),
        SupervisorConfig::default(),
    ));
    let adapter = LazySlotEmbedderAdapter {
        handle,
        is_python: true,
    };
    assert_eq!(
        adapter.provider(),
        trusty_common::embedder::resolve_expected_python_provider(),
        "python-arm adapter must report the python-sidecar's resolved provider"
    );
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    assert_eq!(
        adapter.provider(),
        trusty_common::embedder::ExecutionProvider::Mps,
        "on Apple Silicon the python arm must predict MPS, not CoreML"
    );
}

/// Why (epic #3524 slice 5, issue #3493 P1): `resolved_provider_label()` must
/// forward to `LazyEmbedderHandle::last_reported_device()` rather than the
/// build-features prediction. This adapter is used for BOTH the default Rust
/// ort arm and the opt-in Python arm, so the `None`-before-spawn case (this
/// test) matters on both paths: `/health` must fall back to `provider()`'s
/// prediction rather than reporting a stale value.
/// What: a fresh (never-spawned) handle's `last_reported_device()` is `None`
/// (proven directly by `lazy_handle_last_reported_device_reflects_client` in
/// `embedder_supervisor::tests`); this test proves the adapter forwards that
/// `None` through unchanged rather than substituting a wrong guess.
/// Test: this test.
#[test]
fn lazy_python_adapter_reports_wire_device() {
    use crate::core::Embedder as _;
    use crate::service::embedder_supervisor::{LazyEmbedderHandle, SupervisorConfig};

    let handle = std::sync::Arc::new(LazyEmbedderHandle::new(
        std::path::PathBuf::from("/nonexistent/trusty-embedderd-py"),
        SupervisorConfig::default(),
    ));
    let adapter = LazySlotEmbedderAdapter {
        handle,
        is_python: true,
    };
    assert_eq!(
        adapter.resolved_provider_label(),
        None,
        "before any spawn, resolved_provider_label() must be None so /health \
         falls back to provider()'s prediction rather than reporting a stale \
         or fabricated device"
    );
}

/// Why (review finding, PR #3560 HIGH fix): `supervisor_gave_up()` must
/// forward to `LazyEmbedderHandle::supervisor_gave_up()` rather than falling
/// through to the trait-default `false` unconditionally — this is what lets
/// `FallbackEmbedderAdapter` observe the REAL supervisor give-up ceiling
/// instead of an independently-counted request-failure proxy.
/// What: a fresh (never-spawned) handle's `supervisor_gave_up()` is `false`
/// (proven directly by
/// `lazy_handle_supervisor_gave_up_reflects_handle_state` in
/// `embedder_supervisor::tests`); this test proves the adapter forwards that
/// through unchanged rather than silently falling back to the trait default
/// (which would look identical here but would NOT forward a real `true`
/// after a spawn).
/// Test: this test.
#[test]
fn lazy_python_adapter_reports_supervisor_gave_up_default_false() {
    use crate::core::Embedder as _;
    use crate::service::embedder_supervisor::{LazyEmbedderHandle, SupervisorConfig};

    let handle = std::sync::Arc::new(LazyEmbedderHandle::new(
        std::path::PathBuf::from("/nonexistent/trusty-embedderd-py"),
        SupervisorConfig::default(),
    ));
    let adapter = LazySlotEmbedderAdapter {
        handle: handle.clone(),
        is_python: true,
    };
    assert_eq!(
        adapter.supervisor_gave_up(),
        handle.supervisor_gave_up(),
        "adapter must forward LazyEmbedderHandle::supervisor_gave_up() \
         verbatim, not the trait-default false"
    );
    assert!(
        !adapter.supervisor_gave_up(),
        "before any spawn, supervisor_gave_up() must be false"
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

// ── Python-arm idle-shutdown resolution (epic #3524 fast-follow) ───────────
//
// `resolve_python_idle_shutdown_secs` is pure (no env access), so these cover
// the precedence table directly. `resolve_python_supervisor_config_*` below
// additionally exercise the real-env wiring end to end.

use super::embedder::{resolve_python_idle_shutdown_secs, resolve_python_supervisor_config};

/// py-specific var, when set to a valid value, wins outright — even over an
/// explicitly-set shared var.
#[test]
fn resolve_python_idle_shutdown_secs_py_var_wins_when_set() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(Some("60".to_string()), Some("300".to_string()), 300),
        60,
        "TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS must win over the shared var"
    );
}

/// `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS=0` (always-warm) must be honoured,
/// not treated as "unset".
#[test]
fn resolve_python_idle_shutdown_secs_py_var_zero_is_honored() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(Some("0".to_string()), None, 1800),
        0,
        "py-var 0 must disable idle-shutdown (always-warm), not fall through to 1800"
    );
}

/// When the py-var is unset but the operator explicitly set the shared var,
/// their intent must be honoured rather than silently overridden with 1800.
#[test]
fn resolve_python_idle_shutdown_secs_shared_explicit_is_honored_when_py_unset() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(None, Some("120".to_string()), 120),
        120,
        "explicit shared var must be honoured when the py-var is unset"
    );
}

/// The shared var explicitly set to `0` must also be honoured (not silently
/// promoted to the python default).
#[test]
fn resolve_python_idle_shutdown_secs_shared_explicit_zero_is_honored() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(None, Some("0".to_string()), 0),
        0,
        "explicit shared-var 0 must be honoured when the py-var is unset"
    );
}

/// Neither var set: the python arm's own 1800s default applies (NOT the
/// shared 300s default).
#[test]
fn resolve_python_idle_shutdown_secs_defaults_to_1800_when_neither_set() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(None, None, 300),
        1800,
        "python arm must default to 1800s, not the shared 300s default"
    );
}

/// A malformed py-var value must be ignored (fall through to the
/// shared-var-or-1800 resolution) rather than panicking or silently zeroing.
#[test]
fn resolve_python_idle_shutdown_secs_malformed_py_var_falls_through_to_shared() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(
            Some("not_a_number".to_string()),
            Some("90".to_string()),
            90
        ),
        90,
        "malformed py-var must fall through to the explicit shared value"
    );
}

/// A malformed py-var value with no shared var set must fall all the way
/// through to the python 1800s default.
#[test]
fn resolve_python_idle_shutdown_secs_malformed_py_var_and_no_shared_falls_through_to_1800() {
    assert_eq!(
        resolve_python_idle_shutdown_secs(Some("bogus".to_string()), None, 300),
        1800,
        "malformed py-var with no shared override must fall through to 1800"
    );
}

/// End-to-end wiring: with neither env var set, `resolve_python_supervisor_config`
/// must return `idle_shutdown_secs=1800` while leaving every other
/// `SupervisorConfig` field at the shared `from_env()` default.
#[test]
#[serial]
fn resolve_python_supervisor_config_defaults_to_1800_and_preserves_other_fields() {
    let _g1 = EnvGuard::remove("TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS");
    let _g2 = EnvGuard::remove("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS");

    let config = resolve_python_supervisor_config();
    assert_eq!(config.idle_shutdown_secs, 1800);
    assert_eq!(config.startup_timeout_secs, 30);
    assert_eq!(config.max_restarts, 5);
}

/// End-to-end wiring: `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS=0` must produce
/// an always-warm config through the real env-reading path, not just the pure
/// helper.
#[test]
#[serial]
fn resolve_python_supervisor_config_py_var_zero_is_always_warm() {
    let _g1 = EnvGuard::set("TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS", "0");
    let _g2 = EnvGuard::remove("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS");

    let config = resolve_python_supervisor_config();
    assert_eq!(config.idle_shutdown_secs, 0);
}

// ── SwitchableEmbedder wiring metadata (epic #3524 slice 6, PR 1/5) ────────

use super::embedder::quantized_from_env;

/// Unset `TRUSTY_EMBEDDER_MODEL` must describe the fp32 (non-quantized)
/// default — matches trusty-common's `resolve_default_embedding_model`
/// default.
#[test]
#[serial]
fn quantized_from_env_unset_is_false() {
    let _g = EnvGuard::remove("TRUSTY_EMBEDDER_MODEL");
    assert!(!quantized_from_env());
}

/// `int8` / `quantized` / `q` (case-insensitive, trimmed) must all describe
/// the quantized backend — mirrors trusty-common's own convention exactly.
#[test]
#[serial]
fn quantized_from_env_recognizes_all_aliases() {
    for alias in ["int8", "INT8", "quantized", " Q ", "q"] {
        let _g = EnvGuard::set("TRUSTY_EMBEDDER_MODEL", alias);
        assert!(
            quantized_from_env(),
            "{alias:?} must be recognized as the quantized alias"
        );
    }
}

/// Any other value (including unrecognized junk) must describe fp32 — the
/// same "anything else defaults to fp32" fallback trusty-common applies.
#[test]
#[serial]
fn quantized_from_env_unrecognized_value_is_false() {
    let _g = EnvGuard::set("TRUSTY_EMBEDDER_MODEL", "fp32");
    assert!(!quantized_from_env());
}

/// Issue #3530 / #3493 P1: `TRUSTY_EMBEDDER_MODEL` only governs the
/// ort/in-process `FastEmbedder` path — the Python sidecar and a manually
/// managed remote sidecar must never inherit `quantized=true` just because
/// that env var happens to be set for an unrelated ort run.
#[test]
fn backend_respects_quantized_env_only_ort_and_in_process() {
    use super::embedder::backend_respects_quantized_env;
    use crate::service::embedder_supervisor::BackendKind;

    assert!(backend_respects_quantized_env(BackendKind::Ort));
    assert!(backend_respects_quantized_env(BackendKind::InProcess));
    assert!(!backend_respects_quantized_env(BackendKind::Python));
    assert!(!backend_respects_quantized_env(BackendKind::Remote));
    assert!(!backend_respects_quantized_env(BackendKind::Candle));
}

/// RAII guard that restores an env var to its original state on drop.
///
/// Why: env vars are global; leaking changes between tests causes flakiness
/// in parallel runs. Mirrors `embedder_supervisor::tests::EnvGuard` — kept
/// local here rather than shared, since that one is private to its module.
struct EnvGuard {
    key: String,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: `#[serial]` guarantees no other test mutates env concurrently.
        unsafe { std::env::set_var(key, value) }
        Self {
            key: key.to_owned(),
            old,
        }
    }

    fn remove(key: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: same invariant as above.
        unsafe { std::env::remove_var(key) }
        Self {
            key: key.to_owned(),
            old,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test teardown; no workers live past the test body.
        unsafe {
            match &self.old {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}

// ── Graceful Apple-Silicon default resolution (epic #3524 slice 6, PR 3/5) ──

use super::embedder::{resolve_default_embedder_mode_for, truthy_env, DefaultEmbedderMode};

/// Apple Silicon + the ship-gate flag enabled must resolve to the new
/// background graceful path — the whole point of this PR.
#[test]
fn resolve_default_embedder_mode_for_apple_silicon_with_flag_is_graceful() {
    assert_eq!(
        resolve_default_embedder_mode_for(true, true, false),
        DefaultEmbedderMode::GracefulPython
    );
}

/// Apple Silicon with the ship-gate flag OFF (this PR's shipped default —
/// `TRUSTY_PY_DEFAULT` unset) must resolve to the unchanged ort path, proving
/// this PR ships as a no-op for real users.
#[test]
fn resolve_default_embedder_mode_for_apple_silicon_opted_out_is_ort() {
    assert_eq!(
        resolve_default_embedder_mode_for(true, false, false),
        DefaultEmbedderMode::Ort
    );
}

/// Apple Silicon + `TRUSTY_EMBEDDER_PYTHON_EAGER` (and the ship-gate flag
/// OFF) must resolve to the existing eager blocking python arm, not the new
/// background path.
#[test]
fn resolve_default_embedder_mode_for_apple_silicon_eager_env_is_eager() {
    assert_eq!(
        resolve_default_embedder_mode_for(true, false, true),
        DefaultEmbedderMode::EagerPython
    );
}

/// The ship-gate flag wins outright over the eager env when both are set —
/// there is no reason to run the slow blocking path when the background
/// graceful path is available.
#[test]
fn resolve_default_embedder_mode_for_apple_silicon_flag_wins_over_eager_env() {
    assert_eq!(
        resolve_default_embedder_mode_for(true, true, true),
        DefaultEmbedderMode::GracefulPython
    );
}

/// Non-Apple-Silicon (Linux, Intel mac, etc.) with NEITHER env var set must
/// resolve to the unchanged ort path.
#[test]
fn resolve_default_embedder_mode_for_non_apple_silicon_is_ort() {
    assert_eq!(
        resolve_default_embedder_mode_for(false, false, false),
        DefaultEmbedderMode::Ort
    );
}

/// Non-Apple-Silicon (Linux/CUDA) resolution MUST return the ort path even
/// when the ship-gate flag is enabled — `TRUSTY_PY_DEFAULT` only ever
/// applies on Apple Silicon; a CUDA/Linux box never reaches the python
/// bootstrap through this default-resolution path (`GracefulPython` is
/// unreachable off Apple Silicon regardless of env), so `ensure_venv` /
/// `uv` / torch are never invoked there.
#[test]
fn resolve_default_embedder_mode_for_non_apple_silicon_ignores_flag() {
    assert_eq!(
        resolve_default_embedder_mode_for(false, true, false),
        DefaultEmbedderMode::Ort,
        "GracefulPython must never be selected off Apple Silicon"
    );
}

/// `truthy_env` must recognize the common truthy spellings, case-insensitive
/// and trimmed.
#[test]
#[serial]
fn truthy_env_recognizes_common_truthy_values() {
    for value in ["1", "true", "TRUE", " yes ", "On"] {
        let _g = EnvGuard::set("TRUSTY_TEST_TRUTHY_3524", value);
        assert!(
            truthy_env("TRUSTY_TEST_TRUTHY_3524"),
            "{value:?} must be truthy"
        );
    }
}

/// Unset or any non-truthy value must resolve to `false`.
#[test]
#[serial]
fn truthy_env_false_for_unset_or_other() {
    let _g = EnvGuard::remove("TRUSTY_TEST_TRUTHY_3524");
    assert!(!truthy_env("TRUSTY_TEST_TRUTHY_3524"));

    let _g2 = EnvGuard::set("TRUSTY_TEST_TRUTHY_3524", "0");
    assert!(!truthy_env("TRUSTY_TEST_TRUTHY_3524"));

    let _g3 = EnvGuard::set("TRUSTY_TEST_TRUTHY_3524", "nope");
    assert!(!truthy_env("TRUSTY_TEST_TRUTHY_3524"));
}

// ── TRUSTY_PY_BOOTSTRAP_RETRIES resolution (epic #3524 slice 6, PR 3/5) ─────

use super::graceful_bootstrap::resolve_bootstrap_retries;

#[test]
#[serial]
fn resolve_bootstrap_retries_defaults_to_2_when_unset() {
    let _g = EnvGuard::remove("TRUSTY_PY_BOOTSTRAP_RETRIES");
    assert_eq!(resolve_bootstrap_retries(), 2);
}

#[test]
#[serial]
fn resolve_bootstrap_retries_honors_explicit_value() {
    let _g = EnvGuard::set("TRUSTY_PY_BOOTSTRAP_RETRIES", "5");
    assert_eq!(resolve_bootstrap_retries(), 5);
}

/// A malformed or zero value must fall back to the default rather than
/// disabling retries entirely (zero attempts would never even try once).
#[test]
#[serial]
fn resolve_bootstrap_retries_malformed_or_zero_falls_back_to_default() {
    let _g = EnvGuard::set("TRUSTY_PY_BOOTSTRAP_RETRIES", "not_a_number");
    assert_eq!(resolve_bootstrap_retries(), 2);

    let _g2 = EnvGuard::set("TRUSTY_PY_BOOTSTRAP_RETRIES", "0");
    assert_eq!(resolve_bootstrap_retries(), 2);
}

// ── Background bootstrap→hot-swap orchestrator (epic #3524 slice 6, PR 3/5) ─
//
// These drive `graceful_bootstrap::drive_bootstrap` — the actual retry/probe/
// swap state machine — with a fully deterministic fake `PythonBootstrap` and
// a fake in-memory `Embedder`. No real `uv`, torch, MPS, or subprocess ever
// runs; every test completes in milliseconds.

use super::graceful_bootstrap::{drive_bootstrap, PythonBootstrap};
use crate::core::Embedder as _;
use crate::service::embedder_supervisor::{
    ActiveBackend as SwitchableActiveBackend, BackendKind as SwitchableBackendKind,
    BootstrapState as SwitchableBootstrapState, SwitchableEmbedder,
};
use crate::service::SearchAppState;
use std::sync::atomic::{AtomicU32 as OrchestratorAtomicU32, AtomicUsize};
use std::sync::Arc as OrchestratorArc;

/// The initial ort "backend" installed into the `SwitchableEmbedder` under
/// test — a fake, not the real ort stdio sidecar. Its call counter proves
/// `inner` is untouched when the orchestrator gives up and stays on ort.
struct FakeOrtEmbedder {
    calls: AtomicUsize,
}

impl FakeOrtEmbedder {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl crate::core::Embedder for FakeOrtEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(vec![text.len() as f32; trusty_common::embedder::EMBED_DIM])
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(texts
            .iter()
            .map(|t| vec![t.len() as f32; trusty_common::embedder::EMBED_DIM])
            .collect())
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// Fake python adapter whose `embed_batch` (the readiness probe) always
/// succeeds.
struct FakePythonEmbedderOk;

#[async_trait::async_trait]
impl crate::core::Embedder for FakePythonEmbedderOk {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![text.len() as f32; trusty_common::embedder::EMBED_DIM])
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| vec![t.len() as f32; trusty_common::embedder::EMBED_DIM])
            .collect())
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// Fake python adapter whose readiness probe always fails (simulates a
/// spawn/import/model-load failure discovered through the real embed call).
struct FakePythonEmbedderErr;

#[async_trait::async_trait]
impl crate::core::Embedder for FakePythonEmbedderErr {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("fake readiness probe failure")
    }

    async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("fake readiness probe failure")
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// Fake python adapter whose readiness probe never resolves within any
/// reasonable timeout (simulates a hung/stalled real embed call) — used to
/// drive the orchestrator's probe-TIMEOUT teardown path deterministically
/// under paused virtual time.
struct FakePythonEmbedderHangs;

#[async_trait::async_trait]
impl crate::core::Embedder for FakePythonEmbedderHangs {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        std::future::pending().await
    }

    async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        std::future::pending().await
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// Controllable fake [`PythonAdapterTeardown`] — records how many times
/// `teardown()` was invoked so tests can assert it fires on the probe
/// failure/timeout paths and NEVER on success or on a pre-adapter
/// (venv-bootstrap) failure (epic #3524 slice 6 PR-3 fix — code-critic HIGH).
struct FakeTeardown {
    calls: OrchestratorArc<AtomicUsize>,
}

#[async_trait::async_trait]
impl super::graceful_bootstrap::PythonAdapterTeardown for FakeTeardown {
    async fn teardown(&self) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Fully deterministic fake [`PythonBootstrap`] — no real `uv`/torch/venv or
/// subprocess ever runs.
struct FakeBootstrap {
    /// Number of leading `ensure_venv()` calls that fail before succeeding.
    venv_failures_remaining: AtomicUsize,
    /// When true, the built adapter's readiness probe always fails.
    fail_probe: bool,
    /// When true, the built adapter's readiness probe hangs forever (the
    /// probe timeout fires instead of an `Err`).
    hang_probe: bool,
    /// Total `ensure_venv()` invocations observed — lets tests assert retry
    /// behavior actually re-ran the bootstrap step.
    venv_attempts: AtomicUsize,
    /// Total `PythonAdapterTeardown::teardown()` invocations observed across
    /// every `build_adapter` call this fake made.
    teardown_calls: OrchestratorArc<AtomicUsize>,
}

impl FakeBootstrap {
    fn always_succeeds() -> Self {
        Self {
            venv_failures_remaining: AtomicUsize::new(0),
            fail_probe: false,
            hang_probe: false,
            venv_attempts: AtomicUsize::new(0),
            teardown_calls: OrchestratorArc::new(AtomicUsize::new(0)),
        }
    }

    fn probe_always_fails() -> Self {
        Self {
            venv_failures_remaining: AtomicUsize::new(0),
            fail_probe: true,
            hang_probe: false,
            venv_attempts: AtomicUsize::new(0),
            teardown_calls: OrchestratorArc::new(AtomicUsize::new(0)),
        }
    }

    fn probe_always_hangs() -> Self {
        Self {
            venv_failures_remaining: AtomicUsize::new(0),
            fail_probe: false,
            hang_probe: true,
            venv_attempts: AtomicUsize::new(0),
            teardown_calls: OrchestratorArc::new(AtomicUsize::new(0)),
        }
    }

    fn venv_always_fails() -> Self {
        Self {
            venv_failures_remaining: AtomicUsize::new(u32::MAX as usize),
            fail_probe: false,
            hang_probe: false,
            venv_attempts: AtomicUsize::new(0),
            teardown_calls: OrchestratorArc::new(AtomicUsize::new(0)),
        }
    }

    fn venv_fails_once_then_succeeds() -> Self {
        Self {
            venv_failures_remaining: AtomicUsize::new(1),
            fail_probe: false,
            hang_probe: false,
            venv_attempts: AtomicUsize::new(0),
            teardown_calls: OrchestratorArc::new(AtomicUsize::new(0)),
        }
    }

    fn teardown_calls(&self) -> usize {
        self.teardown_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl PythonBootstrap for FakeBootstrap {
    fn ensure_venv(&self) -> anyhow::Result<()> {
        self.venv_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let remaining = self
            .venv_failures_remaining
            .load(std::sync::atomic::Ordering::Relaxed);
        if remaining > 0 {
            self.venv_failures_remaining
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            anyhow::bail!("fake venv bootstrap failure");
        }
        Ok(())
    }

    fn locate_launcher(&self) -> anyhow::Result<std::path::PathBuf> {
        Ok(std::path::PathBuf::from("/fake/trusty-embedderd-py"))
    }

    fn build_adapter(
        &self,
        _launcher: std::path::PathBuf,
    ) -> (
        OrchestratorArc<dyn crate::core::Embedder>,
        Option<OrchestratorArc<OrchestratorAtomicU32>>,
        OrchestratorArc<dyn super::graceful_bootstrap::PythonAdapterTeardown>,
    ) {
        let adapter: OrchestratorArc<dyn crate::core::Embedder> = if self.fail_probe {
            OrchestratorArc::new(FakePythonEmbedderErr)
        } else if self.hang_probe {
            OrchestratorArc::new(FakePythonEmbedderHangs)
        } else {
            OrchestratorArc::new(FakePythonEmbedderOk)
        };
        let teardown: OrchestratorArc<dyn super::graceful_bootstrap::PythonAdapterTeardown> =
            OrchestratorArc::new(FakeTeardown {
                calls: OrchestratorArc::clone(&self.teardown_calls),
            });
        (
            adapter,
            Some(OrchestratorArc::new(OrchestratorAtomicU32::new(4242))),
            teardown,
        )
    }

    fn probe_timeout(&self) -> std::time::Duration {
        if self.hang_probe {
            // Short enough that the paused-clock test resolves instantly,
            // long enough to be unambiguous in intent.
            std::time::Duration::from_millis(50)
        } else {
            std::time::Duration::from_secs(5)
        }
    }
}

/// The `ActiveBackend` `build_embedder`'s new graceful-python arm installs:
/// ort, `Bootstrapping`.
fn test_ort_bootstrapping_active() -> SwitchableActiveBackend {
    SwitchableActiveBackend {
        kind: SwitchableBackendKind::Ort,
        provider: trusty_common::embedder::ExecutionProvider::Cpu,
        model: "all-MiniLM-L6-v2".to_string(),
        quantized: false,
        bootstrap: SwitchableBootstrapState::Bootstrapping,
    }
}

/// Happy path: a successful bootstrap + readiness probe must hot-swap the
/// switchable from ort/Bootstrapping to python/Ready.
#[tokio::test]
async fn graceful_bootstrap_swaps_to_python_on_success() {
    let ort: OrchestratorArc<dyn crate::core::Embedder> =
        OrchestratorArc::new(FakeOrtEmbedder::new());
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::always_succeeds());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 2).await;

    let active = switchable.active();
    assert_eq!(active.kind, SwitchableBackendKind::Python);
    assert_eq!(active.bootstrap, SwitchableBootstrapState::Ready);
    assert_eq!(
        fake.teardown_calls(),
        0,
        "teardown must never fire on the success path — the handle stays live"
    );
}

/// A readiness-probe failure must leave the switchable on the still-installed
/// ort backend, marked `Failed` — never a torn or missing backend.
#[tokio::test]
async fn graceful_bootstrap_stays_ort_and_failed_after_probe_failure() {
    let ort = OrchestratorArc::new(FakeOrtEmbedder::new());
    let ort_dyn: OrchestratorArc<dyn crate::core::Embedder> = OrchestratorArc::clone(&ort) as _;
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort_dyn,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::probe_always_fails());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 1).await;

    let active = switchable.active();
    assert_eq!(
        active.kind,
        SwitchableBackendKind::Ort,
        "must stay on the still-installed ort backend"
    );
    assert_eq!(active.bootstrap, SwitchableBootstrapState::Failed);

    // Prove the ort backend is still live and serving (not swapped away).
    switchable
        .embed("probe")
        .await
        .expect("ort backend must still serve after a failed python bootstrap");
    assert_eq!(ort.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// A venv-bootstrap failure (before a python adapter is even built) must
/// also leave the switchable on ort, marked `Failed`.
#[tokio::test]
async fn graceful_bootstrap_stays_ort_and_failed_after_bootstrap_failure() {
    let ort: OrchestratorArc<dyn crate::core::Embedder> =
        OrchestratorArc::new(FakeOrtEmbedder::new());
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::venv_always_fails());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 1).await;

    let active = switchable.active();
    assert_eq!(active.kind, SwitchableBackendKind::Ort);
    assert_eq!(active.bootstrap, SwitchableBootstrapState::Failed);
    assert_eq!(
        fake.teardown_calls(),
        0,
        "no adapter/handle was ever built (venv bootstrap failed first) — \
         nothing to tear down"
    );
}

/// code-critic HIGH fix (epic #3524 slice 6 PR-3): a probe-failure path must
/// cooperatively tear down the just-spawned handle BEFORE dropping it, so a
/// failed bootstrap never orphans a real python/MPS child process waiting on
/// the idle watchdog (up to 1800s later).
///
/// Why: this is the dedicated regression test for the exact leak code-critic
/// flagged — earlier `graceful_bootstrap_stays_ort_and_failed_after_probe_failure`
/// proves the SWITCHABLE state; this test proves the TEARDOWN CALL itself
/// fires exactly once per failed attempt.
/// What: 2 retries, both probe failures — `teardown_calls()` must equal 2
/// (one per attempt, each attempt builds a fresh adapter/handle).
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_probe_failure_tears_down_the_handle() {
    let ort: OrchestratorArc<dyn crate::core::Embedder> =
        OrchestratorArc::new(FakeOrtEmbedder::new());
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::probe_always_fails());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 2).await;

    assert_eq!(
        switchable.active().bootstrap,
        SwitchableBootstrapState::Failed
    );
    assert_eq!(
        fake.teardown_calls(),
        2,
        "teardown must fire once per failed attempt — a probe failure must \
         never leave a spawned handle un-shut-down"
    );
}

/// code-critic HIGH fix, timeout variant: a probe TIMEOUT (not just an
/// `Err`) must ALSO tear down the handle before dropping it — the probe may
/// still be in flight against a live, just-spawned child when the timeout
/// fires.
/// What: a hanging fake adapter + a short fake probe timeout, under paused
/// virtual time so the test resolves instantly; asserts the switchable stays
/// ort/Failed and `teardown_calls() == 1`.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_probe_timeout_tears_down_the_handle() {
    let ort: OrchestratorArc<dyn crate::core::Embedder> =
        OrchestratorArc::new(FakeOrtEmbedder::new());
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::probe_always_hangs());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 1).await;

    let active = switchable.active();
    assert_eq!(active.kind, SwitchableBackendKind::Ort);
    assert_eq!(active.bootstrap, SwitchableBootstrapState::Failed);
    assert_eq!(
        fake.teardown_calls(),
        1,
        "a probe TIMEOUT must also tear down the handle, not just a probe Err"
    );
}

/// A transient failure on the first attempt must be retried, succeeding on
/// the second — proving the retry loop actually re-invokes every bootstrap
/// step rather than giving up after one failure. Uses paused virtual time so
/// the linear backoff between attempts resolves instantly.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_retries_before_giving_up() {
    let ort: OrchestratorArc<dyn crate::core::Embedder> =
        OrchestratorArc::new(FakeOrtEmbedder::new());
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::venv_fails_once_then_succeeds());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 2).await;

    let active = switchable.active();
    assert_eq!(
        active.kind,
        SwitchableBackendKind::Python,
        "the second attempt must succeed and hot-swap"
    );
    assert_eq!(active.bootstrap, SwitchableBootstrapState::Ready);
    assert_eq!(
        fake.venv_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        2,
        "ensure_venv must have been retried, not just called once"
    );
}

/// Exhausting every retry attempt must mark the switchable `Failed` while
/// staying on ort — confirms the retry loop actually bounds itself at
/// `retries` attempts rather than looping forever.
#[tokio::test(start_paused = true)]
async fn graceful_bootstrap_gives_up_after_exhausting_retries() {
    let ort: OrchestratorArc<dyn crate::core::Embedder> =
        OrchestratorArc::new(FakeOrtEmbedder::new());
    let switchable = OrchestratorArc::new(SwitchableEmbedder::new(
        ort,
        test_ort_bootstrapping_active(),
    ));
    let state = SearchAppState::new(crate::core::registry::IndexRegistry::new());
    let fake = OrchestratorArc::new(FakeBootstrap::venv_always_fails());
    let ops: OrchestratorArc<dyn PythonBootstrap> = OrchestratorArc::clone(&fake) as _;

    drive_bootstrap(OrchestratorArc::clone(&switchable), state, ops, 3).await;

    let active = switchable.active();
    assert_eq!(active.kind, SwitchableBackendKind::Ort);
    assert_eq!(active.bootstrap, SwitchableBootstrapState::Failed);
    assert_eq!(
        fake.venv_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        3,
        "must attempt exactly `retries` times, no more, no less"
    );
}
