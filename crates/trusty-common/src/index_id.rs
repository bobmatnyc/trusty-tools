//! Canonical trusty-search index-id derivation from a project path.
//!
//! Why: trusty-search derives an index id from the current project (the
//! git-root basename, fallback to the cwd basename) when serving CLI queries,
//! but the MCP `serve` path never reached that logic — so trusty-mpm, which
//! injects a contextless `trusty-search serve` MCP stub, left index selection
//! to the LLM and routinely resolved the WRONG index (issue #1373). To
//! register-and-pin the correct project index, BOTH trusty-mpm (at session
//! launch) and trusty-search (in `detect_project`) must derive the *identical*
//! id from the same project root. Centralising the one rule here in
//! `trusty-common` — which both crates already depend on, and which avoids a
//! trusty-mpm → trusty-search dependency edge (trusty-search pulls the heavy
//! ONNX/usearch stack) — makes it the single source of truth so the two cannot
//! silently diverge.
//!
//! What: [`resolve_project_root`] walks up from a starting directory to the
//! nearest `.git` root (fallback: the start dir itself), and
//! [`derive_index_id`] turns a project root into its index id (the path
//! basename, preserved verbatim for backward-compatibility with already-indexed
//! projects). [`derive_preferred_index_id`] (issue #4062) is the newer,
//! preferred derivation: it returns a `RepoIdentity`-derived `owner-repo` token
//! when the project's git origin resolves, falling back to
//! [`derive_index_id`]'s basename rule otherwise. Callers that must stay
//! backward-compatible with indexes already registered under the legacy
//! basename id should use [`crate::search_index::resolve_effective_index_id`]
//! (feature `search-index`), which adds the alias-fallback lookup on top of
//! this module's pure derivation. No global state; pure functions.
//!
//! Test: `cargo test -p trusty-common -- index_id::tests` covers basename
//! derivation, the git-root walk, the no-marker fallback, and the preferred-id
//! / legacy-fallback split.

use std::path::{Path, PathBuf};

/// Walk up from `start` to the nearest directory containing a `.git` entry.
///
/// Why: a trusty-search index is keyed to a project root, and the canonical
/// project root is the git repository root. Both trusty-mpm (resolving the
/// session's project) and trusty-search (`detect_project`) must agree on which
/// directory is "the project root" so they derive the same index id (#1373).
/// What: returns the first ancestor of `start` (inclusive) that contains a
/// `.git` directory or file; when none is found, returns `start` itself
/// (a path with no enclosing git repo is still indexable by its own basename).
///
/// Known limitation: this returns the FIRST (innermost) ancestor with a `.git`,
/// so in a nested-repo / monorepo layout where a parent directory above the
/// intended project also has `.git`, the *inner* repo wins — and if the project
/// itself has no `.git` but a parent does, that parent `.git` would win. This
/// matches trusty-search's prior `detect_project` semantics (the two must agree
/// to derive the same index id), so it is intentional, not a bug; documented
/// here so a future monorepo-aware override is a conscious change, not a surprise.
/// Test: `resolve_project_root_finds_git_root` and
/// `resolve_project_root_falls_back_to_start` in `tests`.
pub fn resolve_project_root(start: &Path) -> PathBuf {
    find_git_root(start).unwrap_or_else(|| start.to_path_buf())
}

/// Walk up from `start` to the nearest `.git` root, returning `None` when no
/// enclosing git repository exists.
///
/// Why: some callers must DISTINGUISH "inside a git repo" from "no repo at all"
/// — [`resolve_project_root`] can't, because it collapses both cases to a
/// `PathBuf` (returning `start` itself on a miss). trusty-code's task-start
/// index hook uses this to cheaply short-circuit the bake-off/scratch case: a
/// throwaway directory with no `.git` has nothing worth registering with
/// trusty-search, so indexing is skipped entirely rather than creating an index
/// keyed to a directory that will be deleted moments later.
/// What: returns `Some(first_ancestor_with_.git)` (inclusive of `start`), or
/// `None` when the walk reaches the filesystem root without finding one. `.git`
/// is matched via `exists()` so both a normal clone (`.git` directory) and a git
/// worktree/submodule (`.git` file) resolve. This is the exact walk
/// [`resolve_project_root`] performs, exposed as an `Option` — the two share one
/// implementation so they can never disagree on which directory is the root.
/// Test: `find_git_root_some_when_repo`, `find_git_root_none_when_no_repo`.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        // `.git` is a directory in a normal clone and a file in a git worktree
        // / submodule; `exists()` matches both so worktrees resolve correctly.
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Derive the trusty-search index id for a project root.
///
/// Why: the index id is the stable handle every search/grep call targets. It
/// MUST be derived identically wherever it is computed (trusty-mpm's
/// register-and-pin at launch, trusty-search's `detect_project`) or a session
/// would create/pin one id while querying another (#1373).
/// What: returns the final path component of `project_root` as a `String`
/// (lossy on non-UTF-8). The basename is preserved verbatim — NOT slugified —
/// so the derived id byte-for-byte matches the ids trusty-search already
/// assigned to existing on-disk indexes (changing the casing/punctuation would
/// orphan every previously-indexed project). An empty / root path yields the
/// empty string; callers that need a non-empty id must guard that case.
/// Test: `derive_index_id_uses_basename` and `derive_index_id_empty_for_root`
/// in `tests`.
pub fn derive_index_id(project_root: &Path) -> String {
    project_root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Derive the PREFERRED trusty-search index id for a project root (issue
/// #4062, scoped follow-up to DOC-37 / #2611).
///
/// Why: [`derive_index_id`]'s bare-basename id carries no information about
/// which actual GitHub project a directory is, and two unrelated checkouts
/// that happen to share a directory name collide on it. `RepoIdentity` (used
/// today only as an ADDITIVE `repo_identity` join-key field on `PersistedIndex`,
/// #2611) already derives a path-independent `owner/repo` identity from the
/// git origin remote — this function promotes that identity to the id itself
/// for *newly-registered* indexes, while leaving [`derive_index_id`]'s
/// basename rule as the one true fallback for repos with no resolvable
/// remote, so behavior for local-only / content-hash-only projects is
/// unchanged. This function alone does NOT make lookups backward-compatible
/// with already-registered basename-keyed indexes — see
/// [`crate::search_index::resolve_effective_index_id`] (feature
/// `search-index`) for the alias-fallback that must be used at any
/// registration/lookup call site.
/// What: tries [`crate::repo_identity::RepoIdentity::derive`] first; when it
/// resolves to the `GitHub(owner/repo)` variant, joins `owner` and `repo` with
/// a single hyphen (`"<owner>-<repo>"`) — deliberately NOT
/// [`crate::repo_identity::RepoIdentity::canonical`]'s `"<owner>/<repo>"` form,
/// because `index_id` is used as a literal, single-segment token in every
/// `trusty-search` HTTP route (`/indexes/{id}/status`, `/search`, …) and MCP
/// `--index <id>` arg; an embedded `/` would silently break routing across
/// that entire surface (axum matches `{id}` against exactly one path
/// segment). Mirrors the same single-hyphen join
/// [`crate::palace_id::owner_repo_from_git_remote`] already uses for the
/// analogous trusty-memory palace-id problem. Otherwise (no git repo, no
/// origin remote, or a remote-less repo that only yields the `ContentHash`
/// variant) falls back to [`derive_index_id`]'s bare-basename rule unchanged.
/// Test: `derive_preferred_index_id_uses_org_repo_when_remote_resolves`,
/// `derive_preferred_index_id_falls_back_to_basename_without_remote` in
/// `tests`.
pub fn derive_preferred_index_id(project_root: &Path) -> String {
    match crate::repo_identity::RepoIdentity::derive(project_root) {
        Some(crate::repo_identity::RepoIdentity::GitHub(gp)) => {
            format!("{}-{}", gp.owner, gp.repo)
        }
        _ => derive_index_id(project_root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-index-id-{tag}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn derive_index_id_uses_basename() {
        assert_eq!(
            derive_index_id(Path::new("/Users/me/code/trusty-tools")),
            "trusty-tools"
        );
        // Casing and punctuation are preserved verbatim (NOT slugified) so the
        // id matches what trusty-search already stored for existing indexes.
        assert_eq!(
            derive_index_id(Path::new("/Users/me/code/MyProject")),
            "MyProject"
        );
        assert_eq!(
            derive_index_id(Path::new("/srv/Repo_With_Underscores")),
            "Repo_With_Underscores"
        );
    }

    #[test]
    fn derive_index_id_empty_for_root() {
        assert_eq!(derive_index_id(Path::new("/")), "");
    }

    #[test]
    fn resolve_project_root_finds_git_root() {
        let tmp = scratch_dir("git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        let root = resolve_project_root(&nested);
        assert_eq!(root, tmp);
        // And the derived id is the git-root basename, not the nested dir.
        assert_eq!(derive_index_id(&root), derive_index_id(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_project_root_falls_back_to_start() {
        let tmp = scratch_dir("no-git");
        fs::create_dir_all(&tmp).unwrap();

        let root = resolve_project_root(&tmp);
        assert_eq!(root, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_git_root_some_when_repo() {
        let tmp = scratch_dir("fgr-git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_git_root(&nested), Some(tmp.clone()));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_git_root_none_when_no_repo() {
        // A scratch dir with no `.git` anywhere up the chain: the tcode
        // short-circuit relies on this returning None so nothing is indexed.
        let tmp = scratch_dir("fgr-no-git");
        fs::create_dir_all(&tmp).unwrap();

        assert_eq!(find_git_root(&tmp), None);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Run a git subcommand in `dir`, returning whether it succeeded.
    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Why (issue #4062): a project whose git origin remote resolves to a
    /// GitHub-style identity must get the `owner/repo` form, not the bare
    /// basename — this is the entire point of the preferred-id derivation.
    /// What: inits a temp repo named differently from `owner/repo`, adds an
    /// origin remote, and asserts `derive_preferred_index_id` returns the
    /// canonical `owner/repo` string (NOT the directory's own basename).
    /// Test: itself (skips cleanly if git is unavailable on the runner).
    #[test]
    fn derive_preferred_index_id_uses_org_repo_when_remote_resolves() {
        let tmp = scratch_dir("preferred-remote");
        fs::create_dir_all(&tmp).unwrap();
        if !git_ok(&tmp, &["init"]) {
            let _ = fs::remove_dir_all(&tmp);
            return; // no usable git on this runner
        }
        let _ = git_ok(
            &tmp,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ],
        );

        assert_eq!(derive_preferred_index_id(&tmp), "bobmatnyc-trusty-tools");
        // The basename fallback would have been the scratch dir's own name —
        // proving the preferred id is NOT just falling through to it.
        assert_ne!(derive_preferred_index_id(&tmp), derive_index_id(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Why (issue #4062): a repo with no origin remote (or no repo at all)
    /// must keep returning the exact same id [`derive_index_id`] always has —
    /// this is the backward-compatibility half of the preferred-id contract,
    /// so a local-only / content-hash-only project's id never changes underfoot.
    /// What: asserts `derive_preferred_index_id` equals `derive_index_id` for
    /// (a) a plain non-repo directory, and (b) a git repo with a commit but no
    /// origin remote (content-hash `RepoIdentity` fallback).
    /// Test: itself (skips cleanly if git is unavailable on the runner).
    #[test]
    fn derive_preferred_index_id_falls_back_to_basename_without_remote() {
        // (a) not a repo at all.
        let plain = scratch_dir("preferred-plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(derive_preferred_index_id(&plain), derive_index_id(&plain));
        let _ = fs::remove_dir_all(&plain);

        // (b) a repo with a commit but no origin remote.
        let tmp = scratch_dir("preferred-no-remote");
        fs::create_dir_all(&tmp).unwrap();
        if !git_ok(&tmp, &["init"]) {
            let _ = fs::remove_dir_all(&tmp);
            return; // no usable git on this runner
        }
        let _ = git_ok(&tmp, &["config", "user.email", "t@t.test"]);
        let _ = git_ok(&tmp, &["config", "user.name", "t"]);
        fs::write(tmp.join("README.md"), "hi").unwrap();
        let _ = git_ok(&tmp, &["add", "."]);
        if !git_ok(&tmp, &["commit", "-m", "init"]) {
            let _ = fs::remove_dir_all(&tmp);
            return; // commit failed — skip
        }

        assert_eq!(derive_preferred_index_id(&tmp), derive_index_id(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }
}
