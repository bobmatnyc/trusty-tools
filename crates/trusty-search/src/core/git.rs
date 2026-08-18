//! Git subprocess helpers for branch-aware search (issue #122) and for
//! merge-base delta resolution (ADR-0050).
//!
//! Why: two callers need to know how a checkout differs from a base ref. The
//! search pipeline uses it to boost branch-modified files when a `SearchQuery`
//! carries `branch: Some(name)` without explicit `branch_files`. ADR-0050's
//! delta-indexed worktree facets use it to decide which files a worktree owes
//! the index at all. Both route through [`resolve_merge_base_delta`] so the
//! two can never disagree about what "differs from the merge-base" means. We
//! shell out rather than linking libgit2 to keep the dependency surface small
//! and to inherit the caller's `.gitconfig` / safe.directory settings
//! unchanged.
//! What: [`resolve_merge_base_delta`] runs `git merge-base HEAD <base_ref>`,
//! then `git diff --name-status -z <base>` (against the WORKING TREE, not
//! `HEAD`) and `git ls-files --others --exclude-standard -z`. Any failure
//! (non-git workdir, unknown ref, detached HEAD, missing binary) returns
//! `None` with a `tracing::warn!` — a caller falls back to no boost rather
//! than failing the search, and never receives a partial delta.
//! Test: covered by unit tests in this module (no-git case, uncommitted work,
//! deletions, renames, refusal arms) and the integration tests in
//! `core::indexer::tests` that exercise the explicit `branch_files` path.

use std::path::Path;
use std::process::Command;

/// How a checkout differs from its merge-base with some other ref.
///
/// Why: ADR-0050 indexes a worktree as the DELTA against its merge-base rather
/// than as a full copy, so the indexer needs both halves of that delta and they
/// mean opposite things. `changed` names files whose current bytes must be
/// (re)indexed; `deleted` names files whose chunks the base facet still carries
/// and which the worktree must shadow as absent. Collapsing them into one list
/// (which is all `resolve_branch_files` returned before) leaves a deletion
/// indistinguishable from an edit, so the stale chunks are never dropped.
/// What: `base_sha` is the resolved merge-base commit; `changed` covers
/// added, modified, and untracked files; `deleted` covers files removed
/// relative to the merge-base. A rename contributes its old path to `deleted`
/// and its new path to `changed`. Paths are repo-root-relative and
/// forward-slash separated, exactly as git prints them.
/// Test: `merge_base_delta_reports_uncommitted_work_and_deletions`,
/// `merge_base_delta_splits_a_rename_into_deleted_and_changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeBaseDelta {
    /// The merge-base commit the delta was computed against.
    pub base_sha: String,
    /// Files to index: added, modified, or untracked relative to the base.
    pub changed: Vec<String>,
    /// Files removed relative to the base; their chunks must be dropped.
    pub deleted: Vec<String>,
}

/// Compute how the checkout at `root_path` differs from its merge-base with
/// `base_ref`.
///
/// Why: ADR-0050 point 5 keys a worktree's index to the merge-base rather than
/// to main's moving HEAD, so a delta stays stable while main advances. The
/// merge-base is compared against the WORKING TREE, not `HEAD`, because a live
/// agent worktree is mostly uncommitted work — `git diff <base>..HEAD` reports
/// only what was committed and would omit the majority of a worktree's real
/// content (measured on a four-change fixture: 1 of 4 differences reported).
/// What: three best-effort git calls — `merge-base`, `diff --name-status -z`,
/// and `ls-files --others --exclude-standard -z`. `-z` is used throughout so
/// paths containing spaces, quotes, or non-ASCII bytes survive verbatim rather
/// than arriving in git's quoted form. `--exclude-standard` keeps `.gitignore`
/// honoured, so build output never enters the delta.
///
/// Returns `None` on ANY failure, including a failure of the untracked-file
/// step alone. A partial delta is worse than no delta here: it reads as a
/// complete answer while silently omitting files that exist in the tree, and
/// the caller has no way to tell the two apart.
/// Test: `merge_base_delta_reports_uncommitted_work_and_deletions`,
/// `merge_base_delta_splits_a_rename_into_deleted_and_changed`,
/// `merge_base_delta_refuses_rather_than_returning_a_partial_delta`,
/// `merge_base_delta_is_none_outside_a_repo`.
pub fn resolve_merge_base_delta(root_path: &Path, base_ref: &str) -> Option<MergeBaseDelta> {
    resolve_merge_base_delta_with(root_path, base_ref, "git")
}

/// [`resolve_merge_base_delta`] with an injectable git program name.
///
/// Why: the refusal arms are the branches that matter most here, and the only
/// hermetic way to reach a spawn failure is to name a program that does not
/// exist — mutating `PATH` is process-global and racy under a parallel test
/// runner. Mirrors [`probe_work_tree_with`] beside it.
/// What: identical to [`resolve_merge_base_delta`]; `git_bin` names the program
/// to spawn.
/// Test: `merge_base_delta_refuses_rather_than_returning_a_partial_delta`.
fn resolve_merge_base_delta_with(
    root_path: &Path,
    base_ref: &str,
    git_bin: &str,
) -> Option<MergeBaseDelta> {
    let base_sha = merge_base_sha(root_path, base_ref, git_bin)?;

    // #5815: diff the base against the WORKING TREE — `<base>..HEAD` compares
    // two commits and misses the uncommitted work a worktree mostly consists of.
    let diff = run_git(
        root_path,
        git_bin,
        &["diff", "--name-status", "-z", &base_sha],
    )?;
    let (mut changed, deleted) = parse_name_status_z(&diff);

    // #5815: an untracked file is real worktree content the delta owes the
    // index. Failing this step yields a delta that LOOKS complete, so refuse.
    let untracked = run_git(
        root_path,
        git_bin,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    changed.extend(split_z(&untracked).map(str::to_owned));

    Some(MergeBaseDelta {
        base_sha,
        changed,
        deleted,
    })
}

/// Resolve the merge-base commit between `HEAD` and `base_ref`.
fn merge_base_sha(root_path: &Path, base_ref: &str, git_bin: &str) -> Option<String> {
    let out = run_git(root_path, git_bin, &["merge-base", "HEAD", base_ref])?;
    let sha = out.trim().to_owned();
    if sha.is_empty() {
        tracing::warn!("merge-base resolution failed for '{base_ref}': empty merge-base");
        return None;
    }
    Some(sha)
}

/// Run one git subcommand and return its stdout, or `None` with a warning.
///
/// Why: the three calls above share identical failure handling, and a silent
/// divergence between them is how a partial delta gets returned.
fn run_git(root_path: &Path, git_bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(git_bin)
        .args(args)
        .current_dir(root_path)
        .output()
        .ok()?;
    if !out.status.success() {
        tracing::warn!(
            "git {:?} exited {:?} in {}",
            args,
            out.status.code(),
            root_path.display()
        );
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Split a NUL-delimited git output stream into its non-empty fields.
fn split_z(body: &str) -> impl Iterator<Item = &str> {
    body.split('\0').filter(|f| !f.is_empty())
}

/// Parse `git diff --name-status -z` into `(changed, deleted)`.
///
/// Why: the `-z` stream is not line-oriented and a rename carries a different
/// field count from every other status, so a naive pairwise walk desynchronises
/// on the first rename and mis-attributes every path after it.
/// What: fields arrive as `status`, then one path — except `R…`/`C…`, which
/// carry a source path AND a destination path. A rename's source is a deletion
/// and its destination is a change; a copy's source is untouched.
/// Test: `merge_base_delta_splits_a_rename_into_deleted_and_changed`.
fn parse_name_status_z(body: &str) -> (Vec<String>, Vec<String>) {
    let (mut changed, mut deleted) = (Vec::new(), Vec::new());
    let mut fields = split_z(body);
    while let Some(status) = fields.next() {
        let Some(first) = fields.next() else { break };
        match status.as_bytes().first() {
            Some(b'D') => deleted.push(first.to_owned()),
            Some(b'R') => {
                let Some(dest) = fields.next() else { break };
                deleted.push(first.to_owned());
                changed.push(dest.to_owned());
            }
            Some(b'C') => {
                let Some(dest) = fields.next() else { break };
                changed.push(dest.to_owned());
            }
            _ => changed.push(first.to_owned()),
        }
    }
    (changed, deleted)
}

/// Compute the list of files that differ from the merge-base with `branch`.
///
/// Why: the branch boost (#122) exists to surface what the developer is
/// working on, and most of that work is uncommitted at the moment they search
/// for it. This delegates to [`resolve_merge_base_delta`] so the boost sees the
/// same file set ADR-0050's delta indexing does.
/// What: returns [`MergeBaseDelta::changed`] — added, modified, and untracked
/// files. Deletions are omitted deliberately: a deleted file has no chunks to
/// boost. Paths are repo-root-relative, forward-slash separated.
///
/// Returns `None` on any failure — the caller treats that as "no boost".
/// Test: `resolve_branch_files_reports_uncommitted_work`,
/// `test_resolve_branch_files_returns_none_when_not_a_repo`.
pub fn resolve_branch_files(root_path: &Path, branch: &str) -> Option<Vec<String>> {
    // #5815: delegate so the boost and ADR-0050's delta indexing agree on what
    // "differs from the merge-base" means.
    resolve_merge_base_delta(root_path, branch).map(|d| d.changed)
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

    /// Build a repo on `main` with `kept/deleted/modified` committed, then a
    /// `feature` branch carrying one committed add. Returns the repo path.
    fn fixture_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = tmp.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(p.join("kept.rs"), "base\n").unwrap();
        std::fs::write(p.join("deleted.rs"), "old\n").unwrap();
        std::fs::write(p.join("modified.rs"), "x\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        git(&["checkout", "-qb", "feature"]);
        std::fs::write(p.join("committed_change.rs"), "committed\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "feat"]);
        tmp
    }

    /// Why: THE regression test for ADR-0050's delta model. A live agent
    /// worktree is mostly uncommitted work, and the previous implementation
    /// (`git diff --name-only <base>..HEAD`) compared two COMMITS — so on this
    /// fixture it reported 1 of the 4 real differences, silently omitting the
    /// uncommitted edit, the deletion, and the untracked file. Indexing a
    /// worktree from that answer would omit most of its content.
    ///
    /// Fails against the pre-change implementation on every assertion below
    /// except `committed_change.rs`.
    #[test]
    fn merge_base_delta_reports_uncommitted_work_and_deletions() {
        let tmp = fixture_repo();
        let p = tmp.path();
        // Uncommitted work: an edit, a removal, and a brand-new file.
        std::fs::write(p.join("modified.rs"), "x\nuncommitted\n").unwrap();
        std::fs::remove_file(p.join("deleted.rs")).unwrap();
        std::fs::write(p.join("untracked.rs"), "brand new\n").unwrap();

        let delta = resolve_merge_base_delta(p, "main").expect("delta");

        assert!(!delta.base_sha.is_empty(), "merge-base must be resolved");
        assert!(
            delta.changed.contains(&"committed_change.rs".to_owned()),
            "committed add must be in the delta: {:?}",
            delta.changed
        );
        assert!(
            delta.changed.contains(&"modified.rs".to_owned()),
            "UNCOMMITTED edit must be in the delta: {:?}",
            delta.changed
        );
        assert!(
            delta.changed.contains(&"untracked.rs".to_owned()),
            "UNTRACKED file must be in the delta: {:?}",
            delta.changed
        );
        assert_eq!(
            delta.deleted,
            vec!["deleted.rs".to_owned()],
            "a removal must be reported as deleted, never as changed"
        );
        assert!(
            !delta.changed.contains(&"kept.rs".to_owned()),
            "an unchanged file must not enter the delta: {:?}",
            delta.changed
        );
    }

    /// Why: a rename is the one status carrying TWO paths in the `-z` stream.
    /// A pairwise walk desynchronises on it and mis-attributes every path
    /// after it. The old path must be deleted (its base chunks are stale) and
    /// the new path indexed.
    #[test]
    fn merge_base_delta_splits_a_rename_into_deleted_and_changed() {
        let tmp = fixture_repo();
        let p = tmp.path();
        let out = Command::new("git")
            .args(["mv", "kept.rs", "renamed.rs"])
            .current_dir(p)
            .output()
            .expect("git mv");
        assert!(out.status.success(), "git mv failed");
        // A second change AFTER the rename: proves the parser stayed in sync.
        std::fs::write(p.join("modified.rs"), "x\nafter rename\n").unwrap();

        let delta = resolve_merge_base_delta(p, "main").expect("delta");

        assert!(
            delta.deleted.contains(&"kept.rs".to_owned()),
            "the rename source must be deleted: {:?}",
            delta.deleted
        );
        assert!(
            delta.changed.contains(&"renamed.rs".to_owned()),
            "the rename destination must be changed: {:?}",
            delta.changed
        );
        assert!(
            delta.changed.contains(&"modified.rs".to_owned()),
            "a path AFTER the rename must not be mis-attributed: {:?}",
            delta.changed
        );
    }

    /// A `git` shim that forwards to real git EXCEPT `ls-files --others`,
    /// which it fails.
    ///
    /// Why: an unspawnable binary is not enough to test the untracked step's
    /// refusal — it fails the FIRST call (`merge-base`) and the function
    /// returns before the step under test ever runs. A test built that way
    /// passes against a deliberately fail-open implementation, which is how it
    /// would miss the very defect it exists to catch. Only a git that answers
    /// the first two calls and fails the third reaches the arm.
    fn git_shim_failing_untracked(dir: &Path) -> String {
        let shim = dir.join("git-shim.sh");
        std::fs::write(
            &shim,
            "#!/bin/sh\nfor a in \"$@\"; do\n  [ \"$a\" = \"--others\" ] && exit 1\ndone\nexec git \"$@\"\n",
        )
        .expect("write shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        shim.to_string_lossy().into_owned()
    }

    /// Why: the fail-open arm. The untracked-file step runs LAST, after the
    /// merge-base and the tracked diff have both succeeded, so failing it open
    /// would return a delta that reads as complete while omitting every new
    /// file in the tree — and the caller cannot tell the two apart. Refusing is
    /// the only answer that stays distinguishable.
    /// What: a shim that answers the first two git calls and fails only
    /// `ls-files --others`, so the fixture itself stays a perfectly good repo
    /// and nothing but the refusal rule can produce `None`.
    #[test]
    fn merge_base_delta_refuses_rather_than_returning_a_partial_delta() {
        let tmp = fixture_repo();
        let p = tmp.path();
        std::fs::write(p.join("untracked.rs"), "brand new\n").unwrap();
        let shim_home = tempfile::tempdir().expect("tempdir");
        let shim = git_shim_failing_untracked(shim_home.path());

        // Control: with real git the same tree resolves AND sees the new file,
        // so the refusal below cannot be blamed on the fixture.
        let ok = resolve_merge_base_delta_with(p, "main", "git").expect("real git resolves");
        assert!(ok.changed.contains(&"untracked.rs".to_owned()));

        assert_eq!(
            resolve_merge_base_delta_with(p, "main", &shim),
            None,
            "a failed untracked-file step must refuse, never return a partial delta"
        );
    }

    /// Why: the boost path and the delta path must agree on what changed, so
    /// the file a developer is actively editing is boosted before they commit
    /// it. Returns `None` on the pre-change implementation's committed-only
    /// answer.
    #[test]
    fn resolve_branch_files_reports_uncommitted_work() {
        let tmp = fixture_repo();
        let p = tmp.path();
        std::fs::write(p.join("modified.rs"), "x\nuncommitted\n").unwrap();
        std::fs::remove_file(p.join("deleted.rs")).unwrap();

        let files = resolve_branch_files(p, "main").expect("branch files");

        assert!(
            files.contains(&"modified.rs".to_owned()),
            "an uncommitted edit must be boostable: {files:?}"
        );
        assert!(
            !files.contains(&"deleted.rs".to_owned()),
            "a deleted file has no chunks to boost: {files:?}"
        );
    }

    /// Why: the merge-base step's own refusal must survive the rewrite.
    #[test]
    fn merge_base_delta_is_none_outside_a_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_merge_base_delta(tmp.path(), "main"), None);
    }

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
