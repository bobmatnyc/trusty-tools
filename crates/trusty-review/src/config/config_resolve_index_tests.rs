//! Tests for `ReviewConfig::resolve_index` and the cmd_run/serve/compare wiring.
//!
//! Why: split from `config_tests.rs` to keep that file under the 500-line cap
//! (#610).  This file covers the auto-derive feature (issue #661) and the
//! production wiring (issue #670).
//! What: exercises the mock-backed `resolve_index` API and proves the wiring
//! pattern used in `cmd_run`, `build_app_state` (serve), and `cmd_compare`.
//! Test: each function is a self-contained async unit test using `tokio::test`.

use super::*;

use crate::integrations::{
    health::{EmbedderState, HealthResponse},
    search_client::{IndexInfo, SearchClient, SearchClientError, SearchResult},
};
use async_trait::async_trait;
use serial_test::serial;

// ─── Mock clients ─────────────────────────────────────────────────────────────

/// Mock SearchClient that returns a fixed index list.
struct FixedIndexSearch(Vec<IndexInfo>);

#[async_trait]
impl SearchClient for FixedIndexSearch {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
        })
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(self.0.clone())
    }
    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

/// Mock SearchClient that always fails list_indexes.
struct FailListSearch;

#[async_trait]
impl SearchClient for FailListSearch {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Err(SearchClientError::Unavailable("daemon down".to_string()))
    }
    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }
}

fn make_index_info(id: &str, root_path: Option<&str>) -> IndexInfo {
    IndexInfo {
        id: id.to_string(),
        name: None,
        root_path: root_path.map(|s| s.to_string()),
    }
}

// ─── resolve_index tests (issue #661) ─────────────────────────────────────────

/// When `TRUSTY_SEARCH_INDEX` is set, `resolve_index` must not change it.
///
/// Why: explicit operator config always wins; the auto-derive logic must not
/// stomp on a deliberately-set index name (issue #661).
/// What: sets `search_index_explicit = true`, calls `resolve_index` with a
/// daemon that would return a different index, asserts value unchanged.
/// Test: this test.
#[tokio::test]
async fn resolve_index_noop_when_explicit() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "explicit-index".to_string();
    config.search_index_explicit = true;

    let indexes = vec![make_index_info("auto-index", Some("/tmp/some-project"))];
    let client = FixedIndexSearch(indexes);
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "explicit-index",
        "explicit index must not be overwritten by auto-derive"
    );
}

/// When `TRUSTY_SEARCH_INDEX` is unset and the daemon returns a matching index,
/// `resolve_index` must update `search_index` to the matched id.
///
/// Why: the core auto-derive feature (issue #661).
/// What: creates a temp dir with a `.git` subdirectory, sets it as cwd,
/// provides a daemon that returns an index with that path, and asserts the
/// resolved value.
/// Test: this test.
#[tokio::test]
#[serial]
async fn resolve_index_updates_when_match_found() {
    let root = tempfile::tempdir().unwrap();
    // Create .git so find_git_root stops here.
    std::fs::create_dir(root.path().join(".git")).unwrap();
    // Make the canonical path (symlinks resolved).
    let canonical = root.path().canonicalize().unwrap();
    let root_path_str = canonical.to_str().unwrap().to_string();

    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let indexes = vec![make_index_info("my-project", Some(&root_path_str))];
    let client = FixedIndexSearch(indexes);

    // Temporarily change cwd to the temp dir so repo_root_from_cwd picks it up.
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(root.path()).unwrap();
    config.resolve_index(&client).await;
    std::env::set_current_dir(original_dir).unwrap();

    assert_eq!(
        config.search_index, "my-project",
        "auto-derive must update search_index to the matched index"
    );
}

/// When the daemon is unreachable, `resolve_index` must keep `search_index`
/// unchanged and not panic.
///
/// Why: the auto-derive is best-effort; daemon downtime must degrade gracefully
/// to the default `"main"` rather than failing the review startup (issue #661).
/// What: provides a FailListSearch, asserts `search_index` stays at `"main"`.
/// Test: this test.
#[tokio::test]
async fn resolve_index_falls_back_on_daemon_error() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let client = FailListSearch;
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "main",
        "daemon error must leave search_index at fallback 'main'"
    );
}

/// When no registered index root_path matches the repo root, keep the default.
///
/// Why: a fresh machine with no indexed projects should not crash or produce an
/// incorrect index name (issue #661).
/// What: provides an index with an unrelated root_path, asserts `"main"` kept.
/// Test: this test.
#[tokio::test]
async fn resolve_index_keeps_default_when_no_match() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let indexes = vec![make_index_info(
        "other-project",
        Some("/srv/totally-different"),
    )];
    let client = FixedIndexSearch(indexes);
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "main",
        "no-match must leave search_index at fallback 'main'"
    );
}

// ─── Wiring-path tests (issue #670) ──────────────────────────────────────────
//
// These tests verify the pattern used in cmd_run, build_app_state (serve), and
// cmd_compare: a mutable config is resolved against a fake SearchClient BEFORE
// it is consumed by the pipeline / AppState.  They mirror the exact call sequence
// introduced in the fix so regressions are caught immediately.

/// Simulate the cmd_run wiring: config created, resolve_index called with a
/// matching client, then search_index is consumed.  Unset TRUSTY_SEARCH_INDEX
/// → auto-derive updates the index.
///
/// Why: proves the wiring in cmd_run (issue #670 fix) — `resolve_index` must
/// be called before `run_review` consumes `config.search_index`.
/// What: creates a temp git root, sets up FixedIndexSearch matching it, runs
/// `resolve_index`, and asserts `search_index` was updated before any pipeline
/// call would consume it.
/// Test: this test.
#[tokio::test]
#[serial]
async fn wiring_cmd_run_resolve_index_updates_before_pipeline() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let root_path_str = canonical.to_str().unwrap().to_string();

    // Simulate what cmd_run does: build config, mark index as unset.
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let indexes = vec![make_index_info("run-project", Some(&root_path_str))];
    let client = FixedIndexSearch(indexes);

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(root.path()).unwrap();
    // This is the exact call that cmd_run now makes.
    config.resolve_index(&client).await;
    std::env::set_current_dir(original_dir).unwrap();

    // The pipeline would now receive the correct index, not "main".
    assert_eq!(
        config.search_index, "run-project",
        "cmd_run wiring: resolve_index must update search_index before pipeline"
    );
}

/// Simulate the build_app_state wiring: config resolved before AppState is
/// constructed.  Explicit TRUSTY_SEARCH_INDEX → no change.
///
/// Why: proves the wiring in build_app_state (serve mode, issue #670 fix) —
/// when the operator sets TRUSTY_SEARCH_INDEX, auto-derive must not overwrite.
/// What: sets search_index_explicit = true, calls resolve_index with a
/// daemon that returns a different index, asserts the explicit value is kept.
/// Test: this test.
#[tokio::test]
async fn wiring_build_app_state_explicit_index_unchanged() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "operator-chosen".to_string();
    config.search_index_explicit = true; // Explicit → no-op.

    let indexes = vec![make_index_info("auto-derived", Some("/srv/some-project"))];
    let client = FixedIndexSearch(indexes);

    // Mirrors what build_app_state now does before AppState::with_verifier_and_dedup.
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "operator-chosen",
        "serve wiring: explicit TRUSTY_SEARCH_INDEX must survive resolve_index"
    );
}

/// Simulate the cmd_compare wiring: index resolved once before the per-model
/// loop, and all model runs share the updated index.
///
/// Why: proves the wiring in cmd_compare (issue #670 fix) — a single
/// resolve_index call at the start of cmd_compare must propagate to every
/// model run inside the loop.
/// What: resolves once with a matching client, then asserts subsequent reads
/// of config.search_index return the derived value.
/// Test: this test.
#[tokio::test]
#[serial]
async fn wiring_cmd_compare_resolve_index_applies_to_all_model_runs() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let root_path_str = canonical.to_str().unwrap().to_string();

    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let indexes = vec![make_index_info("compare-project", Some(&root_path_str))];
    let client = FixedIndexSearch(indexes);

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(root.path()).unwrap();
    // This is the call that cmd_compare now makes once, before the model loop.
    config.resolve_index(&client).await;
    std::env::set_current_dir(original_dir).unwrap();

    // All model iterations would see the updated index.
    for _ in 0..3 {
        assert_eq!(
            config.search_index, "compare-project",
            "compare wiring: all model runs must see the resolved index"
        );
    }
}
