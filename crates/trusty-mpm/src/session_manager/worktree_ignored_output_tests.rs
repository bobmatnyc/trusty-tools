//! Tests for [`super`] (#8534): the gitignored-output rule and its fail-safe.
//! Test: this file.

use std::path::Path;

use super::{
    ignored_output_blocks_removal, ignored_output_refusal, inspect_dirt_with_ignored_output,
    is_harness_path, is_regenerable, kept_ignored_output,
};
use crate::session_manager::DirtyWorktreePolicy;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// Write `patterns` as the shared `info/exclude` of `fx`'s repository.
fn exclude(fx: &GitWorktreeFixture, patterns: &str) {
    std::fs::write(fx.repo.join(".git/info/exclude"), patterns).expect("write info/exclude");
}

/// Ignore `target/` and `results/` for every worktree of `fx`'s repository.
fn ignore_target_and_results(fx: &GitWorktreeFixture) {
    exclude(fx, "target/\nresults/\n");
}

/// Write `body` to `wt/rel`, creating its parent directories.
fn put(wt: &Path, rel: &str, body: &str) {
    let path = wt.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, body).expect("write");
}

/// The name rule: only the entry's LAST component decides (critic round 2).
#[test]
fn rule_excuses_build_output_and_keeps_run_output() {
    for (entry, regenerable) in [
        ("target/", true),
        ("crates/x/target/", true),
        ("web/node_modules/", true),
        ("crates/a/ui/dist/", true),
        ("crates/a/ui/src-tauri/gen/", true),
        ("site/.nuxt/", true),
        (".parcel-cache/", true),
        ("infra/.terraform/", true),
        ("pkg/__pycache__/", true),
        ("mod.pyc", true),
        (".DS_Store", true),
        ("build/out.json", false),
        ("target/report.json", false),
        ("target-analysis/", false),
        ("target-worktree/", false),
        ("gen/", false),
        ("results/", false),
        ("run-output.json", false),
        (".env.local", false),
        ("targets/", false),
    ] {
        assert_eq!(is_regenerable(entry), regenerable, "{entry}");
    }
}

/// Harness bookkeeping is excused at the tree root only, never a look-alike.
#[test]
fn harness_paths_are_excused_and_look_alikes_are_not() {
    for (rel, harness) in [
        (".trusty-mpm-worktree", true),
        (".trusty-mpm/", true),
        (".trusty-mpm/sessions/a.json", true),
        (".claude/settings.json", true),
        (".claude/settings.json.20260926T0000Z.bak", true),
        (".claude/settings.json.lock", true),
        (".claude/settings.local.json", true),
        (".claude/agents/", true),
        (".claude/agents/engineer.md", true),
        (".claude/skills/tm/SKILL.md", true),
        (".claude/output-styles/trusty-mpm.md", true),
        (".claude/output-styles/x.tm-floor.md", true),
        (".claude/output-styles/mine.md", false),
        (".claude/commands/deploy.md", false),
        ("crates/a/.claude/settings.json", false),
        (".trusty-mpmx", false),
        ("results/out.json", false),
    ] {
        assert_eq!(is_harness_path(rel), harness, "{rel}");
    }
}

/// A gitignored results directory is kept output, counted file by file.
#[test]
fn kept_output_counts_files_in_an_ignored_results_dir() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-results");
    ignore_target_and_results(&fx);
    put(&wt, "results/run.json", "{}");
    put(&wt, "results/batch/part.json", "{}");

    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect("results/ is kept output");

    assert_eq!(kept.files, 2);
    assert_eq!(kept.first, "results/");
}

/// Gitignored build output alone is nothing to keep.
#[test]
fn kept_output_is_none_for_build_output_only() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-target");
    ignore_target_and_results(&fx);
    put(&wt, "target/debug/bin", "elf");
    std::fs::create_dir_all(wt.join("results")).expect("an empty ignored dir");

    assert_eq!(kept_ignored_output(&wt), Ok(None));
}

/// 🔴 critic round 2: `build/out.json` under a TRACKED `build/` is run output.
/// Fails at 199c2e447, which excused any path with a `build` component.
#[test]
fn an_ignored_file_under_a_tracked_build_dir_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("tracked-build");
    put(&wt, "build/keep.txt", "tracked\n");
    GitWorktreeFixture::commit_all_and_push(&wt, "track build/");
    exclude(&fx, "build/*.json\n");
    put(&wt, "build/out.json", "{}");

    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect("build/out.json is kept output");
    assert_eq!(kept.first, "build/out.json");
}

/// 🔴 critic round 2: a `target-*` directory is excused by cargo's own marker,
/// never by its name. Fails at 199c2e447, which excused `target-analysis/`.
#[test]
fn tagged_cache_dirs_are_excused_by_content_not_name() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("tagged-caches");
    exclude(&fx, "target-*/\n");
    put(&wt, "target-8510/.rustc_info.json", "{}");
    put(&wt, "target-8510/debug/app", "elf");
    put(&wt, "crates/py/.pytest_cache/.gitignore", "*\n");
    put(
        &wt,
        "crates/py/.pytest_cache/CACHEDIR.TAG",
        "Signature: 8a477f597d28d172789f06886806bc55\n# pytest\n",
    );
    put(&wt, "crates/py/.pytest_cache/v/cache/lastfailed", "{}");
    assert_eq!(kept_ignored_output(&wt), Ok(None), "tagged caches only");

    put(&wt, "target-analysis/x", "a user's notes");
    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect("target-analysis/ is kept output");
    assert_eq!(kept.first, "target-analysis/");
}

/// A `CACHEDIR.TAG` without the spec's signature does not excuse its directory.
#[test]
fn a_cachedir_tag_without_the_signature_is_not_a_cache() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("forged-tag");
    exclude(&fx, "runs/\n");
    put(&wt, "runs/CACHEDIR.TAG", "not a cache\n");
    put(&wt, "runs/out.json", "{}");

    let kept = kept_ignored_output(&wt).expect("the check completes");
    assert_eq!(kept.map(|k| k.files), Some(2));
}

/// A wholly ignored `.claude/` holding only harness files is nothing to keep;
/// a note beside them is.
#[test]
fn a_wholly_ignored_claude_dir_counts_only_non_harness_files() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-claude");
    exclude(&fx, ".claude/\n");
    put(&wt, ".claude/settings.json", "{}");
    put(&wt, ".claude/agents/engineer.md", "# agent");
    assert_eq!(kept_ignored_output(&wt), Ok(None));

    put(&wt, ".claude/notes.md", "keep me");
    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect(".claude/notes.md is kept output");
    assert_eq!((kept.files, kept.first.as_str()), (1, ".claude/"));
}

/// A directory git cannot answer for is an error, never "nothing kept".
#[test]
fn kept_output_errors_when_git_cannot_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = kept_ignored_output(dir.path()).expect_err("git status fails outside a repo");
    assert!(err.contains("git"), "{err}");
}

/// Fail-safe: a path git has to quote is an error, and the tree is kept.
#[test]
fn kept_output_errors_on_a_quoted_path() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("quoted-path");
    exclude(&fx, "*.json\n");
    put(&wt, "a\"b.json", "{}");

    let err = kept_ignored_output(&wt).expect_err("a quoted path cannot be classified");
    assert!(err.contains("quoted path"), "{err}");
    let reason = ignored_output_refusal(&wt).expect("a quoted path must refuse");
    assert!(reason.contains("check failed"), "{reason}");
}

/// Fail-safe: an unreadable kept directory is an error, and the tree is kept.
#[test]
fn kept_output_errors_on_an_unreadable_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unreadable-results");
    ignore_target_and_results(&fx);
    put(&wt, "results/sub/out.json", "{}");
    let sub = wt.join("results/sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let result = kept_ignored_output(&wt);

    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    let err = result.expect_err("an unreadable directory cannot be counted");
    assert!(err.contains("unreadable"), "{err}");
}

/// Fail-safe: a failed check keeps the tree and says why.
#[test]
fn refusal_keeps_the_tree_when_the_check_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reason = ignored_output_refusal(dir.path()).expect("a failed check must refuse");
    assert!(reason.contains("check failed"), "{reason}");
    assert!(
        reason.contains(&dir.path().display().to_string()),
        "{reason}"
    );
    assert!(ignored_output_blocks_removal(dir.path(), DirtyWorktreePolicy::Skip).is_some());
}

/// Only the explicit discard opt-in lets gitignored output go.
#[test]
fn force_discard_policy_lets_gitignored_output_go() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("discard-policy");
    ignore_target_and_results(&fx);
    put(&wt, "results/out.json", "{}");

    assert!(ignored_output_blocks_removal(&wt, DirtyWorktreePolicy::Skip).is_some());
    assert_eq!(
        ignored_output_blocks_removal(&wt, DirtyWorktreePolicy::ForceDiscard),
        None
    );
}

/// The `tm pr cleanup` probe: kept output is dirt, so an ahead-only reading —
/// which cleanup defers to a merge check — no longer passes. Fails at
/// 199c2e447, where the probe was `inspect_dirt` alone.
#[test]
fn probe_counts_ignored_output_as_dirt() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("probe-ahead");
    GitWorktreeFixture::commit_unpushed(&wt);
    ignore_target_and_results(&fx);
    put(&wt, "results/out.json", "{}");

    let dirt = inspect_dirt_with_ignored_output(&wt).expect("kept output is dirt");
    assert_eq!(dirt.dirty_files, 1, "{dirt:?}");
    assert!(dirt.unpushed_commits > 0, "{dirt:?}");
    assert!(dirt.reason.contains("results/"), "{dirt:?}");

    std::fs::remove_dir_all(wt.join("results")).expect("clear results");
    let plain = inspect_dirt_with_ignored_output(&wt).expect("still ahead");
    assert_eq!(plain.dirty_files, 0, "only the unpushed commit: {plain:?}");
}

/// The probe fails toward the "could not read" arm when its own check fails.
#[test]
fn probe_fails_toward_dirty_when_the_ignored_check_fails() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("probe-quoted");
    exclude(&fx, "*.json\n");
    put(&wt, "a\"b.json", "{}");

    let dirt = inspect_dirt_with_ignored_output(&wt).expect("a failed check is dirt");
    assert_eq!(
        (dirt.dirty_files, dirt.unpushed_commits),
        (0, 0),
        "{dirt:?}"
    );
    assert!(dirt.reason.contains("check failed"), "{dirt:?}");
}
