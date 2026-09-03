//! Unit tests for the #4091 dirty-worktree guard in `worktree_safety.rs`.
//!
//! Why: these exercise [`super::inspect_dirt`] in isolation against REAL git
//! repositories built under `tempfile::tempdir()` — never a live worktree —
//! so each "dirty" definition (modified tracked, untracked-not-ignored,
//! committed-but-unpushed) and each fail-safe error path is pinned
//! independently of the reclaim path that consumes them. The wired-up
//! behaviour is covered separately by the `#4091` tests in
//! `prune_orphan_tests`.
//! What: the clean/pushed baseline, the three dirty definitions, the
//! ignored-files carve-out, the non-git-directory classification, and the
//! error-means-dirty invariant.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// The default policy must be the SAFE one. If this ever flips, every caller
/// that relies on `..Default::default()` silently gains the power to destroy
/// uncommitted work.
#[test]
fn dirty_policy_defaults_to_skip() {
    assert_eq!(
        DirtyWorktreePolicy::default(),
        DirtyWorktreePolicy::Skip,
        "the default dirty policy must never be ForceDiscard"
    );
}

/// Baseline: a worktree with no local edits, whose HEAD is already on a
/// remote-tracking ref, is PROVABLY clean — the guard must return `None` so
/// reclamation still works. Without this the guard would be a permanent leak.
#[test]
fn inspect_dirt_clean_pushed_worktree_is_none() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("clean");
    assert!(
        inspect_dirt(&wt).is_none(),
        "a clean, fully-pushed worktree must be reclaimable; got {:?}",
        inspect_dirt(&wt)
    );
}

/// Definition 1a of dirty: a MODIFIED TRACKED file.
#[test]
fn inspect_dirt_reports_modified_tracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("modified");
    std::fs::write(wt.join("README.md"), "edited but never committed\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("a modified tracked file must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason was: {}", dirt.reason);
    assert_eq!(dirt.unpushed_commits, 0);
    assert_eq!(dirt.path, wt);
}

/// Definition 1b of dirty: an UNTRACKED, non-ignored file. Untracked work is
/// the most easily lost kind — it exists nowhere but that directory.
#[test]
fn inspect_dirt_reports_untracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("untracked");
    std::fs::write(wt.join("notes.md"), "hand-written, never added\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("an untracked file must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason was: {}", dirt.reason);
}

/// The carve-out that keeps the guard usable: IGNORED files (build artefacts)
/// are not work, and must not pin a worktree as dirty forever. A `target/`
/// directory in every checkout would otherwise make the guard refuse
/// everything, which is indistinguishable from having no reclamation at all.
#[test]
fn inspect_dirt_ignores_gitignored_files() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored");
    std::fs::write(wt.join(".gitignore"), "build-output/\n").unwrap();
    std::fs::create_dir_all(wt.join("build-output")).unwrap();
    std::fs::write(wt.join("build-output").join("artifact.bin"), "junk\n").unwrap();
    // Commit the .gitignore itself so only the IGNORED path remains on disk.
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["add", ".gitignore"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["commit", "-m", "ignore build output"])
        .output()
        .unwrap();
    assert!(commit.status.success());

    // The commit itself is unpushed, so the worktree IS dirty — but for the
    // commit, not for the ignored artefact. Assert on the file count.
    let dirt = inspect_dirt(&wt).expect("the new commit is unpushed, so this is dirty");
    assert_eq!(
        dirt.dirty_files, 0,
        "ignored build output must not count as uncommitted work; reason: {}",
        dirt.reason
    );
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
}

/// trusty-mpm's OWN untracked ownership sentinel must not count as work.
///
/// Why this test exists: it caught a real self-inflicted bug. Every managed
/// worktree carries a `.trusty-mpm-worktree` sentinel, and most host projects
/// do not gitignore it — so counting it marked EVERY managed worktree
/// permanently dirty, which would have shipped a guard that silently disabled
/// all reclamation.
#[test]
fn inspect_dirt_excludes_own_sentinel() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("sentinel-only");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    assert!(
        inspect_dirt(&wt).is_none(),
        "trusty-mpm's own sentinel must not make a worktree dirty; got {:?}",
        inspect_dirt(&wt)
    );
}

/// The two DISPOSABLE `.trusty-mpm/` artefacts — and only those — must not
/// count as work. The daemon rewrites both on every snapshot.
#[test]
fn inspect_dirt_excludes_untracked_trusty_mpm_dir() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("scrollback");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("scrollback.txt"), "pane dump\n").unwrap();
    std::fs::write(
        wt.join(".trusty-mpm").join("last-instructions.md"),
        "brief\n",
    )
    .unwrap();

    assert!(
        inspect_dirt(&wt).is_none(),
        "the daemon's own scrollback artefacts must not make a worktree dirty; got {:?}",
        inspect_dirt(&wt)
    );
}

/// THE REGRESSION THIS ROUND EXISTS FOR: hand-rescued source parked under
/// `.trusty-mpm/` must read as work.
///
/// Why this exact shape: on 2026-07-27 the live session worktree
/// `.base/.worktrees/2eb72dca-…` held
/// `.trusty-mpm/wip-backup-20260727-jira/` containing twelve files of
/// uncommitted `trusty-git-analytics` source, deliberately preserved by an
/// earlier rescue operation. The first cut of this guard excused the whole
/// `.trusty-mpm/` subtree by prefix, so `dirty_files` stayed 0, `inspect_dirt`
/// returned `None`, and `git worktree remove --force` would have destroyed it
/// silently. The carve-out is now an allowlist of two exact paths.
#[test]
fn inspect_dirt_counts_wip_backup_under_trusty_mpm() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("wip-backup");
    let wip = wt.join(".trusty-mpm").join("wip-backup-20260727-jira");
    std::fs::create_dir_all(wip.join("crates").join("foo").join("src")).unwrap();
    std::fs::write(
        wip.join("crates").join("foo").join("src").join("main.rs"),
        "fn main() { /* rescued, uncommitted */ }\n",
    )
    .unwrap();
    // The disposable artefacts sit alongside it, exactly as they do live.
    std::fs::write(wt.join(".trusty-mpm").join("scrollback.txt"), "pane dump\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("rescued source under .trusty-mpm/ must read as dirty");
    assert_eq!(
        dirt.dirty_files, 1,
        "the wip-backup tree must count and the scrollback must not; reason: {}",
        dirt.reason
    );
}

/// Pause snapshots under `.trusty-mpm/sessions/` are what `/tm-session-resume`
/// reads back, and this PR's own skill change tells operators to trust the
/// pause path. They are not disposable.
#[test]
fn inspect_dirt_counts_pause_snapshots_under_trusty_mpm() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("pause-snapshots");
    std::fs::create_dir_all(wt.join(".trusty-mpm").join("sessions")).unwrap();
    std::fs::write(
        wt.join(".trusty-mpm").join("sessions").join("s-01.md"),
        "# paused session\n",
    )
    .unwrap();

    let dirt = inspect_dirt(&wt).expect("pause snapshots must read as dirty");
    assert!(dirt.dirty_files >= 1, "reason: {}", dirt.reason);
}

/// The SAME `.trusty-mpm/` content must read as dirty when the project
/// GITIGNORES it, which is how this repo's `main` spells it (`.gitignore:49`,
/// `.trusty-mpm/*`). Plain `git status` cannot see it at all in that spelling —
/// only the per-file `--ignored=matching` pass can. Both spellings are live in
/// the fleet simultaneously, on different branches of this one repo.
#[test]
fn inspect_dirt_counts_gitignored_trusty_mpm_work() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-tm");
    std::fs::write(wt.join(".gitignore"), ".trusty-mpm/*\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&wt, "gitignore .trusty-mpm");
    std::fs::create_dir_all(wt.join(".trusty-mpm").join("wip-backup-x")).unwrap();
    std::fs::write(
        wt.join(".trusty-mpm").join("wip-backup-x").join("main.rs"),
        "fn main() {}\n",
    )
    .unwrap();

    // Prove the premise: plain porcelain really is blind to it.
    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: plain porcelain should not see the gitignored subtree"
    );

    let dirt = inspect_dirt(&wt).expect("gitignored .trusty-mpm/ work must still read as dirty");
    assert!(dirt.dirty_files >= 1, "reason: {}", dirt.reason);
}

/// The counterweight: a TRACKED file under `.trusty-mpm/` that someone edited
/// IS real work, and the bookkeeping exclusion must not swallow it. The
/// exclusion is scoped to untracked entries precisely so this stays protected.
#[test]
fn inspect_dirt_counts_tracked_trusty_mpm_edit() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("tracked-config");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("INSTRUCTIONS.md"), "v1\n").unwrap();
    for args in [
        vec!["add", ".trusty-mpm/INSTRUCTIONS.md"],
        vec!["commit", "-m", "track instructions"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {:?}", out);
    }
    std::fs::write(
        wt.join(".trusty-mpm").join("INSTRUCTIONS.md"),
        "v2 edited\n",
    )
    .unwrap();

    let dirt = inspect_dirt(&wt).expect("a tracked edit is real work, wherever it lives");
    assert_eq!(
        dirt.dirty_files, 1,
        "a tracked .trusty-mpm/ edit must still count; reason: {}",
        dirt.reason
    );
}

/// #4166: a tracked file DELETED from `.trusty-mpm/` must be counted by
/// exactly one of the two passes, and before this it was counted by neither.
///
/// Why: pass 1 skips every `.trusty-mpm/`-scoped status line on the promise
/// that pass 2 owns them, and pass 2 returned `Ok(0)` without running git
/// whenever the directory was absent. Deleting the last file under
/// `.trusty-mpm/` removes the directory too, so the deletion fell through the
/// handover and `dirty_files` read 0 — the broken invariant, not a large loss:
/// for a registered worktree both the index and the object store live outside
/// the candidate, so nothing unique dies.
#[test]
fn inspect_dirt_counts_a_deleted_tracked_file_under_trusty_mpm() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("deleted-tm");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("notes.md"), "v1\n").unwrap();
    // Pushed, so the commit itself is not the dirt under test.
    GitWorktreeFixture::commit_all_and_push(&wt, "track notes under .trusty-mpm");
    std::fs::remove_dir_all(wt.join(".trusty-mpm")).unwrap();

    // Premise: the directory really is gone, which is what used to short-circuit
    // pass 2 before it ever asked git.
    assert!(
        !wt.join(".trusty-mpm").exists(),
        "premise broken: the whole directory should be gone"
    );

    let dirt =
        inspect_dirt(&wt).expect("a deleted tracked file under .trusty-mpm/ must read as dirty");
    assert_eq!(
        dirt.dirty_files, 1,
        "the deletion must be counted by pass 2; reason: {}",
        dirt.reason
    );
}

/// Definition 2 of dirty: a COMMITTED but UNPUSHED commit. This looks safe —
/// the work is committed — but `remove_session_worktree` deletes the
/// `session/<leaf>` branch after removal, so the commit loses its last
/// reachable ref.
#[test]
fn inspect_dirt_reports_unpushed_commit() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unpushed");
    GitWorktreeFixture::commit_unpushed(&wt);

    let dirt = inspect_dirt(&wt).expect("an unpushed commit must read as dirty");
    assert_eq!(dirt.dirty_files, 0, "reason: {}", dirt.reason);
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
}

/// THE CRITICAL: a parent worktree containing a GITIGNORED nested worktree
/// holding uncommitted work must read as DIRTY.
///
/// Why: `.gitignore:40` on this repo's `main` ignores `.claude/worktrees/`, the
/// exact shape `find_orphaned_worktrees` location 5 exists to walk. The nested
/// worktrees carry no ownership sentinel, so the #3649 gate routes them to
/// `owner_unknown` and REFUSES to delete them — and then the sweep would delete
/// the parent they live inside, having certified it clean. The guard was
/// bypassable by deleting its own parent.
#[test]
fn inspect_dirt_reports_nested_gitignored_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("parent");
    std::fs::write(parent.join(".gitignore"), ".claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore agent worktrees");
    let nested = fx.add_nested_worktree(&parent, ".claude/worktrees", "agentA");
    std::fs::write(nested.join("patch.rs"), "// agent work, never committed\n").unwrap();

    // Prove the premise: the parent's own porcelain really is blind to it.
    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&parent)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: plain porcelain should not see the gitignored nested worktree"
    );

    let dirt = inspect_dirt(&parent)
        .expect("a nested worktree holding uncommitted work must make the parent dirty");
    assert!(
        dirt.reason.contains("nested git worktree/repository"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The same hole, but via an UNREGISTERED repository — an independent clone
/// dropped into a gitignored directory. `git worktree list` has never heard of
/// it, so only the on-disk scan can find it.
#[test]
fn inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("with-clone");
    std::fs::write(parent.join(".gitignore"), "scratch/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore scratch");
    let inner = parent.join("scratch").join("side-project");
    std::fs::create_dir_all(&inner).unwrap();
    for args in [vec!["init", "--initial-branch=main"], vec!["status"]] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&inner)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    std::fs::write(inner.join("work.rs"), "// never committed anywhere\n").unwrap();

    let dirt = inspect_dirt(&parent)
        .expect("an unregistered nested repo holding work must make the parent dirty");
    assert!(
        dirt.reason.contains("nested git worktree/repository"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The counterweight, and it matters as much as the finding: a nested worktree
/// that is itself CLEAN and pushed must NOT pin the parent.
///
/// Why: without this, any session that ever spawned an agent worktree would be
/// permanently unreclaimable, which is the "guard that silently disables
/// reclamation" failure mode — indistinguishable from having no sweep at all.
#[test]
fn inspect_dirt_allows_clean_nested_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("clean-parent");
    std::fs::write(parent.join(".gitignore"), ".claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore agent worktrees");
    let nested = fx.add_nested_worktree(&parent, ".claude/worktrees", "agentClean");
    std::fs::write(nested.join("done.rs"), "// finished agent work\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&nested, "agent work, pushed");

    assert!(
        inspect_dirt(&parent).is_none(),
        "a clean, pushed nested worktree must not pin the parent; got {:?}",
        inspect_dirt(&parent)
    );
}

/// A dirty nested worktree sitting BEHIND a clean one must still be found.
///
/// Why this test exists: it pins a real bug. The first cut of the scan used
/// `?` on the per-root inspection, so the first CLEAN nested root returned
/// `None` for the whole scan and every root after it went unexamined — a parent
/// with one finished agent worktree and one in-flight one read CLEAN. Roots are
/// visited in sorted order, so `aaa-clean` is checked before `zzz-dirty`.
#[test]
fn inspect_dirt_keeps_scanning_past_a_clean_nested_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("two-nested");
    std::fs::write(parent.join(".gitignore"), ".claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore agent worktrees");

    let clean = fx.add_nested_worktree(&parent, ".claude/worktrees", "aaa-clean");
    std::fs::write(clean.join("done.rs"), "// finished\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&clean, "finished, pushed");

    let dirty = fx.add_nested_worktree(&parent, ".claude/worktrees", "zzz-dirty");
    std::fs::write(dirty.join("wip.rs"), "// in flight, never committed\n").unwrap();

    let dirt = inspect_dirt(&parent)
        .expect("a dirty nested worktree behind a clean one must still make the parent dirty");
    assert!(
        dirt.reason.contains("zzz-dirty"),
        "the reason must name the nested worktree that is actually dirty; got: {}",
        dirt.reason
    );
}

/// Documents the residual, so it is a decision rather than an accident: the
/// nested-repo scan does NOT descend into disposable build directories, so a
/// repository inside `target/` is invisible. Skipping them by name is what
/// keeps the scan affordable across ~95 candidates.
#[test]
fn inspect_dirt_does_not_scan_disposable_build_dirs() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("disposable");
    std::fs::write(parent.join(".gitignore"), "target/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore target");
    let buried = parent.join("target").join("debug").join("checkout");
    std::fs::create_dir_all(buried.join(".git")).unwrap();
    std::fs::write(buried.join("thing.rs"), "// inside target/\n").unwrap();

    assert!(
        inspect_dirt(&parent).is_none(),
        "target/ is deliberately not scanned; got {:?}",
        inspect_dirt(&parent)
    );
}

/// HIGH-1: `status.showUntrackedFiles=no` makes bare `git status --porcelain`
/// exit 0 with EMPTY output while untracked work sits on disk. Its natural home
/// is the SHARED `.base/.git/config` every worktree in the sweep reads, so one
/// line there would blind the guard fleet-wide. The mode is pinned on the
/// command line, which beats every config file.
#[test]
fn inspect_dirt_survives_hostile_show_untracked_files_config() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("hostile-untracked");
    std::fs::write(wt.join("notes.md"), "hand-written, never added\n").unwrap();
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["config", "status.showUntrackedFiles", "no"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Prove the premise: unpinned porcelain really does go silent.
    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: the hostile config should have silenced plain porcelain"
    );

    let dirt = inspect_dirt(&wt).expect("a hostile config must not be able to hide untracked work");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
}

/// The same class of hijack via `core.excludesFile`: a user-global excludes
/// file naming the work hides it exactly as `showUntrackedFiles=no` does.
/// Pinned empty on the command line.
#[test]
fn inspect_dirt_survives_hostile_global_excludes_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("hostile-excludes");
    let excludes = fx.repo.join("hostile-excludes.txt");
    std::fs::write(&excludes, "secret-work.txt\n").unwrap();
    std::fs::write(wt.join("secret-work.txt"), "REAL WORK\n").unwrap();
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["config", "core.excludesFile"])
        .arg(&excludes)
        .output()
        .unwrap();
    assert!(out.status.success());

    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: the excludes file should have hidden the work"
    );

    let dirt = inspect_dirt(&wt).expect("a hostile excludes file must not hide untracked work");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
}

/// MEDIUM: `decommission` force-deletes `session/<leaf>` derived from the
/// DIRECTORY NAME, not from `HEAD`. When HEAD has been switched off that branch
/// — routine; the live `.base/.worktrees/2eb72dca-…` sits on `fix/4061-…` —
/// HEAD reads fully pushed while the branch about to be destroyed is not.
#[test]
fn inspect_dirt_reports_unpushed_session_branch_when_head_moved() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("moved-head");
    GitWorktreeFixture::commit_unpushed(&wt);
    // Switch HEAD off session/moved-head onto the pushed base commit.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["switch", "--detach", "origin/main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let dirt = inspect_dirt(&wt)
        .expect("an unpushed commit on the branch decommission deletes must read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "HEAD is pushed but session/moved-head is not; reason: {}",
        dirt.reason
    );
}

/// The `.base/.worktrees/<session-id>` shape — the DOMINANT population of the
/// sweep — has its branch named BARE by `provisioner/workspace.rs:803`
/// (`session_id.to_string()`), not `session/<id>`. Checking only the prefixed
/// spelling left this leg inert exactly where most worktrees live.
///
/// Deliberately conservative: `branch -D session/<id>` currently MISSES the
/// bare branch, so it survives removal today. The divergence is a live
/// pre-existing bug that will be repaired on one side or the other, and if it
/// is repaired on the `remove_session_worktree` side then bare branches start
/// being destroyed. A guard that goes silently inert because somebody fixed an
/// unrelated naming bug is the failure mode this module exists to prevent.
#[test]
fn inspect_dirt_reports_unpushed_bare_leaf_branch_when_head_moved() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("bare-branch");
    // Re-point the worktree at a BARE branch named after the leaf, the way the
    // provisioner names it, then commit on it and switch HEAD away.
    for args in [
        vec!["switch", "-c", "bare-branch"],
        vec!["branch", "-D", "session/bare-branch"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    GitWorktreeFixture::commit_unpushed(&wt);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["switch", "--detach", "origin/main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let dirt = inspect_dirt(&wt)
        .expect("an unpushed commit on the bare leaf-named branch must read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "HEAD is pushed but the bare `bare-branch` is not; reason: {}",
        dirt.reason
    );
}

/// The counterweight to the test above: when the session branch IS the
/// checked-out branch, its commits must be counted ONCE, not twice.
#[test]
fn inspect_dirt_does_not_double_count_the_checked_out_session_branch() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("single-count");
    GitWorktreeFixture::commit_unpushed(&wt);

    let dirt = inspect_dirt(&wt).expect("an unpushed commit must read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "the same commit must not be counted by both legs; reason: {}",
        dirt.reason
    );
}

/// MEDIUM: `GIT_DIR` + `GIT_WORK_TREE` pointing at a clean repository make
/// `git -C <candidate> status` answer for THAT repository — a false CLEAN. The
/// daemon inherits whatever environment launched it. Asserted on the command
/// itself rather than by mutating process-global environment, which is racy
/// under a parallel test runner.
#[test]
fn git_command_strips_repository_redirecting_env() {
    let cmd = super::git_command(std::path::Path::new("/tmp"), &["status"]);
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_CONFIG_COUNT",
    ] {
        assert!(
            removed.contains(&key),
            "{key} must be removed from the child environment; removed: {removed:?}"
        );
    }
}

/// #6391 REGRESSION: the REMOVAL is subject to the same redirect its gates are
/// hardened against, and until #6391 it was the one call that was not.
///
/// Why: `git worktree` resolves its repository from `GIT_DIR` ahead of `-C`.
/// Measured directly: with `GIT_DIR`/`GIT_WORK_TREE` naming another repository,
/// `git -C <repo> worktree list` lists that OTHER repository's worktrees, and
/// operating on one of `<repo>`'s own worktrees exits 128 with
/// `is not a working tree`. Every gate ahead of the removal goes through
/// [`super::git_command`] and so examines the real worktree; the removal used a
/// bare `Command::new("git")` and did not. The result is a reap that passes
/// every gate, deletes nothing, and reports the refusal to a `warn!` no test
/// reads — `agent_worktree_reap_tests::await_gone` then panics with
/// `was never removed` and no reason (#6391, and the `is not a working tree`
/// text #6099 recorded).
///
/// Against the pre-fix body this fails outright: a bare command removes no
/// variables at all, so `removed` is empty.
#[test]
fn worktree_remove_command_strips_repository_redirecting_env() {
    let cmd = super::worktree_remove_command(
        std::path::Path::new("/r"),
        std::path::Path::new("/r/.claude/worktrees/agent-x"),
    );
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_COUNT",
    ] {
        assert!(
            removed.contains(&key),
            "{key} must be removed from the removal's child environment; removed: {removed:?}"
        );
    }
}

/// The hardening must not reorder the command: `--force` still precedes the
/// target, and the target is still the last argument.
///
/// Why: [`super::git_command`] appends its own pinned globals and `-C <dir>`
/// before the caller's arguments, so routing the removal through it is only
/// correct if the subcommand and its operand still land in that order. A
/// silently reordered `git worktree remove <path> --force` is a different
/// command.
#[test]
fn worktree_remove_command_ends_in_the_target_path() {
    let target = std::path::Path::new("/r/.claude/worktrees/agent-x");
    let cmd = super::worktree_remove_command(std::path::Path::new("/r"), target);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        args.ends_with(&[
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            target.display().to_string(),
        ]),
        "the removal must end in `worktree remove --force <path>`; args: {args:?}"
    );
    assert!(
        args.windows(2).any(|w| w[0] == "-C" && w[1] == "/r"),
        "the repository root must still be named by `-C`; args: {args:?}"
    );
}

/// Every git invocation must carry the pins, not just the ones someone
/// remembered — that is the point of routing them through one builder.
#[test]
fn git_command_pins_untracked_and_excludes_config() {
    let cmd = super::git_command(std::path::Path::new("/tmp"), &["status"]);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for pin in [
        "core.excludesFile=",
        "status.showUntrackedFiles=normal",
        "core.quotePath=false",
    ] {
        assert!(
            args.iter().any(|a| a == pin),
            "`{pin}` must be pinned on every git call; args: {args:?}"
        );
    }
}

/// FAIL-SAFE: a path git cannot examine at all reads as DIRTY, never as clean.
/// An error on the check must never become a green light to delete.
#[test]
fn inspect_dirt_treats_missing_path_as_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does").join("not").join("exist");
    let dirt = inspect_dirt(&missing).expect("an unexaminable path must read as dirty");
    assert!(
        dirt.reason.contains("unreadable") || dirt.reason.contains("not a git worktree"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// A plain directory that is NOT a git worktree root but holds files cannot be
/// assessed by git at all — and the fail-safe answer is DIRTY. This also pins
/// the identity check: the directory sits INSIDE a real checkout, so a naive
/// `git status` would have reported the ENCLOSING repo's (clean) status and
/// wrongly approved deletion.
#[test]
fn inspect_dirt_treats_non_worktree_with_files_as_dirty() {
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join(".worktrees").join("not-a-worktree");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("leftover.txt"), "who knows\n").unwrap();

    let dirt = inspect_dirt(&plain).expect("a non-worktree dir holding files must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
    assert!(
        dirt.reason.contains("not a git worktree root"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The counterweight to the test above: an EMPTY leftover shell (nothing but
/// the ownership sentinel) is provably free of work and stays reclaimable.
/// This is the #1838 case the orphan sweep exists for — one project grew 94 of
/// these — so the guard must not turn it into a permanent leak.
#[test]
fn inspect_dirt_allows_empty_non_git_leftover() {
    let tmp = tempfile::tempdir().unwrap();
    let shell = tmp.path().join(".worktrees").join("empty-shell");
    std::fs::create_dir_all(&shell).unwrap();
    std::fs::write(
        shell.join(super::super::decommission::WORKTREE_SENTINEL_FILE),
        "{}",
    )
    .unwrap();

    assert!(
        inspect_dirt(&shell).is_none(),
        "an empty leftover shell must stay reclaimable; got {:?}",
        inspect_dirt(&shell)
    );
}

// ── #6507: a squash-merged branch is not unsaved work ────────────────────────

/// Run one git command in `dir`, asserting it succeeded (#6507 tests).
fn git_must(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("`git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The #6507 regression. A branch whose PR squash-merged, and whose remote
/// branch was then deleted, holds no work a removal could destroy — its patch
/// is on `origin/main` under a different SHA. Before this fix
/// `rev-list --count HEAD --not --remotes` returned 1 for exactly that state,
/// gate 6 of the merged-PR reclaim reported "holds unsaved work", and
/// `tm session prune-worktrees --merged-prs --force` reclaimed 0 on a
/// squash-merging repository.
///
/// FAIL-OPEN CHECK: reporting clean when work is unsaved is the one direction
/// this module may not get wrong, so the discount is per-commit and
/// evidence-based — `git cherry` marking that commit `-` against a remote
/// landing branch. It is never "the upstream ref is gone, therefore fine": the
/// sibling test below keeps a genuinely unpushed commit on the same fixture
/// refused.
#[test]
fn inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("squashed");
    fx.squash_merge_to_origin(&wt, "landed.rs");

    // The pre-fix state, pinned so a regression stays legible: the commit
    // really is unreachable from every remote ref.
    let reachability = git_stdout(&wt, &["rev-list", "--count", "HEAD", "--not", "--remotes"])
        .expect("rev-list must run");
    assert_eq!(
        reachability.trim(),
        "1",
        "the fixture must reproduce the pruned-upstream shape this bug needs"
    );

    assert!(
        inspect_dirt(&wt).is_none(),
        "a squash-merged branch holds no unsaved work; got {:?}",
        inspect_dirt(&wt)
    );
}

/// A squash of TWO commits clears too — the shape that survived the first fix.
///
/// Why: `git cherry` compares patch ids one commit at a time, and a squash
/// carries the UNION of the branch's patches, so a two-commit branch matches
/// nothing. Live on 2026-09-03: PR #6705 squashed `37540d9cb` and `4c8e11de8`
/// into `7933d406d`, `git cherry refs/remotes/origin/main HEAD` marked both
/// `+`, and gate 6 reported "2 unpushed commit(s)" for a fully landed worktree.
#[test]
fn inspect_dirt_clears_a_two_commit_squash_merge() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("squashed-two");
    fx.squash_merge_commits_to_origin(&wt, &["first.rs", "second.rs"]);

    // The pre-fix state, pinned: both commits are unreachable from every remote
    // ref AND neither matches a landed commit's patch id on its own.
    let reachability = git_stdout(&wt, &["rev-list", "--count", "HEAD", "--not", "--remotes"])
        .expect("rev-list must run");
    assert_eq!(
        reachability.trim(),
        "2",
        "the fixture must reproduce a divergence of more than one commit"
    );
    let cherry =
        git_stdout(&wt, &["cherry", "refs/remotes/origin/main", "HEAD"]).expect("cherry must run");
    assert!(
        !cherry.contains("- "),
        "the per-commit comparison must find NO match — that is the whole bug; got {cherry}"
    );

    assert!(
        inspect_dirt(&wt).is_none(),
        "a branch whose two commits squash-merged as one holds no unsaved work; got {:?}",
        inspect_dirt(&wt)
    );
}

/// The control for the two-commit fix: work added AFTER the squash still counts.
///
/// Why: the aggregate comparison discounts a whole divergence at once, so it
/// has to stop discounting the moment the divergence stops matching.
#[test]
fn inspect_dirt_reports_a_commit_added_after_a_two_commit_squash_merge() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("squashed-two-plus");
    fx.squash_merge_commits_to_origin(&wt, &["first.rs", "second.rs"]);

    std::fs::write(wt.join("never-landed.rs"), "work that exists only here\n").unwrap();
    git_must(&wt, &["add", "never-landed.rs"]);
    git_must(&wt, &["commit", "-m", "feat: never pushed anywhere"]);

    let dirt = inspect_dirt(&wt).expect("a commit added after the squash must still read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 3,
        "an aggregate that no longer matches discounts nothing; reason was: {}",
        dirt.reason
    );
}

/// The control the #6507 fix may never break: a commit that landed NOWHERE is
/// still unsaved work, in the same worktree, beside one that did land.
#[test]
fn inspect_dirt_still_reports_a_genuinely_unpushed_commit_beside_a_squashed_one() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("mixed");
    fx.squash_merge_to_origin(&wt, "landed.rs");

    std::fs::write(wt.join("never-landed.rs"), "work that exists only here\n").unwrap();
    git_must(&wt, &["add", "never-landed.rs"]);
    git_must(&wt, &["commit", "-m", "feat: never pushed anywhere"]);

    let dirt = inspect_dirt(&wt).expect("an unlanded commit must still read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "only the squash-merged commit may be discounted; reason was: {}",
        dirt.reason
    );
}

/// The discount is applied PER COMMIT, never as a difference of counts.
///
/// Why this needs its own fixture: `git cherry`'s output range is
/// `<base>..<tip>`, which can hold commits the rev-list never counted. Here the
/// first commit is reachable from `origin/landed-elsewhere`, so it is not a
/// candidate — and its patch is also on `origin/main`, so cherry marks it `-`.
/// A naive `candidates - landed.len()` therefore reports 1 - 1 = 0 and calls the
/// worktree clean, discarding the SECOND commit, which exists nowhere but this
/// directory. The other two #6507 tests pass under that naive form; this one
/// does not.
#[test]
fn inspect_dirt_discounts_only_the_commits_the_query_counted() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("two-refs");
    fx.land_patch_and_keep_the_remote_branch(&wt, "landed.rs", "landed-elsewhere");

    std::fs::write(wt.join("only-here.rs"), "exists nowhere else\n").unwrap();
    git_must(&wt, &["add", "only-here.rs"]);
    git_must(&wt, &["commit", "-m", "feat: exists nowhere else"]);

    // The fixture's own precondition: exactly ONE candidate (the second
    // commit), and a cherry range that also marks the first one landed.
    let counted = git_stdout(&wt, &["rev-list", "--count", "HEAD", "--not", "--remotes"])
        .expect("rev-list must run");
    assert_eq!(
        counted.trim(),
        "1",
        "the fixture must count only the commit that landed nowhere"
    );
    let cherry = git_stdout(&wt, &["cherry", "refs/remotes/origin/main", "HEAD"])
        .expect("git cherry must run");
    assert_eq!(
        cherry.lines().filter(|l| l.starts_with("- ")).count(),
        1,
        "the cherry range must also hold a landed commit the rev-list never counted: {cherry}"
    );

    let dirt = inspect_dirt(&wt).expect("the second commit is unsaved work");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "a landed commit outside the counted set must not discount one inside it; reason was: {}",
        dirt.reason
    );
}

/// A repository with no remote landing branch discounts NOTHING — the patch
/// comparison has no base, and an unanswerable comparison must leave the raw
/// count exactly where it was.
#[test]
fn inspect_dirt_discounts_nothing_without_a_remote_landing_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("solo");
    std::fs::create_dir_all(&repo).unwrap();
    git_must(&repo, &["init", "--initial-branch=main"]);
    git_must(&repo, &["config", "user.email", "ci@test.invalid"]);
    git_must(&repo, &["config", "user.name", "CI"]);
    git_must(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("only.rs"), "local\n").unwrap();
    git_must(&repo, &["add", "only.rs"]);
    git_must(&repo, &["commit", "-m", "local only"]);

    assert!(
        landing_bases(&repo).is_empty(),
        "a repository with no remotes offers no landing base"
    );
    let dirt = inspect_dirt(&repo).expect("a commit on no remote is unsaved work");
    assert_eq!(dirt.unpushed_commits, 1, "reason was: {}", dirt.reason);
}
