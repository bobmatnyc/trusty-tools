//! Tests for the three-outcome child-repository scan (#7673 round 2 review).
//!
//! A `.git` directory made with `create_dir_all` is all the scan looks for, so
//! these fixtures need no `git` binary.

use super::*;
use tempfile::TempDir;

/// A canonical `<tmp>/workspace` directory, created.
fn workspace(tmp: &TempDir) -> PathBuf {
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("workspace");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Create `count` empty subdirectories of `dir`.
fn fill(dir: &Path, count: usize) {
    for i in 0..count {
        std::fs::create_dir_all(dir.join(format!("d{i:03}"))).unwrap();
    }
}

/// Restores a directory's mode on drop so `TempDir` can clean it up even when
/// an assertion panics first.
#[cfg(unix)]
struct RestoreMode(PathBuf);

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// FAILS BEFORE THIS ROUND: `node_modules` (created first) held more
/// subdirectories than the budget, so a breadth-first scan that dequeued it
/// before `packages` ran out and reported absence. `node_modules` is now
/// skipped uncharged, so the child repository is found whatever order
/// `read_dir` returns.
#[test]
fn a_wide_node_modules_does_not_hide_a_child_repository() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    fill(&ws.join("node_modules"), WORKSPACE_SCAN_BUDGET + 44);
    let app = ws.join("packages").join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Found(app));
}

/// FAILS BEFORE THIS ROUND: exhausting the budget returned the same `None` as
/// genuine absence.
#[test]
fn a_directory_wider_than_the_budget_is_incomplete() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    fill(&ws, WORKSPACE_SCAN_BUDGET + 44);

    assert_eq!(
        scan_for_child_repo(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::BudgetExhausted)
    );
}

/// A tree that fits the budget and holds no repository is the one `Clear`.
#[test]
fn a_directory_within_the_budget_with_no_repository_is_clear() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    fill(&ws, WORKSPACE_SCAN_BUDGET);

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Clear);
}

/// A seed site the pipeline has not created yet has no descendants, so it is
/// `Clear`, not an unreadable directory; `pipeline_creates_claude_md` seeds
/// exactly that shape.
#[test]
fn a_root_that_does_not_exist_yet_is_clear() {
    let tmp = TempDir::new().unwrap();
    let missing = workspace(&tmp).join("not-created-yet");

    assert_eq!(scan_for_child_repo(&missing), ChildRepoScan::Clear);
}

/// FAILS BEFORE THIS ROUND: a repository past the budget down a single chain
/// was reported as absent.
#[test]
fn a_deep_single_chain_beyond_the_budget_is_incomplete() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let mut deep = ws.clone();
    for _ in 0..(WORKSPACE_SCAN_BUDGET + 50) {
        deep = deep.join("d");
    }
    std::fs::create_dir_all(deep.join(".git")).unwrap();

    assert_eq!(
        scan_for_child_repo(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::BudgetExhausted)
    );
}

/// A pnpm/yarn scoped package three levels down is within reach: the budget
/// bounds directories visited, not depth.
#[test]
fn a_scoped_package_repository_three_levels_down_is_found() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let deep = ws.join("packages").join("@scope").join("pkg-a");
    std::fs::create_dir_all(deep.join(".git")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Found(deep));
}

/// FAILS BEFORE THIS ROUND: an unreadable child contributed nothing, so the
/// scan reported `None` without having looked inside it.
#[cfg(unix)]
#[test]
fn an_unreadable_child_directory_is_incomplete() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let locked = ws.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(locked.clone());
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("#7673 tests: running with permission overrides (root?), skipping");
        return;
    }

    match scan_for_child_repo(&ws) {
        ChildRepoScan::Incomplete(ScanIncomplete::Unreadable { path, .. }) => {
            assert_eq!(path, locked);
        }
        other => panic!("an unreadable child must stop the scan, got {other:?}"),
    }
}

/// FAILS BEFORE THIS ROUND: following two self-referencing symlinks doubled
/// the frontier every level, exhausting the budget before the real repository
/// ten levels down was dequeued. A symlink is never descended now, so the
/// cycle costs nothing, and a cycle with no repository scans `Clear` rather
/// than exhausting.
#[cfg(unix)]
#[test]
fn a_symlink_cycle_is_not_descended() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    symlink(&ws, ws.join("loop_a")).unwrap();
    symlink(&ws, ws.join("loop_b")).unwrap();
    let mut repo = ws.join("chain");
    for i in 1..10 {
        repo = repo.join(format!("c{i}"));
    }
    std::fs::create_dir_all(repo.join(".git")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Found(repo));

    let bare = ws.join("chain").join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    symlink(&bare, bare.join("self")).unwrap();
    assert_eq!(scan_for_child_repo(&bare), ChildRepoScan::Clear);
}

/// FAILS BEFORE THIS ROUND: `is_dir` followed a symlink out of the target, so
/// a repository elsewhere on disk refused a directory that holds none.
#[cfg(unix)]
#[test]
fn a_symlink_to_an_outside_directory_is_not_traversed() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let outside = ws.parent().unwrap().join("outside");
    std::fs::create_dir_all(outside.join("proj").join(".git")).unwrap();
    symlink(&outside, ws.join("elsewhere")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Clear);
}

/// A symlinked child whose target IS a repository still counts: the `.git`
/// check looks through the link without descending it.
#[cfg(unix)]
#[test]
fn a_symlink_to_a_repository_counts_as_found() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let repo = ws.parent().unwrap().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let linked = ws.join("linked");
    symlink(&repo, &linked).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Found(linked));
}

/// FAILS BEFORE ROUND 3: a skip-listed name was skipped before its `.git` was
/// checked, so a vendored submodule one level under `vendor/` scanned `Clear`.
#[test]
fn a_repository_under_a_skip_listed_name_is_found() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let lib = ws.join("vendor").join("some-lib");
    std::fs::create_dir_all(lib.join(".git")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Found(lib));
}

/// FAILS BEFORE ROUND 3: a skip-listed directory that is itself a repository
/// scanned `Clear`.
#[test]
fn a_repository_that_is_a_skip_listed_directory_is_found() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let build = ws.join("build");
    std::fs::create_dir_all(build.join(".git")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Found(build));
}

/// FAILS BEFORE ROUND 3: a skip-listed directory was never read, so one too
/// wide to check still scanned `Clear`.
#[test]
fn a_skip_listed_directory_wider_than_its_check_cap_is_incomplete() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let node_modules = ws.join("node_modules");
    fill(&node_modules, SKIP_DIR_CHECK_CAP + 1);

    assert_eq!(
        scan_for_child_repo(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SkipDirTooWide { path: node_modules })
    );
}

/// FAILS BEFORE ROUND 3: the symlink arm used `.exists()`, which reads a
/// permission error as "no `.git`", so a locked linked repository was `Clear`.
#[cfg(unix)]
#[test]
fn an_unreadable_symlink_target_is_incomplete() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let locked = ws.parent().unwrap().join("locked-repo");
    std::fs::create_dir_all(locked.join(".git")).unwrap();
    let linked = ws.join("linked");
    symlink(&locked, &linked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(locked.clone());
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("#7673 tests: running with permission overrides (root?), skipping");
        return;
    }

    match scan_for_child_repo(&ws) {
        ChildRepoScan::Incomplete(ScanIncomplete::Unreadable { path, .. }) => {
            assert_eq!(path, linked);
        }
        other => panic!("an unreadable symlink target must stop the scan, got {other:?}"),
    }
}

/// A symlink to a file or to nothing cannot be a repository, so it neither
/// stops the scan nor counts as found (`AGENTS.md -> CLAUDE.md` is common).
#[cfg(unix)]
#[test]
fn a_symlink_to_a_file_or_to_nothing_is_not_a_repository() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::write(ws.join("CLAUDE.md"), "x").unwrap();
    symlink(ws.join("CLAUDE.md"), ws.join("AGENTS.md")).unwrap();
    symlink(ws.join("missing"), ws.join("dangling")).unwrap();
    std::fs::create_dir_all(ws.join("vendor")).unwrap();
    symlink(ws.join("CLAUDE.md"), ws.join("vendor").join("linked-file")).unwrap();

    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Clear);
}

/// Ratchet: `Clear` is built once, as the tail expression after the queue
/// loop, so no early exit can answer "clear" (#7673 round 3 review).
#[test]
fn clear_is_constructed_in_one_place() {
    let source = include_str!("child_repo_scan.rs");
    let code: Vec<&str> = source
        .lines()
        .take_while(|l| !l.trim_start().starts_with("#[cfg(test)]"))
        .map(|l| l.split("//").next().unwrap_or("").trim())
        .collect();
    let needle = "ChildRepoScan::Clear";
    let sites: Vec<usize> = (0..code.len())
        .filter(|&i| code[i].contains(needle))
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "`{needle}` must be constructed exactly once"
    );
    let site = sites[0];
    assert_eq!(
        code[site], needle,
        "`Clear` must be a bare tail expression, not a mapped arm"
    );

    let loop_start = code
        .iter()
        .position(|l| *l == "while let Some(current) = queue.pop_front() {")
        .expect("the walk drains a queue");
    assert!(loop_start < site, "`Clear` must follow the queue loop");
    assert!(
        code[loop_start..site].iter().all(|l| !l.contains("fn ")),
        "`Clear` must sit in the function that drains the queue"
    );
    let word_uses = code
        .iter()
        .filter(|l| {
            l.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == "Clear")
        })
        .count();
    assert_eq!(
        word_uses, 2,
        "only the variant declaration and its one construction name `Clear`"
    );
}
