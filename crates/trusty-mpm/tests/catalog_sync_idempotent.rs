//! Integration tests for idempotent catalog sync (issue #1751).
//!
//! Why: `CatalogSync::sync` previously called `git clone` unconditionally; a
//! second sync failed with "destination already exists". This file exercises the
//! three decision branches of `ensure_repo` (clone, update, re-clone) and the
//! URL-normalisation + safety-guard helpers via the public API.
//! What: uses only the public surface of `trusty_mpm` so these tests remain
//! valid across refactors of the private helpers.
//! Test: all tests in this file are the tests.

use trusty_mpm::content::CatalogSync;
use trusty_mpm::provisioner::FakeGitBackend;

fn make_sync(dir: &std::path::Path) -> CatalogSync<FakeGitBackend> {
    CatalogSync::with_repo(
        FakeGitBackend::new(),
        dir.to_owned(),
        "https://github.com/bobmatnyc/claude-mpm",
        "main",
    )
}

/// Why: regression test for #1751 — a second forced sync must succeed via the
/// update path (fetch+reset), not fail with "already exists" from git clone.
/// What: first sync clones, second forced sync updates in place.
/// Test: this is the test.
#[test]
fn catalog_sync_second_sync_succeeds() {
    let root = tempfile::TempDir::new().unwrap();
    let sync = make_sync(root.path());

    let r1 = sync.sync(false).unwrap();
    assert!(r1.fetched, "first sync must fetch");
    assert!(
        root.path().join("repo").exists(),
        "repo dir must be created"
    );

    // Force a second sync — repo dir exists with valid .git; must NOT fail with
    // "already exists" but take the update path (fetch+reset via FakeGitBackend).
    let r2 = sync.sync(true).unwrap();
    assert!(r2.fetched, "forced second sync must report fetched=true");
}

/// Why: if the repo dir exists but is not a git repo (e.g. leftover temp files),
/// sync must recover by removing it and re-cloning rather than failing.
/// What: creates a non-git directory at the repo path, then forces a sync.
/// Test: this is the test.
#[test]
fn catalog_sync_corrupt_dir_reclones() {
    let root = tempfile::TempDir::new().unwrap();
    let sync = make_sync(root.path());

    let repo_path = root.path().join("repo");
    std::fs::create_dir_all(&repo_path).unwrap();
    std::fs::write(repo_path.join("junk.txt"), "not a git repo").unwrap();

    let result = sync.sync(true).unwrap();
    assert!(result.fetched, "sync after corrupt dir must report fetched");
    assert!(
        repo_path.join(".git").is_dir(),
        ".git must exist after recovery"
    );
}

/// Why: if the existing checkout points at a different remote the catalog would
/// be stale; sync must detect the mismatch and re-clone from the correct remote.
/// What: first sync uses URL A, a second CatalogSync with URL B force-syncs into
/// the same catalog dir and re-clones.
/// Test: this is the test.
#[test]
fn catalog_sync_wrong_remote_reclones() {
    let root = tempfile::TempDir::new().unwrap();

    let sync_a = CatalogSync::with_repo(
        FakeGitBackend::new(),
        root.path().to_owned(),
        "https://github.com/owner/repo-a",
        "main",
    );
    sync_a.sync(true).unwrap();
    let repo_path = root.path().join("repo");
    assert!(
        repo_path.join(".git").is_dir(),
        "repo-a clone must create .git"
    );

    let sync_b = CatalogSync::with_repo(
        FakeGitBackend::new(),
        root.path().to_owned(),
        "https://github.com/owner/repo-b",
        "main",
    );
    let result = sync_b.sync(true).unwrap();
    assert!(result.fetched, "sync with wrong remote must fetch");
    let config = std::fs::read_to_string(repo_path.join(".git").join("config")).unwrap();
    assert!(
        config.contains("repo-b"),
        "config must be updated to repo-b URL"
    );
}
