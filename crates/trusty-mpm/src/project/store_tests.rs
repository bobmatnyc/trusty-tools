//! Unit tests for [`super::ProjectStore`].
//!
//! Why: extracted from `store.rs`'s inline `#[cfg(test)] mod tests` — the
//! lost-update fix added concurrency coverage that pushed that file toward the
//! 500-SLOC production cap (the whole file, prod + inline tests, counts against
//! the production cap since its basename doesn't match the test-file naming
//! convention). Follows this crate's established colocated-test-file pattern
//! (`registry.rs` + `registry_tests.rs`). The pre-existing tests are pure code
//! motion — no behavior or assertion change.
//! What: load/save round-trip, idempotent upsert, list/get, reload-on-external-
//! write, field-preserving JSON round-trip, the absent-file and non-NotFound I/O
//! paths, and the concurrency guarantees of the locked write path.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use tempfile::TempDir;

fn make_project(name: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: format!("https://github.com/owner/{name}"),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

#[tokio::test]
async fn store_load_save_round_trip() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load empty");
    assert!(store.all().await.expect("all").is_empty());

    store.upsert(make_project("alpha")).await.expect("upsert");

    let mut store2 = ProjectStore::load(dir.path()).await.expect("reload");
    let p = store2.get("alpha").await.expect("get after reload");
    assert_eq!(p.name, "alpha");
}

#[tokio::test]
async fn store_upsert_idempotent() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");

    let p1 = make_project("beta");
    store.upsert(p1).await.expect("first upsert");

    // Update the description — must replace, not duplicate.
    let p2 = Project {
        description: Some("updated".into()),
        ..make_project("beta")
    };
    store.upsert(p2).await.expect("second upsert");

    let all = store.all().await.expect("all");
    assert_eq!(all.len(), 1, "idempotent upsert must not duplicate");
    assert_eq!(all[0].description.as_deref(), Some("updated"));
}

#[tokio::test]
async fn store_list_and_get() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");

    store
        .upsert(make_project("gamma"))
        .await
        .expect("upsert gamma");
    store
        .upsert(make_project("delta"))
        .await
        .expect("upsert delta");

    let all = store.all().await.expect("all");
    assert_eq!(all.len(), 2);

    let p = store.get("gamma").await.expect("get gamma");
    assert_eq!(p.name, "gamma");

    let err = store.get("missing").await;
    assert!(
        matches!(err, Err(ProjectStoreError::NotFound(_))),
        "expected NotFound"
    );
}

#[tokio::test]
async fn store_reload_picks_up_external_write() {
    let dir = TempDir::new().expect("tempdir");

    let mut store_a = ProjectStore::load(dir.path()).await.expect("load A");
    store_a
        .upsert(make_project("epsilon"))
        .await
        .expect("seed A");

    // Simulate a second process writing a new project.
    let mut store_b = ProjectStore::load(dir.path()).await.expect("load B");
    store_b.upsert(make_project("zeta")).await.expect("write B");

    // Store A must pick up the external write on its next read.
    let all = store_a.all().await.expect("all A after external write");
    let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    assert!(
        names.contains(&"zeta"),
        "store A must reload zeta: {names:?}"
    );
}

#[tokio::test]
async fn store_reload_noop_when_unchanged() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");
    store.upsert(make_project("eta")).await.expect("upsert");

    // No external write — reload must be a no-op that preserves data.
    store.reload_if_changed().await.expect("reload no-op");
    let p = store.get("eta").await.expect("get");
    assert_eq!(p.name, "eta");
}

#[tokio::test]
async fn json_round_trip_preserves_all_fields() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");

    let full = Project {
        name: "full-project".into(),
        repo_url: "https://github.com/owner/full-project.git".into(),
        default_branch: "develop".into(),
        stack_hint: Some("typescript".into()),
        tags: vec!["frontend".into(), "oss".into()],
        description: Some("a fully-populated project".into()),
        gh_user: Some("bobmatnyc".into()),
        gh_account: Some("bobmatnyc".into()),
        github: Some(crate::core::trusty_tools_config::GithubConfig {
            config_dir: Some("/home/bob/.config/gh-full".into()),
            token_env: None,
            account: None,
            host: Some("github.example.com".into()),
        }),
        commit_name: Some("Full Bot".into()),
        commit_email: Some("full-bot@example.com".into()),
        worktree: None,
    };
    store.upsert(full.clone()).await.expect("upsert full");

    let mut store2 = ProjectStore::load(dir.path()).await.expect("reload");
    let back = store2.get("full-project").await.expect("get");
    assert_eq!(back, full);
}

/// Verify that a NotFound path still starts with an empty store (happy-path
/// absence handling) and that a subsequent upsert+reload round-trips correctly.
///
/// Why: the bug fix narrows the "start fresh" arm to NotFound only; this test
/// confirms that the intended absent-file path still works as before.
/// What: loads from a directory that contains no `projects.json`, asserts the
/// store is empty, then writes and reloads to confirm persistence works.
/// Test: this IS the test.
#[tokio::test]
async fn store_not_found_starts_fresh() {
    let dir = TempDir::new().expect("tempdir");
    // No projects.json exists yet — load must return an empty store.
    let mut store = ProjectStore::load(dir.path())
        .await
        .expect("not-found should start fresh");
    assert!(
        store.all().await.expect("all").is_empty(),
        "fresh store must be empty"
    );

    // Confirm we can round-trip through a save after starting fresh.
    store
        .upsert(make_project("theta"))
        .await
        .expect("upsert after fresh load");
    let mut store2 = ProjectStore::load(dir.path()).await.expect("reload");
    assert_eq!(store2.get("theta").await.expect("get theta").name, "theta");
}

/// Verify that a non-NotFound I/O error (e.g. a directory occupying the file
/// path) is propagated rather than silently treated as absent.
///
/// Why: the data-loss bug caused any I/O error to be swallowed, so the next
/// `save()` would overwrite projects.json with an empty store. This test
/// exercises the fix by pointing `read_file` at a directory, which produces
/// `IsADirectory` (or equivalent) — not `NotFound`.
/// What: creates a directory at `projects.json`'s expected path, then calls
/// `ProjectStore::load`; asserts the result is an `Io` error, not `Ok`.
/// Test: this IS the test.
#[tokio::test]
async fn store_other_io_error_propagates() {
    let dir = TempDir::new().expect("tempdir");
    // Plant a directory where projects.json would live so read_to_string
    // returns an OS error that is NOT NotFound.
    let blocking_dir = dir.path().join("projects.json");
    tokio::fs::create_dir_all(&blocking_dir)
        .await
        .expect("create blocking dir");

    let result = ProjectStore::load(dir.path()).await;
    assert!(
        matches!(result, Err(ProjectStoreError::Io(_))),
        "expected Io error when path is a directory, got: {result:?}"
    );
}

/// Concurrent writers must not lose each other's entries, even in-process.
///
/// Why: the cross-process regression test lives in
/// `crates/trusty-mpm/tests/projects_json_concurrency.rs`; this is its cheap
/// in-crate companion. Each task owns a SEPARATE `ProjectStore` (hence a
/// separate lock descriptor), so nothing but the file lock serialises them —
/// against the pre-fix store this loses entries and can corrupt the file.
/// What: 6 concurrent tasks each upsert 5 distinct projects into one directory;
/// all 30 must be present afterwards.
/// Test: this IS the test.
#[tokio::test]
async fn store_concurrent_tasks_do_not_lose_writes() {
    let dir = TempDir::new().expect("tempdir");
    let mut handles = Vec::new();
    for task in 0..6u32 {
        let root = dir.path().to_path_buf();
        handles.push(tokio::spawn(async move {
            let mut store = ProjectStore::load(&root).await.expect("load");
            for i in 0..5u32 {
                store
                    .upsert(make_project(&format!("t{task}-{i}")))
                    .await
                    .expect("concurrent upsert");
            }
        }));
    }
    for h in handles {
        h.await.expect("task join");
    }

    let mut store = ProjectStore::load(dir.path()).await.expect("verify load");
    let all = store.all().await.expect("all");
    assert_eq!(
        all.len(),
        30,
        "lost concurrent upserts: {} of 30",
        all.len()
    );
}

/// Repeated contention on the same store must complete, not deadlock.
///
/// Why: an exclusive lock taken on a path that is already held — or taken twice
/// without release — hangs forever rather than failing. This test would time out
/// (rather than fail) if `mutate` ever leaked a guard or became reentrant.
/// What: hammers one directory with interleaved upserts and reads from several
/// tasks, under a wall-clock timeout that is generous for the work but far
/// shorter than a hang.
/// Test: this IS the test.
#[tokio::test]
async fn store_lock_contention_does_not_deadlock() {
    let dir = TempDir::new().expect("tempdir");
    let work = async {
        let mut handles = Vec::new();
        for task in 0..4u32 {
            let root = dir.path().to_path_buf();
            handles.push(tokio::spawn(async move {
                let mut store = ProjectStore::load(&root).await.expect("load");
                for i in 0..10u32 {
                    store
                        .upsert(make_project(&format!("c{task}-{i}")))
                        .await
                        .expect("upsert");
                    // Interleave a read; readers must never block on the writer.
                    let _ = store.all().await.expect("all");
                }
            }));
        }
        for h in handles {
            h.await.expect("task join");
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), work)
        .await
        .expect("lock contention deadlocked");

    let mut store = ProjectStore::load(dir.path()).await.expect("verify load");
    assert_eq!(store.all().await.expect("all").len(), 40);
}

/// An unacquirable write lock must return an error, never write unlocked.
///
/// Why: the repo has a recurring bug shape where a failure branch advances state
/// anyway and the alarms read healthy. A store that fell back to an
/// unsynchronised write when locking failed would be exactly that: silently
/// racy again, with no signal.
/// What: plants a directory where the `projects.json.lock` sidecar belongs so it
/// cannot be opened, then asserts the upsert errors AND that the pre-existing
/// record is still intact.
/// Test: this IS the test.
#[tokio::test]
async fn store_lock_failure_is_not_fail_open() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");
    store.upsert(make_project("kappa")).await.expect("seed");

    let sidecar = dir.path().join("projects.json.lock");
    let _ = tokio::fs::remove_file(&sidecar).await;
    tokio::fs::create_dir_all(&sidecar)
        .await
        .expect("plant blocking dir");

    let result = store.upsert(make_project("lambda")).await;
    assert!(
        matches!(result, Err(ProjectStoreError::Lock(_))),
        "expected Lock error, got {result:?}"
    );

    // Remove the blocker and confirm the seeded record was never disturbed.
    tokio::fs::remove_dir_all(&sidecar).await.expect("unblock");
    let mut verify = ProjectStore::load(dir.path()).await.expect("verify load");
    let all = verify.all().await.expect("all");
    assert_eq!(all.len(), 1, "failed lock must not have written");
    assert_eq!(all[0].name, "kappa");
}

/// A successful write leaves no scratch file behind.
///
/// Why: the pre-fix store wrote to one FIXED `projects.json.tmp` shared by every
/// writer, which is how concurrent writers corrupted the registry. A leftover
/// temp file is the visible symptom of that design.
/// What: performs two upserts and asserts the directory holds no `*.tmp` entry.
/// Test: this IS the test.
#[tokio::test]
async fn store_leaves_no_temp_file_behind() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");
    store.upsert(make_project("mu")).await.expect("upsert mu");
    store.upsert(make_project("nu")).await.expect("upsert nu");

    let mut leftovers = Vec::new();
    let mut entries = tokio::fs::read_dir(dir.path()).await.expect("read_dir");
    while let Some(entry) = entries.next_entry().await.expect("next entry") {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tmp") {
            leftovers.push(name);
        }
    }
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

/// A stale temp file from a crashed writer must not affect a later write.
///
/// Why: crash recovery — an orphaned scratch file is expected debris after a
/// killed writer and must never be mistaken for, or block, the real document.
/// What: plants both a legacy fixed-name `projects.json.tmp` and a stale
/// unique-name temp, then asserts a subsequent upsert succeeds and the registry
/// still reads back correctly.
/// Test: this IS the test.
#[tokio::test]
async fn store_survives_stale_temp_files() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = ProjectStore::load(dir.path()).await.expect("load");
    store.upsert(make_project("xi")).await.expect("seed");

    tokio::fs::write(dir.path().join("projects.json.tmp"), b"{ garbage")
        .await
        .expect("plant legacy temp");
    tokio::fs::write(dir.path().join("projects.json.999.1.tmp"), b"{ garbage")
        .await
        .expect("plant stale temp");

    store.upsert(make_project("omicron")).await.expect("upsert");

    let mut verify = ProjectStore::load(dir.path()).await.expect("verify load");
    let all = verify.all().await.expect("all");
    assert_eq!(all.len(), 2, "stale temp files disturbed the registry");
}
