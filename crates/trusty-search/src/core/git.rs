//! Git subprocess helpers for branch-aware search (issue #122).
//!
//! Why: when a `SearchQuery` carries `branch: Some(name)` but no explicit
//! `branch_files`, the search pipeline asks git which files diverge between
//! `HEAD` and the merge-base with that branch. We shell out rather than
//! linking libgit2 to keep the dependency surface small and to inherit the
//! caller's `.gitconfig` / safe.directory settings unchanged.
//! What: a single best-effort helper that runs `git merge-base HEAD <branch>`
//! followed by `git diff --name-only <base>..HEAD`. Any failure (non-git
//! workdir, unknown branch, detached HEAD, missing binary) returns `None`
//! with a `tracing::warn!` — the caller falls back to no boost rather than
//! failing the search.
//! Test: covered by unit tests in this module (no-git case) and the
//! integration tests in `core::indexer::tests` that exercise the explicit
//! `branch_files` path.

use std::path::Path;
use std::process::Command;

/// Compute the list of files modified on `branch` relative to the merge-base
/// with `HEAD`, by shelling out to `git`. Paths are returned exactly as `git
/// diff --name-only` prints them (forward-slash separated, relative to the
/// repo root).
///
/// Returns `None` on any failure — caller treats this as "no boost".
pub fn resolve_branch_files(root_path: &Path, branch: &str) -> Option<Vec<String>> {
    // 1) Find the merge-base between HEAD and the named branch.
    let base = Command::new("git")
        .args(["merge-base", "HEAD", branch])
        .current_dir(root_path)
        .output()
        .ok()?;
    if !base.status.success() {
        tracing::warn!(
            "branch file resolution failed for branch '{}': git merge-base exited {:?}",
            branch,
            base.status.code()
        );
        return None;
    }
    let base_sha = std::str::from_utf8(&base.stdout).ok()?.trim().to_owned();
    if base_sha.is_empty() {
        tracing::warn!(
            "branch file resolution failed for branch '{}': empty merge-base",
            branch
        );
        return None;
    }

    // 2) List files changed between the merge-base and HEAD.
    let diff = Command::new("git")
        .args(["diff", "--name-only", &format!("{}..HEAD", base_sha)])
        .current_dir(root_path)
        .output()
        .ok()?;
    if !diff.status.success() {
        tracing::warn!(
            "branch file resolution failed for branch '{}': git diff exited {:?}",
            branch,
            diff.status.code()
        );
        return None;
    }

    let body = std::str::from_utf8(&diff.stdout).ok()?;
    Some(
        body.lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Normalize a path string for comparison: strip a leading `./` so that
/// branch_files entries like `./src/foo.rs` and chunk files like
/// `src/foo.rs` compare equal.
pub fn normalize_path(p: &str) -> &str {
    p.strip_prefix("./").unwrap_or(p)
}

/// Read the current `HEAD` SHA for the repo rooted at `root_path` (issue #75).
///
/// Why: the search response advertises `results_may_be_stale` so callers know
/// when the index was built against an older commit than the working tree's
/// current HEAD. The check is O(1) git read — `git rev-parse HEAD`.
/// What: returns `Some(sha)` (40-char hex) on success, `None` for non-git
/// directories, detached HEAD without commits, missing `git` binary, or any
/// other best-effort failure. Never panics; never blocks the search hot path
/// on slow git ops (this is the only call we make).
/// Test: `test_head_sha_is_none_outside_git_repo`.
pub fn head_sha(root_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = std::str::from_utf8(&out.stdout).ok()?.trim().to_owned();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// The only git stderr CONSISTENT with "there is genuinely no repository here".
///
/// Why: the parenthesised clause is load-bearing. Git emits
/// `fatal: not a git repository: (null)` for a STALE WORKTREE POINTER, so the
/// shorter phrase `not a git repository` matches a broken repo and a
/// genuinely-absent one alike — and it is the broken repo that still has a live
/// `.gitignore` the indexer must keep honouring.
///
/// Consistent with, NOT proof of: git emits the same text for an unreadable
/// `.git`, where the repository is real. [`classify_probe_failure`] corroborates
/// it with a filesystem witness first.
///
/// What: verified byte-identical against git 2.54.0. Any wording drift falls
/// through to [`WorkTree::Unknown`] — the fail-closed direction. Mirrors
/// `trusty-agents-common`'s `vcs_claim::NO_REPO_STDERR` (#4448/#4727); #4735
/// extracts the shared probe both will call.
/// Test: `probe_work_tree_is_unknown_for_a_stale_worktree_pointer`.
const NO_REPO_STDERR: &str = "not a git repository (or any of the parent directories)";

/// What git can tell us about whether `root_path` sits in a work tree.
///
/// Why: #4733 — reconcile's mtime catch-up walk does NOT honour `.gitignore`
/// (only `SKIP_DIRS` and the walker's skip predicates), so it is safe only for a
/// root that genuinely has no repository. "git says there is no repo" and "git
/// could not be asked" are different facts with opposite safe answers; folding
/// them together indexed gitignored files and made them retrievable through the
/// `search` and `grep` MCP tools.
/// What: [`Present`](Self::Present) — a work tree was confirmed;
/// [`NoRepo`](Self::NoRepo) — corroborated absence of any repository;
/// [`Unknown`](Self::Unknown) — git is missing, failed, or answered about a bare
/// repository, so no conclusion is available.
/// Test: `probe_work_tree_finds_a_real_repo`,
/// `probe_work_tree_reports_no_repo_for_a_plain_directory`,
/// `probe_work_tree_is_unknown_for_a_stale_worktree_pointer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkTree {
    /// `root_path` is inside a git work tree; its ignore rules are live.
    Present,
    /// There is no git repository at or above `root_path`.
    NoRepo,
    /// git could not be consulted. Treat as possibly a repository.
    Unknown,
}

/// Why a failed `rev-parse` failed — the gate the mtime fallback turns on.
///
/// Why: git's "no repository" message is not proof there is no repository. It
/// emits [`NO_REPO_STDERR`] whenever discovery never got far enough to conclude
/// otherwise — an unreadable `.git`, an unreadable `.git/HEAD`, or
/// `GIT_CEILING_DIRECTORIES` stopping the upward walk. In every one of those the
/// repository and its `.gitignore` are real. `symlink_metadata` on an ancestor
/// `.git` needs only the parent's search bit, so it is a witness git does not
/// use; a disagreement between the two IS the "cannot be asked" state.
/// What: [`WorkTree::NoRepo`] only when the message matches AND no ancestor
/// carries a `.git` entry; [`WorkTree::Unknown`] otherwise.
///
/// 🔴 The `.canonicalize()` is load-bearing, not tidiness. `Path::ancestors`
/// walks the path LEXICALLY, so an uncanonicalised relative path (`.`) or one
/// reached through a symlink yields a chain that is not the real one — the
/// project's `.git` is never visited and the permissive answer wins. Canonicalising
/// first makes the walk follow the actual filesystem parentage, and a
/// canonicalisation failure is itself [`WorkTree::Unknown`].
/// Test: `classify_probe_failure_corroborates_the_no_repo_message`,
/// `classify_probe_failure_canonicalises_before_walking_ancestors`.
fn classify_probe_failure(root_path: &Path, stderr: &str) -> WorkTree {
    if !stderr.contains(NO_REPO_STDERR) {
        return WorkTree::Unknown;
    }
    match root_path.canonicalize() {
        Ok(abs)
            if !abs
                .ancestors()
                .any(|p| p.join(".git").symlink_metadata().is_ok()) =>
        {
            WorkTree::NoRepo
        }
        _ => WorkTree::Unknown,
    }
}

/// Ask git whether `root_path` is inside a work tree, in three states (#4733).
///
/// Why: callers that fall back to a less-protective mode when a git probe fails
/// need to know WHY it failed. Only a corroborated "there is no repository here"
/// justifies a `.gitignore`-blind walk; every other outcome must keep the
/// gitignore-honouring path.
///
/// 🔴 THE EXIT CODE ALONE IS NOT A CLASSIFIER. Git has no dedicated exit code
/// for "this is not a repository" — it exits 128 for that AND for `detected
/// dubious ownership`, a stale worktree gitlink, a broken
/// `repositoryformatversion`, or any failing `git` shim on `PATH`. Success is
/// likewise not enough: a BARE repository exits 0 printing `false`, so stdout
/// must read exactly `true`.
///
/// What: runs `git -C <root_path> rev-parse --is-inside-work-tree`; a spawn
/// failure is [`WorkTree::Unknown`], a non-zero exit is delegated to
/// [`classify_probe_failure`], and a zero exit whose stdout is not `true` is
/// [`WorkTree::Unknown`]. Never panics; blocking, like [`head_sha`] beside it,
/// and called only on reconcile's cold fallback path.
/// Test: `probe_work_tree_finds_a_real_repo`,
/// `probe_work_tree_reports_no_repo_for_a_plain_directory`,
/// `probe_work_tree_is_unknown_for_a_stale_worktree_pointer`,
/// `probe_work_tree_is_unknown_when_the_git_binary_is_missing`.
pub fn probe_work_tree(root_path: &Path) -> WorkTree {
    probe_work_tree_with(root_path, "git")
}

/// [`probe_work_tree`] with an injectable git program name.
///
/// Why: the spawn-failure arm (no `git` on `PATH`) is a real, security-relevant
/// branch, and the only alternatives for reaching it are mutating `PATH` —
/// process-global and racy under a parallel test runner. A program-name
/// parameter makes it reachable hermetically.
/// What: identical to [`probe_work_tree`]; `git_bin` names the program to spawn.
/// Test: `probe_work_tree_is_unknown_when_the_git_binary_is_missing`.
fn probe_work_tree_with(root_path: &Path, git_bin: &str) -> WorkTree {
    let out = Command::new(git_bin)
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root_path)
        .output();
    match out {
        Err(_) => WorkTree::Unknown,
        Ok(out) if !out.status.success() => {
            classify_probe_failure(root_path, &String::from_utf8_lossy(&out.stderr))
        }
        Ok(out) if String::from_utf8_lossy(&out.stdout).trim() != "true" => WorkTree::Unknown,
        Ok(_) => WorkTree::Present,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_branch_files_returns_none_when_not_a_repo() {
        // Why: helper must be best-effort. A non-git directory must produce
        // `None`, not a panic.
        let tmp = tempfile::tempdir().unwrap();
        // git merge-base will fail with non-zero exit in a non-repo dir.
        let result = resolve_branch_files(tmp.path(), "nope");
        assert!(result.is_none(), "expected None outside a git repo");
    }

    #[test]
    fn test_head_sha_is_none_outside_git_repo() {
        // Why: `head_sha` must be best-effort. A non-git directory must
        // produce `None`, not a panic.
        let tmp = tempfile::tempdir().unwrap();
        assert!(head_sha(tmp.path()).is_none());
    }

    #[test]
    fn test_normalize_path_strips_leading_dot_slash() {
        assert_eq!(normalize_path("./src/foo.rs"), "src/foo.rs");
        assert_eq!(normalize_path("src/foo.rs"), "src/foo.rs");
        assert_eq!(normalize_path(""), "");
    }

    // ── #4733: three-state work-tree probe ──────────────────────────────

    /// Why: the affirmative case must not be over-refused, or reconcile would
    /// full-reindex every git-backed index on every boot.
    #[test]
    fn probe_work_tree_finds_a_real_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ok = Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        assert!(ok.status.success(), "git init failed");
        assert_eq!(probe_work_tree(tmp.path()), WorkTree::Present);
    }

    /// Why: a genuinely non-git root is the one case the `.gitignore`-blind
    /// mtime walk is legitimate for — over-refusing it would disable
    /// reconciliation for archived tarballs and mounted docs trees.
    #[test]
    fn probe_work_tree_reports_no_repo_for_a_plain_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(probe_work_tree(tmp.path()), WorkTree::NoRepo);
    }

    /// Why: `fatal: not a git repository: (null)` (stale worktree pointer)
    /// contains the substring `not a git repository` while meaning the
    /// opposite. Matching the short phrase is the trap #4733 turns on — the
    /// repository, and its `.gitignore`, are entirely real here.
    #[test]
    fn probe_work_tree_is_unknown_for_a_stale_worktree_pointer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".git"), "gitdir: /nonexistent/xyz-4733\n")
            .expect("write gitlink");
        assert_eq!(probe_work_tree(tmp.path()), WorkTree::Unknown);
    }

    /// Why: git is not always on `PATH` — a stripped container, a broken shim,
    /// a daemon started with a sanitised environment. A spawn failure tells us
    /// nothing about whether a repository exists, so it must refuse.
    #[test]
    fn probe_work_tree_is_unknown_when_the_git_binary_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A plain directory: with a REAL git this is the permissive `NoRepo`.
        assert_eq!(probe_work_tree(tmp.path()), WorkTree::NoRepo);
        // With no git binary at all, the same directory must refuse instead.
        assert_eq!(
            probe_work_tree_with(tmp.path(), "trusty-no-such-git-binary-4733"),
            WorkTree::Unknown,
            "an unspawnable git answers nothing — it must not be read as 'no repository'"
        );
    }

    /// Why: `Path::ancestors` walks LEXICALLY. Without `.canonicalize()` a path
    /// reached through a symlink (or a relative one like `.`) yields a chain
    /// that is not its real parentage, so the project's `.git` is never visited
    /// and the permissive `NoRepo` wins. Dropping the call passes every other
    /// test in this suite — this is the one that fails.
    /// What: `link -> repo/sub`, with `.git` on `repo` only. The lexical
    /// ancestors of `link` never include `repo`; the canonicalised ones do.
    #[test]
    fn classify_probe_failure_canonicalises_before_walking_ancestors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).expect("mkdir repo/sub");
        std::fs::write(repo.join(".git"), "gitdir: /somewhere\n").expect("gitlink");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(repo.join("sub"), &link).expect("symlink");

        let msg = format!("fatal: {NO_REPO_STDERR}: .git");
        assert_eq!(
            classify_probe_failure(&link, &msg),
            WorkTree::Unknown,
            "the real parent carries a .git — only a canonicalised ancestor walk sees it"
        );
    }

    /// Why: git prints the "no repository" text for an unreadable `.git` just
    /// as readily as for an empty directory, so the message is a necessary and
    /// never a sufficient condition; the filesystem witness decides.
    ///
    /// The near-miss assertion is not redundant with
    /// `probe_work_tree_is_unknown_for_a_stale_worktree_pointer`: THERE the
    /// gitlink is itself the `.git` witness, so the witness alone would refuse
    /// even with a too-broad phrase match. Only asserting the wording against a
    /// directory with NO witness pins [`NO_REPO_STDERR`]'s narrowness.
    #[test]
    fn classify_probe_failure_corroborates_the_no_repo_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let msg = format!("fatal: {NO_REPO_STDERR}: .git");
        assert_eq!(classify_probe_failure(tmp.path(), &msg), WorkTree::NoRepo);

        assert_eq!(
            classify_probe_failure(tmp.path(), "fatal: not a git repository: (null)"),
            WorkTree::Unknown,
            "the stale-worktree near-miss contains the short phrase but means the opposite"
        );

        std::fs::write(tmp.path().join(".git"), "gitdir: /somewhere\n").expect("gitlink");
        assert_eq!(
            classify_probe_failure(tmp.path(), &msg),
            WorkTree::Unknown,
            "a .git witness contradicts the message — a disagreement is 'cannot be asked'"
        );

        assert_eq!(
            classify_probe_failure(tmp.path(), "fatal: detected dubious ownership"),
            WorkTree::Unknown,
            "an unrecognised failure never concludes 'no repository'"
        );
    }
}
