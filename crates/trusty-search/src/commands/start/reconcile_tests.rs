//! Tests for `commands::start::reconcile`.
//!
//! Why: split into a dedicated `*_tests.rs` file so `reconcile.rs` stays
//! under the 500-SLOC production cap while this file enjoys the 1500-SLOC
//! test cap (path contains `_tests.rs`).
//! What: covers the pure gate functions, git-backed integration helpers, and
//! async reconciliation paths.
//! Test: run with `cargo test -p trusty-search -- reconcile`.

use super::*;
use chrono::Datelike as _;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Pure unit tests ──────────────────────────────────────────────────────────

/// Why: the disable gate must be testable without touching process env to
/// avoid flaky behaviour in parallel test binaries.
/// Test: only `Some("1")` disables; other values leave reconciliation on.
#[test]
fn reconcile_disabled_for_value_only_matches_one() {
    assert!(
        reconcile_disabled_for_value(Some("1")),
        "Some(\"1\") must disable"
    );
    assert!(
        !reconcile_disabled_for_value(None),
        "None (unset) must keep enabled"
    );
    assert!(
        !reconcile_disabled_for_value(Some("0")),
        "\"0\" must keep enabled"
    );
    assert!(
        !reconcile_disabled_for_value(Some("true")),
        "\"true\" must keep enabled"
    );
    assert!(
        !reconcile_disabled_for_value(Some("")),
        "empty string must keep enabled"
    );
}

/// Why: outside a git repo `changed_files_between` must return `None`,
/// not panic — callers fall back to a full reindex on `None`.
/// Test: this test.
#[tokio::test]
async fn changed_files_between_returns_none_outside_git_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        changed_files_between(tmp.path(), "deadbeef")
            .await
            .is_none(),
        "expected None outside a git repo"
    );
}

/// Why: skip predicates must exclude build artefacts and pass normal source.
/// Test: `node_modules` path excluded; plain Rust file not excluded.
#[test]
fn reconcile_skip_excluded_path() {
    assert!(
        should_skip_for_reconcile(std::path::Path::new("node_modules/lodash/index.js")),
        "node_modules must be excluded"
    );
    assert!(
        !should_skip_for_reconcile(std::path::Path::new("src/lib.rs")),
        "normal source file must not be excluded"
    );
}

/// Why: `stamp_handle` (which now uses `chrono::Utc::now().to_rfc3339()`)
/// must write a valid RFC-3339 timestamp with month ∈ 1..=12 and day ∈ 1..=31.
/// The old hand-rolled Gregorian approximation that lived in this module
/// previously produced month values of 13 and day values of 0 or 32 (e.g.
/// January 32nd); this test fails on that implementation.
/// Test: call `stamp_handle` on a minimal handle, read `last_indexed_at`,
/// parse it with `chrono::DateTime::parse_from_rfc3339`, and assert field
/// ranges. This is a regression guard for the FIX-1 reviewer finding.
#[tokio::test]
async fn stamp_handle_produces_valid_rfc3339_date() {
    use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let dir = tempfile::tempdir().expect("tempdir");
    let handle = Arc::new(IndexHandle {
        id: IndexId::new("ts-validity"),
        indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
            "ts-validity",
            dir.path(),
        ))),
        root_path: dir.path().to_path_buf(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: true,
        respect_gitignore: true,
        extra_skip_dirs: vec![],
        data_file_max_bytes: 0,
        path_filter: vec![],
        context_embedding: Arc::new(RwLock::new(None)),
        context_summary: Arc::new(RwLock::new(None)),
        indexed_head_sha: Arc::new(RwLock::new(None)),
        last_indexed_at: Arc::new(RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        defer_embed: false,
        stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 0,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
            skip_kg: false,
            corpus_open_failed: false,
        }))),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
    });

    stamp_handle(&handle, "abc123").await;

    let ts = handle
        .last_indexed_at
        .read()
        .await
        .clone()
        .expect("last_indexed_at must be set by stamp_handle");

    // Must parse as RFC-3339 (would fail on the old broken hand-rolled math).
    let dt = chrono::DateTime::parse_from_rfc3339(&ts)
        .unwrap_or_else(|e| panic!("stamp_handle wrote unparseable timestamp '{ts}': {e}"));

    // Calendar field ranges — catches month=13 or day=32 produced by the old
    // Gregorian approximation that divided day_of_year by 30.
    let month = dt.month();
    let day = dt.day();
    assert!(
        (1..=12).contains(&month),
        "month must be 1..=12, got {month} (ts={ts})"
    );
    assert!(
        (1..=31).contains(&day),
        "day must be 1..=31, got {day} (ts={ts})"
    );
    assert!(
        ts.contains('T'),
        "RFC-3339 timestamp must contain 'T': {ts}"
    );
}

// ── Git-backed integration helpers ───────────────────────────────────────────

/// Create a minimal git repo with one committed file.
/// Returns `(TempDir, initial_sha, root_path)`.
fn init_git_repo_with_file(
    filename: &str,
    content: &str,
) -> (tempfile::TempDir, String, std::path::PathBuf) {
    use std::process::Command;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();

    let ok = |output: std::process::Output| {
        assert!(output.status.success(), "git command failed");
    };

    ok(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&root)
        .output()
        .expect("git init"));
    ok(Command::new("git")
        .args(["config", "user.email", "test@test.test"])
        .current_dir(&root)
        .output()
        .expect("git config email"));
    ok(Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&root)
        .output()
        .expect("git config name"));

    std::fs::write(root.join(filename), content).expect("write file");
    ok(Command::new("git")
        .args(["add", "."])
        .current_dir(&root)
        .output()
        .expect("git add"));
    ok(Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&root)
        .output()
        .expect("git commit"));

    let sha_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .expect("git rev-parse");
    let sha = std::str::from_utf8(&sha_out.stdout)
        .expect("utf8")
        .trim()
        .to_owned();

    (dir, sha, root)
}

/// Add a second commit with modified file content. Returns the new HEAD SHA.
fn add_commit(root: &std::path::Path, filename: &str, content: &str) -> String {
    use std::process::Command;

    std::fs::write(root.join(filename), content).expect("write file");
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", "update"])
        .current_dir(root)
        .output()
        .expect("git commit");
    let sha_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .expect("git rev-parse");
    std::str::from_utf8(&sha_out.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

// ── Git-backed integration tests ─────────────────────────────────────────────

/// Why: `changed_files_between` must find the file that changed between
/// two real commits in a git repo.
/// Test: two commits; assert the diff includes the modified file.
#[tokio::test]
async fn changed_files_between_finds_modified_file() {
    let (_dir, first_sha, root) = init_git_repo_with_file("src.rs", "fn a() {}");
    add_commit(&root, "src.rs", "fn a() {}\nfn b() {}\n");

    let files = changed_files_between(&root, &first_sha)
        .await
        .expect("changed_files_between must return Some in a valid repo");
    assert!(
        files.iter().any(|f| f == "src.rs"),
        "expected src.rs in changed files, got {files:?}"
    );
}

/// Why: a fabricated / history-rewritten SHA must return `None` so the
/// caller can fall back to a full reindex.
/// Test: pass a zeroed SHA to a valid git repo.
#[tokio::test]
async fn changed_files_between_returns_none_for_unknown_sha() {
    let (_dir, _sha, root) = init_git_repo_with_file("foo.rs", "fn x() {}");
    assert!(
        changed_files_between(&root, "0000000000000000000000000000000000000000")
            .await
            .is_none(),
        "unknown SHA must return None"
    );
}

/// Why: `stamp_handle` must update both `indexed_head_sha` and
/// `last_indexed_at` so the staleness signal clears after reconciliation.
/// Test: build a minimal handle, call `stamp_handle`, assert both fields.
#[tokio::test]
async fn reconcile_stamps_head_sha_after_delta() {
    use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let dir = tempfile::tempdir().expect("tempdir");
    let handle = Arc::new(IndexHandle {
        id: IndexId::new("test-stamp"),
        indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
            "test-stamp",
            dir.path(),
        ))),
        root_path: dir.path().to_path_buf(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: true,
        respect_gitignore: true,
        extra_skip_dirs: vec![],
        data_file_max_bytes: 0,
        path_filter: vec![],
        context_embedding: Arc::new(RwLock::new(None)),
        context_summary: Arc::new(RwLock::new(None)),
        indexed_head_sha: Arc::new(RwLock::new(Some("old_sha".to_owned()))),
        last_indexed_at: Arc::new(RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        defer_embed: false,
        stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 0,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
            skip_kg: false,
            corpus_open_failed: false,
        }))),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
    });

    let new_sha = "new_sha_abcde";
    stamp_handle(&handle, new_sha).await;

    let stored = handle.indexed_head_sha.read().await.clone();
    assert_eq!(
        stored,
        Some(new_sha.to_owned()),
        "indexed_head_sha must equal new_sha after stamp"
    );

    let ts_opt = handle.last_indexed_at.read().await.clone();
    assert!(ts_opt.is_some(), "last_indexed_at must be Some after stamp");

    // Verify the timestamp is a valid RFC-3339 date (not a broken approximation).
    let ts = ts_opt.unwrap();
    let dt = chrono::DateTime::parse_from_rfc3339(&ts)
        .unwrap_or_else(|e| panic!("stamp_handle wrote unparseable timestamp '{ts}': {e}"));
    let month = dt.month();
    let day = dt.day();
    assert!(
        (1..=12).contains(&month),
        "stamped month must be 1..=12, got {month}"
    );
    assert!(
        (1..=31).contains(&day),
        "stamped day must be 1..=31, got {day}"
    );
}

/// Why: `TRUSTY_NO_BOOT_RECONCILE=1` must prevent any reconcile task from
/// being spawned. This uses `serial_test::serial` to avoid env contamination
/// from concurrent tests.
/// Test: set the env var, call `reconcile_stale_indexes`, assert no panic.
#[tokio::test]
#[serial_test::serial]
async fn reconcile_disabled_gate() {
    // SAFETY: serial; only one test mutates this env var at a time.
    unsafe { std::env::set_var("TRUSTY_NO_BOOT_RECONCILE", "1") };
    let state = crate::service::SearchAppState::new(crate::core::registry::IndexRegistry::new());
    reconcile_stale_indexes(&state).await; // must not panic
    unsafe { std::env::remove_var("TRUSTY_NO_BOOT_RECONCILE") };
}

/// Why: when `indexed_head_sha == current HEAD`, `reconcile_one_index`
/// must be a no-op (the handle must not be modified).
/// Test: real git repo; stamp handle with current HEAD; call
/// `reconcile_one_index`; assert SHA is unchanged.
#[tokio::test]
async fn reconcile_up_to_date_index_is_noop() {
    use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let (_dir, first_sha, root) = init_git_repo_with_file("hello.rs", "fn hello() {}");

    let handle = Arc::new(IndexHandle {
        id: IndexId::new("test-up-to-date"),
        indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
            "test-up-to-date",
            &root,
        ))),
        root_path: root.clone(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: true,
        respect_gitignore: true,
        extra_skip_dirs: vec![],
        data_file_max_bytes: 0,
        path_filter: vec![],
        context_embedding: Arc::new(RwLock::new(None)),
        context_summary: Arc::new(RwLock::new(None)),
        indexed_head_sha: Arc::new(RwLock::new(Some(first_sha.clone()))),
        last_indexed_at: Arc::new(RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        defer_embed: false,
        stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 0,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
            skip_kg: false,
            corpus_open_failed: false,
        }))),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
    });

    reconcile_one_index(Arc::clone(&handle)).await;

    let stored = handle.indexed_head_sha.read().await.clone();
    assert_eq!(
        stored,
        Some(first_sha),
        "SHA must not change when already up-to-date"
    );
    // last_indexed_at must remain None (no work done).
    assert!(
        handle.last_indexed_at.read().await.is_none(),
        "last_indexed_at must remain None when no-op"
    );
}

/// Why: when the stored SHA is older than current HEAD and the delta is
/// within threshold, per-file reconciliation must run and stamp new SHA.
/// Test: two-commit repo; handle stores old SHA; call `reconcile_one_index`;
/// assert `indexed_head_sha` is updated to the new HEAD and
/// `last_indexed_at` is stamped.
#[tokio::test]
async fn reconcile_stale_index_stamps_new_sha() {
    use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let (_dir, first_sha, root) = init_git_repo_with_file("lib.rs", "fn old() {}");
    add_commit(&root, "lib.rs", "fn old() {}\nfn new_fn() {}\n");
    let current_sha = crate::core::git::head_sha(&root).expect("head sha");

    let handle = Arc::new(IndexHandle {
        id: IndexId::new("test-stale"),
        indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
            "test-stale",
            &root,
        ))),
        root_path: root.clone(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: true,
        respect_gitignore: true,
        extra_skip_dirs: vec![],
        data_file_max_bytes: 0,
        path_filter: vec![],
        context_embedding: Arc::new(RwLock::new(None)),
        context_summary: Arc::new(RwLock::new(None)),
        indexed_head_sha: Arc::new(RwLock::new(Some(first_sha))),
        last_indexed_at: Arc::new(RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        defer_embed: false,
        stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 0,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
            skip_kg: false,
            corpus_open_failed: false,
        }))),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
    });

    reconcile_one_index(Arc::clone(&handle)).await;

    let stored = handle.indexed_head_sha.read().await.clone();
    assert_eq!(
        stored,
        Some(current_sha),
        "indexed_head_sha must be updated to current HEAD"
    );
    assert!(
        handle.last_indexed_at.read().await.is_some(),
        "last_indexed_at must be stamped after reconcile"
    );
}
