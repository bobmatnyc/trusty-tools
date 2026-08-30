//! Unit tests for the `base_clone` doctor probe (issue #3605).
//!
//! Why: the condition being detected is a directory layout, so every case is
//! built on disk rather than mocked — a fixture that only *described* a severed
//! base would not prove the probe reads the same bytes git does.
//! What: builds the real `<base>/.git/worktrees/<name>` shape a `git worktree
//! add` produces, then deletes exactly what the 2026-07-21 incident deleted.
//! Test: this module IS the test module.

use super::*;

/// Build a base clone plus `count` linked worktrees, the way git lays them out.
///
/// Returns the base root and the worktree workspace paths.
fn base_with_worktrees(root: &Path, name: &str, count: usize) -> (PathBuf, Vec<PathBuf>) {
    let base = root.join(name);
    let common = base.join(".git");
    std::fs::create_dir_all(common.join("objects")).unwrap();
    std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let mut workspaces = Vec::new();
    for i in 0..count {
        let id = format!("wt-{i}");
        let admin = common.join("worktrees").join(&id);
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let workspace = root.join(&id);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        workspaces.push(workspace);
    }
    (base, workspaces)
}

#[test]
fn base_clone_ok_with_no_live_workspaces() {
    let check = check_base_clones(&[]);
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn base_clone_ok_for_a_healthy_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (_base, workspaces) = base_with_worktrees(dir.path(), "proj", 2);
    let check = check_base_clones(&workspaces);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("2 live worktree(s)"),
        "{}",
        check.message
    );
}

#[test]
fn base_clone_ignores_a_plain_checkout() {
    // A workspace whose `.git` is a DIRECTORY is its own clone — it has no base
    // clone to lose, and must never be counted or reported.
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(plain.join(".git").join("objects")).unwrap();
    let check = check_base_clones(&[plain]);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("0 live worktree(s)"),
        "{}",
        check.message
    );
}

#[test]
fn base_clone_fails_when_the_admin_dir_is_gone() {
    // The 2026-07-21 signature: the base clone's git internals are deleted
    // while the worktrees still point at them.
    let dir = tempfile::tempdir().unwrap();
    let (base, workspaces) = base_with_worktrees(dir.path(), "proj", 1);
    std::fs::remove_dir_all(base.join(".git").join("worktrees")).unwrap();

    let check = check_base_clones(&workspaces);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains(&base.display().to_string()),
        "the failing base must be named: {}",
        check.message
    );
    assert!(
        check.message.contains("1 live worktree(s)"),
        "the blast radius must be reported: {}",
        check.message
    );
    assert!(
        check.message.contains("Do NOT recursively delete"),
        "remediation must preserve the quarantine discipline: {}",
        check.message
    );
}

#[test]
fn base_clone_fails_when_the_object_database_is_gone() {
    // A partial deletion that spares `worktrees/` still leaves every git
    // command in the worktree failing, so the admin-dir probe alone is not
    // enough.
    let dir = tempfile::tempdir().unwrap();
    let (base, workspaces) = base_with_worktrees(dir.path(), "proj", 1);
    std::fs::remove_dir_all(base.join(".git").join("objects")).unwrap();

    let check = check_base_clones(&workspaces);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("no object database"),
        "{}",
        check.message
    );
}

#[test]
fn base_clone_counts_every_worktree_behind_one_base() {
    // The count is what turns "a base is broken" into "and this much work is
    // sitting behind it" — the number the original incident had to be
    // reconstructed by hand.
    let dir = tempfile::tempdir().unwrap();
    let (base, workspaces) = base_with_worktrees(dir.path(), "proj", 5);
    std::fs::remove_dir_all(base.join(".git")).unwrap();

    let check = check_base_clones(&workspaces);
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("5 live worktree(s)"),
        "{}",
        check.message
    );
}

#[test]
fn base_clone_reports_the_legacy_bare_layout_too() {
    // #4270 moved the base clone off the bare `.base` shape, but a machine that
    // ran an older build still has `<base>/worktrees/<id>` on disk, and that
    // layout is what the incident actually hit.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join(".base");
    let admin = base.join("worktrees").join("wt-0");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(base.join("objects")).unwrap();
    std::fs::write(base.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let workspace = dir.path().join("wt-0");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();

    assert_eq!(
        check_base_clones(std::slice::from_ref(&workspace)).status,
        CheckStatus::Ok
    );

    // Now delete the bare clone's non-dot internals, exactly as the incident's
    // `rm -rf .base/*` did.
    std::fs::remove_dir_all(base.join("worktrees")).unwrap();
    std::fs::remove_dir_all(base.join("objects")).unwrap();
    std::fs::remove_file(base.join("HEAD")).unwrap();

    let check = check_base_clones(&[workspace]);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains(&base.display().to_string()),
        "{}",
        check.message
    );
}
