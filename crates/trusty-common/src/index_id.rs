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
//! when the project's git origin resolves — suffixed with the git worktree name
//! (`owner-repo-worktree`) for a LINKED worktree, which shares that origin with
//! every other worktree of the same repo and would otherwise collapse onto one
//! id — falling back to [`derive_index_id`]'s basename rule otherwise.
//! Callers that must stay
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
///
/// Worktree disambiguation (the reason this is not JUST `owner-repo`):
/// `RepoIdentity::derive` reads `remote.origin.url` via `git config`, which for
/// a LINKED git worktree transparently resolves to the shared repo config — the
/// identical value the main checkout and every sibling worktree return. That is
/// exactly right for `RepoIdentity`'s own purpose (it is a *grouping* key: all
/// facets of one repo SHOULD share it), but catastrophic for an index id, which
/// is a *partitioning* key: trusty-search's registry holds one `root_path` per
/// id, so a bare `owner-repo` would make the second worktree's registration a
/// silent no-op "find" against the FIRST worktree's index — every subsequent
/// search and incremental update then targeting the wrong content tree. So when
/// [`linked_worktree_name`] reports `project_root` is a linked worktree, its git
/// worktree name is appended (`"<owner>-<repo>-<worktree>"`). Git guarantees
/// that name is unique within a repo and keeps it stable across
/// `git worktree move`, so sibling worktrees can never collide and an id does
/// not churn when a worktree is relocated. The main working tree keeps the bare
/// `owner-repo` form.
///
/// Known limitation (documented, not a bug): the hyphen join is not injective —
/// `owner=a, repo=b-c` and `owner=a-b, repo=c` both render `a-b-c`, and the
/// worktree suffix widens that surface by one component. Slugging can likewise
/// collapse two distinct worktree names (`wt_x` and `wt-x`). Both require
/// deliberately adversarial naming; the alternative (a separator illegal in a
/// slug) would break the single-path-segment constraint above. Left as-is
/// consciously — see the `owner-repo` join rationale in the paragraph above.
/// Test: `derive_preferred_index_id_uses_org_repo_when_remote_resolves`,
/// `derive_preferred_index_id_falls_back_to_basename_without_remote`,
/// `derive_preferred_index_id_distinguishes_worktrees_of_same_repo` in `tests`.
pub fn derive_preferred_index_id(project_root: &Path) -> String {
    match crate::repo_identity::RepoIdentity::derive(project_root) {
        Some(crate::repo_identity::RepoIdentity::GitHub(gp)) => {
            match linked_worktree_name(project_root) {
                Some(worktree) => format!("{}-{}-{}", gp.owner, gp.repo, worktree),
                None => format!("{}-{}", gp.owner, gp.repo),
            }
        }
        _ => derive_index_id(project_root),
    }
}

/// The git worktree name of `dir` when it is a LINKED worktree; `None` for a
/// main working tree, a bare repo, a submodule, or a non-repo (issue #4062).
///
/// Why: [`derive_preferred_index_id`] needs to tell "this directory IS the
/// repo's primary checkout" from "this directory is one of N sibling worktrees
/// sharing the repo's origin remote", because only the latter needs a
/// disambiguating suffix to avoid every worktree collapsing onto one index id.
/// The discriminator has to come from git itself rather than a path heuristic
/// (a `.git` FILE also appears for submodules, whose origin remote is their own
/// — they need no suffix).
/// What: runs one `git -C <dir> rev-parse --git-dir --git-common-dir`. For a
/// main working tree (and a bare repo) git reports the same directory for both;
/// for a linked worktree `--git-dir` is `<common>/worktrees/<name>` while
/// `--git-common-dir` is the shared repo dir, so a mismatch identifies the
/// linked case and the final component of `--git-dir` is git's own per-repo
/// worktree name. Both paths are resolved against `dir` (git may print them
/// relative) and canonicalised before comparison so a relative-vs-absolute or
/// symlinked rendering never reads as a spurious mismatch. The name is passed
/// through [`crate::slugify_string`] — the workspace's one slug rule, already
/// applied to the `owner`/`repo` components — and an empty slug yields `None`
/// (fall back to the bare `owner-repo` form rather than emit a trailing-hyphen
/// id). Returns `None` on any git failure: best-effort, never panics, no
/// network.
/// Test: `derive_preferred_index_id_distinguishes_worktrees_of_same_repo`
/// (linked-worktree path) and
/// `derive_preferred_index_id_uses_org_repo_when_remote_resolves` (main-checkout
/// path, which must NOT gain a suffix).
fn linked_worktree_name(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-dir", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let git_dir = absolutise(dir, lines.next()?.trim())?;
    let common_dir = absolutise(dir, lines.next()?.trim())?;
    if git_dir == common_dir {
        return None; // main working tree (or bare repo): no suffix needed.
    }
    let slug = crate::slugify_string(&git_dir.file_name()?.to_string_lossy());
    (!slug.is_empty()).then_some(slug)
}

/// Resolve a path git printed (possibly relative to `base`) to a comparable
/// absolute form.
///
/// Why: `git rev-parse --git-dir` prints a RELATIVE path (`.git`) for a main
/// checkout but an absolute one for a linked worktree; comparing the two raw
/// strings would report a mismatch for every repo and suffix main checkouts
/// too. Canonicalising both sides makes the comparison meaningful (and immune
/// to symlinked temp dirs — `/tmp` → `/private/tmp` on macOS).
/// What: joins a relative `raw` onto `base` (git runs with `-C base`, so that is
/// its reference point), then canonicalises; falls back to the un-canonicalised
/// join if the path cannot be resolved. `None` only for an empty `raw`.
/// Test: exercised via [`linked_worktree_name`]'s tests.
fn absolutise(base: &Path, raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    Some(std::fs::canonicalize(&joined).unwrap_or(joined))
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

    /// Why (issue #4062 BLOCK finding): `RepoIdentity::derive` shells out to
    /// `git config --get remote.origin.url`, which inside a LINKED worktree
    /// transparently resolves to the SHARED repo config — the identical origin
    /// the main checkout and every sibling worktree report. A bare `owner-repo`
    /// id therefore collapsed every worktree of one repo onto a single id, and
    /// since trusty-search's registry is one `root_path` per id, the second
    /// worktree's registration degraded into a silent no-op "find" against the
    /// FIRST worktree's index — its searches and incremental updates then
    /// targeting the wrong content tree entirely. Every other test in this
    /// module uses a single isolated temp repo, so none of them could catch it:
    /// the collision only exists when TWO roots share ONE remote.
    /// What: builds a real repo with a real origin remote, adds TWO real
    /// `git worktree add` worktrees off it, and asserts all three roots derive
    /// DIFFERENT ids — while the main checkout keeps the bare `owner-repo` form
    /// and each worktree gets it as a prefix (so the repo is still legible in
    /// the id). Also re-asserts the single-path-segment invariant every
    /// trusty-search HTTP route and `--index` arg depends on.
    /// Test: itself (skips cleanly if git is unavailable on the runner).
    #[test]
    fn derive_preferred_index_id_distinguishes_worktrees_of_same_repo() {
        let main = scratch_dir("wt-main");
        let wt_a = scratch_dir("wt-a");
        let wt_b = scratch_dir("wt-b");
        let cleanup = || {
            let _ = fs::remove_dir_all(&main);
            let _ = fs::remove_dir_all(&wt_a);
            let _ = fs::remove_dir_all(&wt_b);
        };

        fs::create_dir_all(&main).unwrap();
        if !git_ok(&main, &["init"]) {
            cleanup();
            return; // no usable git on this runner
        }
        let _ = git_ok(&main, &["config", "user.email", "t@t.test"]);
        let _ = git_ok(&main, &["config", "user.name", "t"]);
        let _ = git_ok(
            &main,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ],
        );
        // `git worktree add` requires at least one commit to check out.
        fs::write(main.join("README.md"), "hi").unwrap();
        let _ = git_ok(&main, &["add", "."]);
        if !git_ok(&main, &["commit", "-m", "init"]) {
            cleanup();
            return; // commit failed — skip
        }
        let added_a = git_ok(&main, &["worktree", "add", &wt_a.to_string_lossy()]);
        let added_b = git_ok(&main, &["worktree", "add", &wt_b.to_string_lossy()]);
        if !(added_a && added_b) {
            cleanup();
            return; // this git cannot create worktrees — skip
        }

        // Pre-condition: the whole hazard is that all three DO share one origin.
        for root in [&main, &wt_a, &wt_b] {
            assert_eq!(
                crate::repo_identity::RepoIdentity::derive(root).map(|r| r.canonical()),
                Some("bobmatnyc/trusty-tools".to_string()),
                "expected the shared origin remote to resolve from {}",
                root.display()
            );
        }

        let main_id = derive_preferred_index_id(&main);
        let a_id = derive_preferred_index_id(&wt_a);
        let b_id = derive_preferred_index_id(&wt_b);

        // The main working tree keeps the bare org/repo id (#4062's actual fix).
        assert_eq!(main_id, "bobmatnyc-trusty-tools");
        // …and each worktree is distinct from it AND from its sibling.
        assert_ne!(a_id, main_id, "worktree A collided with the main checkout");
        assert_ne!(b_id, main_id, "worktree B collided with the main checkout");
        assert_ne!(a_id, b_id, "sibling worktrees collided with each other");
        // The repo stays legible in a worktree id (prefix), and the id remains
        // ONE path segment — every trusty-search route matches `{id}` against
        // exactly one segment.
        for id in [&main_id, &a_id, &b_id] {
            assert!(!id.contains('/'), "index id must be one path segment: {id}");
            assert!(!id.is_empty());
        }
        for id in [&a_id, &b_id] {
            assert!(
                id.starts_with("bobmatnyc-trusty-tools-"),
                "worktree id should keep the org/repo prefix: {id}"
            );
        }

        cleanup();
    }
}
