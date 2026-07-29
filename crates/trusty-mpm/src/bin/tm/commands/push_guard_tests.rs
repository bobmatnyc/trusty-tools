//! Tests for `tm repair push-guard` (#2867).
//!
//! Why: this command is the ONLY supported way to protect a clone that was
//! provisioned before the guard shipped, so its exit codes and its refusal
//! behaviour are the contract an operator (or a script) relies on.
//! What: real temp git repos, no mocks. Covers the fresh install, idempotency,
//! the dry run writing nothing, a foreign-hook refusal exiting non-zero, and
//! path resolution.
//! Test: this file IS the test module.

use super::*;

/// A real, minimal git repo, or `None` when `git` is unavailable.
fn temp_repo() -> Option<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("tm-test-repairguard-")
        .tempdir()
        .ok()?;
    let path = dir.path().to_path_buf();
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&path)
        .status()
        .ok()?;
    if !ok.success() {
        return None;
    }
    Some((dir, path))
}

#[test]
fn installs_into_a_fresh_repo_then_is_idempotent() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let repo_str = repo.to_string_lossy().to_string();

    repair_push_guard(Some(repo_str.clone()), false).expect("a fresh retrofit must succeed");
    let hooks = effective_hooks_dir(&repo).expect("hooks dir");
    assert!(hooks.join("pre-push").exists(), "the hook must be on disk");

    repair_push_guard(Some(repo_str), false)
        .expect("re-running an up-to-date guard must still succeed (idempotent)");
    assert!(matches!(
        inspect_pre_push_guard(&repo),
        GuardState::Current(_)
    ));
}

#[test]
fn dry_run_writes_nothing() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    repair_push_guard(Some(repo.to_string_lossy().to_string()), true).expect("dry run");
    let hooks = effective_hooks_dir(&repo).expect("hooks dir");
    assert!(
        !hooks.join("pre-push").exists(),
        "a dry run must never create the hook"
    );
}

#[test]
fn refusal_exits_nonzero_and_leaves_the_foreign_hook_intact() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let hooks = effective_hooks_dir(&repo).expect("hooks dir");
    std::fs::create_dir_all(&hooks).expect("mkdir hooks");
    let foreign = "#!/bin/sh\n# husky\nexit 0\n";
    std::fs::write(hooks.join("pre-push"), foreign).expect("seed foreign hook");

    assert!(
        repair_push_guard(Some(repo.to_string_lossy().to_string()), false).is_err(),
        "a refusal means UNPROTECTED and must not read as success to a script"
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join("pre-push")).expect("read"),
        foreign,
        "the foreign hook must be untouched"
    );
}

#[test]
fn unresolvable_path_exits_nonzero() {
    let dir = tempfile::Builder::new()
        .prefix("tm-test-repairguard-nogit-")
        .tempdir()
        .expect("temp dir");
    assert!(
        repair_push_guard(Some(dir.path().to_string_lossy().to_string()), false).is_err(),
        "a directory outside any git working tree must fail loudly"
    );
    assert!(repair_push_guard(Some("/nonexistent/tm/test/path".to_string()), false).is_err());
}

/// A BARE repository must retrofit successfully.
///
/// Why: trusty-mpm's own managed base clone is bare, and the operator-
/// authorised retrofit points this command straight at it — so `--path <bare>`
/// is the headline use case, not an edge case. The first revision resolved the
/// target with `git rev-parse --show-toplevel`, which FATALS in a bare repo
/// ("this operation must be run in a work tree"), so the command rejected its
/// most obvious argument outright.
#[test]
fn retrofits_a_bare_repository() {
    let dir = tempfile::Builder::new()
        .prefix("tm-test-repairguard-bare-")
        .tempdir()
        .expect("temp dir");
    let bare = dir.path().join("base.git");
    let ok = std::process::Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main"])
        .arg(&bare)
        .status();
    match ok {
        Ok(s) if s.success() => {}
        _ => return,
    }

    repair_push_guard(Some(bare.to_string_lossy().to_string()), false)
        .expect("a bare repository must retrofit, not fail on --show-toplevel");
    assert!(
        bare.join("hooks").join("pre-push").exists(),
        "the guard must land in the bare repo's own hooks dir"
    );
    assert!(matches!(
        inspect_pre_push_guard(&bare),
        GuardState::Current(_)
    ));

    // …and the dry run must work against a bare repo too.
    repair_push_guard(Some(bare.to_string_lossy().to_string()), true).expect("bare dry run");
}

/// A retrofit run from a LINKED worktree must protect the whole clone — that
/// is the property that makes one invocation cover a 95-worktree base.
#[test]
fn worktree_count_counts_linked_worktrees() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git")
            .status
            .success()
    };
    std::fs::write(repo.join("README"), b"seed").expect("write");
    assert!(git(&["add", "."]));
    assert!(git(&["commit", "-qm", "seed"]));
    assert_eq!(worktree_count(&repo), "1");

    let linked = repo.join("linked");
    assert!(git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "side",
        linked.to_str().expect("utf8")
    ]));
    assert_eq!(worktree_count(&repo), "2");

    // Retrofitting FROM the linked worktree must land in the shared hooks dir,
    // so the base checkout is protected too.
    repair_push_guard(Some(linked.to_string_lossy().to_string()), false)
        .expect("retrofit from a linked worktree");
    assert!(matches!(
        inspect_pre_push_guard(&repo),
        GuardState::Current(_)
    ));
}
