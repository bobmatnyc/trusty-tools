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
        ChildRepoScan::Incomplete(ScanIncomplete::BudgetExhausted {
            budget: WORKSPACE_SCAN_BUDGET
        })
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
        ChildRepoScan::Incomplete(ScanIncomplete::BudgetExhausted {
            budget: WORKSPACE_SCAN_BUDGET
        })
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

/// #8878 R1: the excepted entry is neither found nor descended, and a
/// repository elsewhere, also one with the same name deeper down, is found.
#[test]
fn an_excepted_entry_is_skipped_and_its_siblings_are_still_found() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let local = ws.join("local");
    std::fs::create_dir_all(local.join(".git")).unwrap();
    std::fs::create_dir_all(local.join("nested/.git")).unwrap();
    assert_eq!(
        scan_for_child_repo(&ws),
        ChildRepoScan::Found(local.clone())
    );
    assert_eq!(
        scan_for_child_repo_except(&ws, &local),
        ChildRepoScan::Clear
    );

    let deeper = ws.join("sub/local");
    std::fs::create_dir_all(deeper.join(".git")).unwrap();
    assert_eq!(
        scan_for_child_repo_except(&ws, &local),
        ChildRepoScan::Found(deeper)
    );
}

/// The exhaustive rule with a roomy budget.
fn exhaustive(dir: &Path) -> ChildRepoScan {
    scan_for_child_repo_exhaustive(dir, None, 4096)
}

/// #10457: a repository two or more levels inside a skip-listed directory is
/// found, at any depth of the tree, which the shallow rule never read.
#[test]
fn exhaustive_finds_a_repository_deep_in_a_skip_listed_dir() {
    for rel in [
        "node_modules/a/b",
        "build/x/proj",
        "packages/app/node_modules/@scope/pkg/nested",
    ] {
        let tmp = TempDir::new().unwrap();
        let ws = workspace(&tmp);
        let repo = ws.join(rel);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(exhaustive(&ws), ChildRepoScan::Found(repo), "{rel}");
    }
}

/// #10457: a directory symlink out of the root refuses, with or without a
/// repository behind it.
#[cfg(unix)]
#[test]
fn exhaustive_reports_an_escaping_symlink() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let outside = ws.parent().unwrap().join("outside");
    std::fs::create_dir_all(outside.join("sub")).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Clear);

    let link = ws.join("deep/elsewhere");
    std::fs::create_dir_all(ws.join("deep")).unwrap();
    symlink(&outside, &link).unwrap();
    let escape = ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes {
        link: link.clone(),
        target: outside.clone(),
    });
    assert_eq!(exhaustive(&ws), escape);
    std::fs::create_dir_all(outside.join("sub/.git")).unwrap();
    assert_eq!(exhaustive(&ws), escape);
    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Clear);
}

/// #10457: links that stay inside the root (a pnpm store, a cycle to the
/// root) are already walked, so they are skipped and cannot loop; a link
/// into the unwalked `.git` tree refuses.
#[cfg(unix)]
#[test]
fn exhaustive_skips_a_symlink_inside_the_root_and_cannot_cycle() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let store = ws.join("node_modules/.pnpm/pkg@1/node_modules/pkg");
    std::fs::create_dir_all(&store).unwrap();
    symlink(&store, ws.join("node_modules/pkg")).unwrap();
    symlink(&ws, ws.join("loop")).unwrap();
    symlink(Path::new(".."), store.join("up")).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Clear);

    std::fs::create_dir_all(store.join("inner/.git")).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Found(store.join("inner")));

    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::create_dir_all(ws.join(".git/modules/x")).unwrap();
    let link = ws.join("hidden");
    symlink(ws.join(".git/modules"), &link).unwrap();
    assert!(matches!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes { link: l, .. }) if l == link
    ));
}

/// #10457: skip-listed directories cost budget under the exhaustive rule, and
/// exhaustion names the budget and the next unchecked directory.
#[test]
fn exhaustive_charges_skip_listed_dirs_to_the_budget() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let node_modules = ws.join("node_modules");
    fill(&node_modules, 10);
    assert_eq!(
        scan_for_child_repo_within_shallow(&ws, 5),
        ChildRepoScan::Clear
    );
    match scan_for_child_repo_exhaustive(&ws, None, 5) {
        ChildRepoScan::Incomplete(stop @ ScanIncomplete::BudgetExhaustedAt { budget: 5, .. }) => {
            let text = stop.to_string();
            assert!(
                text.contains("stopped after checking 5 directories"),
                "{text}"
            );
            assert!(text.contains(&node_modules.display().to_string()), "{text}");
        }
        other => panic!("expected exhaustion, got {other:?}"),
    }
    assert_eq!(
        scan_for_child_repo_exhaustive(&ws, None, 11),
        ChildRepoScan::Clear
    );
}

/// The shallow rule with a named budget, to compare against the exhaustive one.
fn scan_for_child_repo_within_shallow(dir: &Path, budget: usize) -> ChildRepoScan {
    scan(dir, None, budget, None)
}

/// #10457: the probe still runs before the escape check, so a symlink whose
/// target is a repository is `Found`, not an escape.
#[cfg(unix)]
#[test]
fn exhaustive_still_finds_a_symlink_to_a_repository() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let repo = ws.parent().unwrap().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let linked = ws.join("linked");
    symlink(&repo, &linked).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Found(linked));
}

/// A fake `st_dev` reader: `mnt` is another filesystem, `bad` cannot be read.
fn fake_device(path: &Path) -> io::Result<u64> {
    match path.file_name().and_then(|n| n.to_str()) {
        Some("mnt") => Ok(2),
        Some("bad") => Err(io::Error::new(io::ErrorKind::PermissionDenied, "no stat")),
        _ => Ok(1),
    }
}

/// #10457: a directory on another device refuses, and so does one whose
/// `st_dev` cannot be read. A test cannot mount a filesystem without
/// privileges, so the reader is injected; `device_of` is the real one.
#[test]
fn exhaustive_refuses_a_mount_point_and_an_unreadable_device() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::create_dir_all(ws.join("a/b")).unwrap();
    assert_eq!(scan(&ws, None, 64, Some(fake_device)), ChildRepoScan::Clear);

    let mnt = ws.join("a/b/mnt");
    std::fs::create_dir_all(&mnt).unwrap();
    assert_eq!(
        scan(&ws, None, 64, Some(fake_device)),
        ChildRepoScan::Incomplete(ScanIncomplete::MountPoint { path: mnt.clone() })
    );
    std::fs::remove_dir(&mnt).unwrap();

    let bad = ws.join("a/bad");
    std::fs::create_dir_all(&bad).unwrap();
    assert!(matches!(
        scan(&ws, None, 64, Some(fake_device)),
        ChildRepoScan::Incomplete(ScanIncomplete::Unreadable { path, .. }) if path == bad
    ));
    // The real reader sees one filesystem across the scratch tree.
    std::fs::remove_dir(&bad).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Clear);
    assert_eq!(device_of(&ws).unwrap(), device_of(&ws.join("a")).unwrap());
}

/// #10457: the seed guard and `auto_git_init` keep the shallow rule, so a
/// repository deep in `node_modules` is still `Clear` for them. Pins the
/// caller decision recorded on the issue.
#[test]
fn the_shallow_rule_is_unchanged_for_the_seed_guard_and_auto_init() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::create_dir_all(ws.join("node_modules/a/b/.git")).unwrap();
    assert_eq!(scan_for_child_repo(&ws), ChildRepoScan::Clear);
    assert_eq!(
        scan_for_child_repo_except(&ws, &ws.join("local")),
        ChildRepoScan::Clear
    );
    assert!(matches!(exhaustive(&ws), ChildRepoScan::Found(_)));
}

/// #10457 critic MEDIUM: the excepted entry is scanned by the caller from its
/// own root, so only this walk can compare its device with the root's.
#[test]
fn exhaustive_refuses_an_excepted_entry_on_another_device() {
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let mnt = ws.join("mnt");
    let local = ws.join("local");
    std::fs::create_dir_all(&mnt).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    assert_eq!(
        scan(&ws, Some(&mnt), 64, Some(fake_device)),
        ChildRepoScan::Incomplete(ScanIncomplete::MountPoint { path: mnt.clone() })
    );
    std::fs::remove_dir(&mnt).unwrap();
    assert_eq!(
        scan(&ws, Some(&local), 64, Some(fake_device)),
        ChildRepoScan::Clear
    );
}

/// Deletes a root named `vanish` while reading its device, as a concurrent
/// `rm -rf` between the fence and the first `read_dir` would.
fn vanishing_device(path: &Path) -> io::Result<u64> {
    if path.file_name().is_some_and(|n| n == "vanish") {
        std::fs::remove_dir(path)?;
    }
    Ok(1)
}

/// #10457 critic LOW: a root that existed when the fence was built and is
/// gone at the first `read_dir` is unreadable, not empty.
#[test]
fn exhaustive_refuses_a_root_that_vanishes_mid_scan() {
    let tmp = TempDir::new().unwrap();
    let root = workspace(&tmp).join("vanish");
    std::fs::create_dir_all(&root).unwrap();
    assert!(matches!(
        scan(&root, None, 64, Some(vanishing_device)),
        ChildRepoScan::Incomplete(ScanIncomplete::Unreadable { path, .. }) if path == root
    ));
    assert_eq!(
        exhaustive(&root),
        ChildRepoScan::Clear,
        "never created: nothing to walk"
    );
}

/// #10457 ruling (b): a dangling link whose lexical target leaves the root
/// refuses, relative or absolute.
#[cfg(unix)]
#[test]
fn exhaustive_refuses_a_dangling_link_outside_the_root() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::create_dir_all(ws.join("deep")).unwrap();
    let link = ws.join("deep/gone");
    symlink(Path::new("../../nowhere"), &link).unwrap();
    assert_eq!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes {
            link: link.clone(),
            target: ws.parent().unwrap().join("nowhere"),
        })
    );
    std::fs::remove_file(&link).unwrap();
    symlink(Path::new("/nonexistent-10457/x"), &link).unwrap();
    assert!(matches!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes { link: l, .. }) if l == link
    ));
}

/// #10457 ruling (b): a link to a file outside the root refuses.
#[cfg(unix)]
#[test]
fn exhaustive_refuses_a_file_link_outside_the_root() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let notes = ws.parent().unwrap().join("notes.md");
    std::fs::write(&notes, "x").unwrap();
    let link = ws.join("AGENTS.md");
    symlink(&notes, &link).unwrap();
    assert_eq!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes {
            link,
            target: notes
        })
    );
}

/// #10457 ruling (b): a dangling link that stays inside the root passes; the
/// walk covers the path it names.
#[cfg(unix)]
#[test]
fn exhaustive_passes_a_dangling_link_inside_the_root() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::create_dir_all(ws.join("a/b")).unwrap();
    symlink(Path::new("../missing"), ws.join("a/b/gone")).unwrap();
    symlink(ws.join("not-yet"), ws.join("later")).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Clear);
}

/// #10457 ruling (b): a link to a file inside the root passes
/// (`AGENTS.md -> CLAUDE.md` is common).
#[cfg(unix)]
#[test]
fn exhaustive_passes_a_file_link_inside_the_root() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::write(ws.join("CLAUDE.md"), "x").unwrap();
    std::fs::create_dir_all(ws.join("docs")).unwrap();
    symlink(Path::new("CLAUDE.md"), ws.join("AGENTS.md")).unwrap();
    symlink(Path::new("./../CLAUDE.md"), ws.join("docs/rules.md")).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Clear);
}

/// #10457 delta critic HIGH 1: `..` after a symlinked component is the parent
/// of the link's TARGET. With pnpm's own `.pnpm/back -> ..` (= `node_modules`),
/// `back/../../x` is `parent(ws)/x` to the kernel but `node_modules/x` by text.
/// Covers a target that exists and a dangling one.
#[cfg(unix)]
#[test]
fn exhaustive_resolves_dotdot_after_a_link_like_the_kernel() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let pnpm = ws.join("node_modules/.pnpm");
    std::fs::create_dir_all(&pnpm).unwrap();
    symlink(Path::new(".."), pnpm.join("back")).unwrap();
    assert_eq!(exhaustive(&ws), ChildRepoScan::Clear);

    let outside = ws.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "x").unwrap();
    let link = pnpm.join("f");
    symlink(Path::new("back/../../outside.txt"), &link).unwrap();
    assert_eq!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes {
            link: link.clone(),
            target: outside,
        })
    );

    std::fs::remove_file(&link).unwrap();
    symlink(Path::new("back/../../nowhere.txt"), &link).unwrap();
    assert_eq!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes {
            link,
            target: ws.parent().unwrap().join("nowhere.txt"),
        })
    );
}

/// #10457 delta critic HIGH 2: APFS resolves `.GIT` to `.git`, so a link into
/// any case of `.git` refuses. The `.GIT` tail is dangling here, so the fence
/// decides it on a case-sensitive filesystem too.
#[cfg(unix)]
#[test]
fn exhaustive_fences_a_git_component_in_any_case() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    std::fs::create_dir_all(ws.join("local")).unwrap();
    let link = ws.join("AGENTS.md");
    symlink(Path::new("local/.GIT/config"), &link).unwrap();
    assert_eq!(
        exhaustive(&ws),
        ChildRepoScan::Incomplete(ScanIncomplete::SymlinkEscapes {
            link,
            target: ws.join("local/.GIT/config"),
        })
    );
}

/// #10457 delta critic LOW 3: the caller checked a real directory at the
/// excepted path; one swapped for a symlink before the walk refuses.
#[cfg(unix)]
#[test]
fn exhaustive_refuses_an_excepted_entry_swapped_for_a_symlink() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let ws = workspace(&tmp);
    let outside = ws.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let local = ws.join("local");
    symlink(&outside, &local).unwrap();
    assert!(matches!(
        exhaustive_except(&ws, &local),
        ChildRepoScan::Incomplete(ScanIncomplete::Unreadable { path, .. }) if path == local
    ));
}

/// The exhaustive rule with `except`, under the real device reader.
fn exhaustive_except(dir: &Path, except: &Path) -> ChildRepoScan {
    scan_for_child_repo_exhaustive(dir, Some(except), 4096)
}
