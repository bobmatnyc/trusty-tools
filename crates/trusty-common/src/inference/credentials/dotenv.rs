//! The one shared `.env.local` loader (issue #2401).
//!
//! Why: `trusty-agents` (`runtime/startup.rs:114-119`) and `trusty-search`
//! (`main.rs:1155`) each call `dotenvy` ad hoc today, with slightly
//! different search strategies (cwd-only vs. cwd + detected project dir).
//! This module is the canonical replacement — designed so those two crates
//! can adopt it in their own migration tickets without re-deriving the
//! search logic — but this ticket does **not** modify either call site; it
//! only ships the shared loader.
//! What: [`load_env_local_once`] is the production entry point: idempotent
//! (a `OnceLock` — repeated calls across a process are free), searches from
//! the current working directory upward for a `.env.local` file (mirroring
//! Cargo/git's own upward-search convention for "workspace root" discovery),
//! bounded at the first repository/workspace root so it can never wander past
//! this checkout into an unrelated ancestor or `$HOME` (issue #2474). A linked
//! worktree's own `.git` pointer file is NOT that boundary — this repo's
//! write-side sessions always run inside one (`.claude/worktrees/*`,
//! `.worktrees/*`), so the walk resolves the pointer to the shared main
//! checkout root and keeps climbing to it (see [`resolve_worktree_main_root`]);
//! only a `.git` DIRECTORY or a workspace `Cargo.toml` is a hard stop. Because
//! `dotenvy` never overrides an already-set environment variable, this
//! naturally implements
//! "process env beats `.env.local`" — the resolver's tier check
//! (`std::env::var`) is what actually observes the precedence; this loader
//! just populates the process environment once, then gets out of the way.
//! [`find_workspace_env_local`] and [`load_env_from_path`] are the hermetic
//! cores used by the resolver's precedence tests, which must not depend on
//! `OnceLock`'s once-only semantics or the real repo's `.env.local`.
//! Test: `dotenv_tests` (sibling file).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Search `start` and each ancestor directory for a `.env.local` file, never
/// climbing past the first repository/workspace root.
///
/// Why: issue #2474 — the previous unbounded walk climbed `parent()` all the
/// way to the filesystem root with no repo-root boundary. In this
/// worktree-heavy repo (`.claude/worktrees/*`, `.worktrees/*` nested several
/// levels deep) that could accidentally load a `.env.local` from an unrelated
/// ancestor directory (or `$HOME`), substituting the wrong credentials — a
/// footgun against the intended "repo-root, gitignored" contract. This is
/// also the hermetic core for [`load_env_local_once`]; tests point it at a
/// temp directory tree instead of the real cwd.
///
/// A first cut of this fix treated ANY `.git` entry — file or directory — as
/// a hard boundary. That broke the common case in THIS repo: every
/// write-side session runs inside a linked worktree, whose `.git` is a
/// pointer file, so the walk stopped at the worktree's own root and could
/// never reach the main checkout's `.env.local` one or more levels above it —
/// contradicting [`load_env_local_once`]'s documented "picked up regardless
/// of which subdirectory a binary was launched from" contract. A `.git`
/// DIRECTORY is still a hard boundary (that IS a repository root); a `.git`
/// FILE instead has its `gitdir:` pointer resolved via
/// [`resolve_worktree_main_root`], and — only when that resolves to a real
/// filesystem ancestor of the worktree — the walk keeps climbing, now bound
/// at that main checkout root instead of the worktree root. An unreadable or
/// malformed pointer, or a resolved root that is NOT an ancestor (an unusual,
/// disjoint worktree layout), falls back to the conservative original
/// behaviour: treat the worktree's own root as the hard boundary rather than
/// risk an unbounded climb.
/// What: returns the first `.env.local` found walking upward from `start`.
/// Each candidate directory is checked BEFORE any boundary test, so a
/// `.env.local` sitting directly in a boundary directory itself is still
/// found. `None` if no ancestor up to (and including) the boundary has one.
/// Test: `dotenv_tests::finds_env_local_in_ancestor`,
/// `dotenv_tests::absent_env_local_returns_none`,
/// `dotenv_tests::walk_stops_at_git_dir_boundary`,
/// `dotenv_tests::walk_stops_at_git_file_boundary`,
/// `dotenv_tests::walk_finds_env_local_at_the_boundary_itself`,
/// `dotenv_tests::workspace_cargo_toml_is_a_boundary`,
/// `dotenv_tests::worktree_pointer_reaches_env_local_in_main_checkout_root`,
/// `dotenv_tests::real_git_worktree_add_reaches_main_checkout_env_local`.
pub fn find_workspace_env_local(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    // Once a linked worktree's own `.git` pointer resolves to a real,
    // ancestor main-checkout root, THAT directory — not the worktree root —
    // becomes the true boundary; this holds the resolved root (canonicalised,
    // see `canonical`) until the walk reaches it.
    let mut worktree_main_root: Option<PathBuf> = None;
    while let Some(d) = dir {
        let candidate = d.join(".env.local");
        if candidate.is_file() {
            return Some(candidate);
        }
        if worktree_main_root
            .as_deref()
            .is_some_and(|root| canonical(&d) == root)
        {
            return None;
        }
        let git_marker = d.join(".git");
        if git_marker.is_dir() {
            return None;
        }
        if git_marker.is_file() {
            match resolve_worktree_main_root(&git_marker) {
                Some(root)
                    if d.ancestors()
                        .skip(1)
                        .any(|a| canonical(a) == canonical(&root)) =>
                {
                    worktree_main_root = Some(canonical(&root));
                }
                // Malformed/unreadable pointer, or a resolved root that is
                // not actually an ancestor of this worktree — conservatively
                // stop here rather than risk an unbounded climb.
                _ => return None,
            }
        } else if is_workspace_cargo_toml(&d) {
            return None;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

/// Best-effort canonicalisation for boundary-path comparisons.
///
/// Why: a `gitdir:` pointer git itself writes is already fully resolved
/// (symlink-free); a tempdir-based `start`/ancestor path is often NOT (e.g.
/// macOS's `TMPDIR` is `/var/folders/...`, a symlink to
/// `/private/var/folders/...`), so a literal `PathBuf` equality between a
/// resolved `gitdir:` root and an unresolved ancestor path can spuriously
/// fail even when they name the same directory. Both sides of every boundary
/// comparison in [`find_workspace_env_local`] go through this first.
/// What: returns `path.canonicalize()`, or `path` unchanged when
/// canonicalisation fails (e.g. a race where the directory was removed) —
/// never panics, never errors out of the walk.
/// Test: exercised via `dotenv_tests::real_git_worktree_add_reaches_main_checkout_env_local`
/// (the scenario this exists for) and every other boundary test (where
/// `path` has no symlink component, so canonicalisation is a no-op).
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `dir` contains a `Cargo.toml` with a `[workspace]` table header.
///
/// Why: factored out of the walk so the workspace-root boundary rule is
/// independently testable — one of the two boundary markers issue #2474's
/// spec calls for ("`.git` (or workspace `Cargo.toml`) marker"); the other,
/// `.git`, is handled inline in [`find_workspace_env_local`] because it needs
/// different treatment for a directory vs. a linked worktree's pointer file.
/// What: cheap substring heuristic (not a full TOML parse) — sufficient for a
/// footgun-prevention boundary; false positives/negatives are not a security
/// concern here, unlike a directory-traversal check.
/// Test: `dotenv_tests::workspace_cargo_toml_is_a_boundary`,
/// `dotenv_tests::plain_package_cargo_toml_is_not_a_boundary`.
fn is_workspace_cargo_toml(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml"))
        .is_ok_and(|contents| contents.contains("[workspace]"))
}

/// Resolve a linked worktree's `.git` pointer file to the main checkout root.
///
/// Why: the follow-up fix to issue #2474 — distinguishes "this worktree's own
/// root" (not a real repository boundary the search should stop at) from a
/// genuine unrelated ancestor, so [`find_workspace_env_local`] can keep
/// climbing past a worktree root to the shared main checkout above it. An
/// independent review's empirical repro caught the first cut of this function
/// assuming the shared git-common-directory is always literally named
/// `.git` — true for a vanilla `git worktree add`, but NOT for this repo's own
/// trusty-mpm worktree tooling, which uses a common dir named `.base`
/// (`git --git-dir=.base …`). Git's on-disk layout for a linked worktree's
/// admin directory is structurally fixed regardless of that name —
/// `<commondir>/worktrees/<name>` — so this resolves via that STRUCTURE
/// (look for the `worktrees` path segment) rather than assuming any
/// particular commondir basename.
/// What: `worktree_git_file` is the path to a `.git` FILE (e.g.
/// `<worktree>/.git`). Reads it, finds a `gitdir:` line, and resolves that
/// path (relative to `worktree_git_file`'s parent when not absolute) — this
/// is `<commondir>/worktrees/<name>`. Returns `<commondir>`'s PARENT (the
/// checkout root the commondir itself lives in — e.g. `.base`'s parent, or a
/// vanilla `.git`'s parent) when the resolved path's immediate parent
/// directory is literally named `worktrees` (the structural sanity check);
/// `None` on any I/O error, missing/empty `gitdir:` line, or a resolved path
/// that doesn't match that shape — callers must treat `None` as "cannot
/// safely resolve", never as license to keep walking unbounded.
/// Test: `dotenv_tests::resolves_main_checkout_root_from_worktree_pointer`,
/// `dotenv_tests::resolves_main_checkout_root_with_non_dot_git_commondir_name`,
/// `dotenv_tests::malformed_pointer_file_returns_none`,
/// `dotenv_tests::real_git_worktree_add_reaches_main_checkout_env_local`,
/// `dotenv_tests::real_git_worktree_add_with_renamed_base_commondir_reaches_env_local`.
fn resolve_worktree_main_root(worktree_git_file: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(worktree_git_file).ok()?;
    let gitdir = contents
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?;
    let gitdir = gitdir.trim();
    if gitdir.is_empty() {
        return None;
    }
    let gitdir_path = PathBuf::from(gitdir);
    let gitdir_path = if gitdir_path.is_absolute() {
        gitdir_path
    } else {
        worktree_git_file.parent()?.join(gitdir_path)
    };
    // `<commondir>/worktrees/<name>` — a fixed structural contract of git's
    // linked-worktree admin layout, independent of what `<commondir>` itself
    // is named.
    let worktrees_dir = gitdir_path.parent()?;
    if worktrees_dir.file_name() != Some(std::ffi::OsStr::new("worktrees")) {
        return None;
    }
    let commondir = worktrees_dir.parent()?;
    commondir.parent().map(Path::to_path_buf)
}

/// Load a specific `.env.local` path into the process environment.
///
/// Why: the resolver's precedence tests need to simulate "the `.env.local`
/// tier fired" without touching [`load_env_local_once`]'s process-wide
/// `OnceLock` (which would make a second test's load a silent no-op) or the
/// real cwd search.
/// What: thin wrapper over `dotenvy::from_path`; returns `true` on success.
/// Like `dotenvy` generally, this never overrides a variable already present
/// in the process environment.
/// Test: `dotenv_tests::load_env_from_path_sets_new_var`,
/// `dotenv_tests::load_env_from_path_does_not_override_existing`.
pub fn load_env_from_path(path: &Path) -> bool {
    dotenvy::from_path(path).is_ok()
}

/// Idempotent, cwd-upward-search `.env.local` loader for production use.
///
/// Why: the resolver calls this once before checking `std::env::var` so a
/// developer's `.env.local` (at the repo root, or any ancestor of cwd) is
/// picked up regardless of which subdirectory a binary was launched from —
/// the same problem `trusty-agents` fixed ad hoc for itself in #250.
/// What: on first call, searches upward from the current working directory
/// via [`find_workspace_env_local`] and loads the first hit (if any) via
/// `dotenvy::from_path`. Subsequent calls are a no-op `OnceLock` check.
/// Errors (missing file, malformed file) are swallowed — a missing
/// `.env.local` is the common case (production/CI), not a failure.
/// Test: exercised indirectly via `resolver::resolve_key`'s production path;
/// the once-only + cwd-search behaviour itself is not independently unit
/// tested because `OnceLock` fires exactly once per test binary — see
/// module docs for why precedence tests use [`load_env_from_path`] instead.
pub fn load_env_local_once() {
    static LOADED: OnceLock<()> = OnceLock::new();
    LOADED.get_or_init(|| {
        if let Ok(cwd) = std::env::current_dir()
            && let Some(path) = find_workspace_env_local(&cwd)
        {
            let _ = dotenvy::from_path(&path);
        }
    });
}

/// Read a single variable's value from a specific `.env.local` file **without**
/// mutating the process environment.
///
/// Why: the `config keys list` verb reports which tier (process env >
/// `.env.local` > secure store) currently supplies each provider's key. To
/// distinguish "set in the shell env" from "loaded from `.env.local`" honestly,
/// the lister must inspect the file directly rather than call
/// [`load_env_local_once`] (which would fold the file's values into the process
/// env and erase the distinction). This hermetic, path-injectable reader is that
/// inspection primitive.
/// What: parses `path` with `dotenvy`'s non-mutating iterator, skipping malformed
/// entries, and returns the first non-empty value bound to `var`, or `None` when
/// the file is unreadable, has no such key, or binds it to an empty value.
/// Test: `dotenv_tests::read_var_from_env_local_finds_value`,
/// `dotenv_tests::read_var_from_env_local_absent_is_none`.
pub fn read_var_from_env_local(path: &Path, var: &str) -> Option<String> {
    let iter = dotenvy::from_path_iter(path).ok()?;
    iter.flatten()
        .find(|(k, _)| k == var)
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty())
}

/// Value of `var` in the nearest ancestor `.env.local`, without mutating the
/// process environment.
///
/// Why: the production `.env.local` tier check for `config keys list` — it must
/// use the same cwd-upward search [`load_env_local_once`] uses so the reported
/// tier matches what resolution would actually pick, but observe the file
/// read-only.
/// What: searches upward from the current working directory via
/// [`find_workspace_env_local`] and delegates to [`read_var_from_env_local`];
/// `None` when there is no `.env.local` or it does not bind `var`.
/// Test: covered structurally via `read_var_from_env_local` (the cwd-search wrap
/// is not independently unit tested — it depends on the real cwd, exactly like
/// [`load_env_local_once`]).
pub fn env_local_value(var: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let path = find_workspace_env_local(&cwd)?;
    read_var_from_env_local(&path, var)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Why: the upward search must find a `.env.local` in a parent of the
    /// starting directory, not just the exact directory.
    /// Test: itself.
    #[test]
    fn finds_env_local_in_ancestor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let child = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(tmp.path().join(".env.local"), "X=1\n").unwrap();
        let found = find_workspace_env_local(&child).unwrap();
        assert_eq!(found, tmp.path().join(".env.local"));
    }

    /// Why: absent `.env.local` anywhere in the ancestor chain must return
    /// `None`, not error.
    /// Test: itself.
    #[test]
    fn absent_env_local_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(find_workspace_env_local(tmp.path()), None);
    }

    /// Why: issue #2474 — a `.env.local` sitting OUTSIDE the repo root (an
    /// ancestor beyond the `.git` boundary) must never be picked up, even
    /// though the old unbounded walk would have found it. This is the exact
    /// footgun scenario: an unrelated ancestor (or `$HOME`) carrying a stray
    /// `.env.local`.
    /// What: builds `outer/.env.local` and `outer/repo/.git/` (a directory
    /// boundary) and `outer/repo/nested/`, then asserts the search from
    /// `nested` stops at `repo` and never reaches `outer/.env.local`.
    /// Test: itself.
    #[test]
    fn walk_stops_at_git_dir_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".env.local"), "OUTER=1\n").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let nested = repo.join("nested").join("deeper");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_workspace_env_local(&nested), None);
    }

    /// Why: a linked git worktree's `.git` is a FILE (a `gitdir: …` pointer).
    /// When that pointer resolves to a root that is NOT an actual filesystem
    /// ancestor of the worktree (a disjoint layout — here `/elsewhere`, wholly
    /// unrelated to the tempdir tree), continuing to climb could reintroduce
    /// the original unbounded-walk footgun, so the fix conservatively falls
    /// back to treating the worktree's own root as the hard boundary.
    /// What: same shape as [`walk_stops_at_git_dir_boundary`] but `.git` is
    /// written as a file pointing somewhere disjoint from this tempdir tree.
    /// Test: itself.
    #[test]
    fn walk_stops_at_git_file_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".env.local"), "OUTER=1\n").unwrap();
        let repo = tmp.path().join("worktree");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        let nested = repo.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_workspace_env_local(&nested), None);
    }

    /// Why: the low-level pointer resolver must correctly extract the main
    /// checkout root from a well-formed `gitdir: …` pointer — the building
    /// block the walk's ancestor-continuation depends on.
    /// What: `<main>/.git/worktrees/wt` is the gitdir the pointer references;
    /// resolving it must yield `<main>` (the `.git` component's parent).
    /// Test: itself.
    #[test]
    fn resolves_main_checkout_root_from_worktree_pointer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_root = tmp.path().join("main-repo");
        let gitdir = main_root.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        let worktree_git_file = tmp.path().join("worktree-git-file");
        std::fs::write(
            &worktree_git_file,
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        assert_eq!(
            resolve_worktree_main_root(&worktree_git_file),
            Some(main_root)
        );
    }

    /// Why: an independent review's empirical repro against the first cut of
    /// this fix found it assumed the commondir is always literally named
    /// `.git` — false for this repo's own trusty-mpm worktree tooling, which
    /// uses a commondir named `.base`. The resolver must key off the
    /// structural `<commondir>/worktrees/<name>` shape, not the commondir's
    /// basename.
    /// What: `<trusty-tools>/.base/worktrees/session-x` is the gitdir the
    /// pointer references (mirroring the exact real-repo path shape);
    /// resolving it must yield `<trusty-tools>` (i.e. `.base`'s parent, not
    /// `.base` itself).
    /// Test: itself.
    #[test]
    fn resolves_main_checkout_root_with_non_dot_git_commondir_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_root = tmp.path().join("trusty-tools");
        let gitdir = main_root.join(".base").join("worktrees").join("session-x");
        std::fs::create_dir_all(&gitdir).unwrap();
        let worktree_git_file = tmp.path().join("worktree-git-file");
        std::fs::write(
            &worktree_git_file,
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        assert_eq!(
            resolve_worktree_main_root(&worktree_git_file),
            Some(main_root)
        );
    }

    /// Why: a `.git` pointer file with no `gitdir:` line (corrupt/foreign
    /// content) must resolve to `None`, not panic or misparse — the walk's
    /// conservative-fallback path depends on this.
    /// Test: itself.
    #[test]
    fn malformed_pointer_file_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("bogus-git-file");
        std::fs::write(&bogus, "not a gitdir pointer at all\n").unwrap();
        assert_eq!(resolve_worktree_main_root(&bogus), None);

        let missing = tmp.path().join("does-not-exist");
        assert_eq!(resolve_worktree_main_root(&missing), None);
    }

    /// Why: this is the exact scenario an independent review reproduced
    /// against the first cut of the #2474 fix — a linked worktree nested
    /// under a main checkout that itself has a `.git` DIRECTORY and holds
    /// the real, gitignored `.env.local`. The walk must resolve the
    /// worktree's `.git` pointer, recognise the main checkout root as a real
    /// ancestor, and keep climbing to find it — matching
    /// [`load_env_local_once`]'s documented "picked up regardless of which
    /// subdirectory a binary was launched from" contract.
    /// What: builds `main-repo/.git/` (directory) + `main-repo/.env.local`,
    /// then `main-repo/.claude/worktrees/wt/.git` (a pointer file resolving
    /// to `main-repo/.git/worktrees/wt`), then a deeply nested source
    /// directory inside the worktree (mirroring
    /// `crates/trusty-common/src/...`). The search from that nested directory
    /// must find `main-repo/.env.local`.
    /// Test: itself.
    #[test]
    fn worktree_pointer_reaches_env_local_in_main_checkout_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_root = tmp.path().join("main-repo");
        std::fs::create_dir_all(main_root.join(".git").join("worktrees").join("wt")).unwrap();
        std::fs::write(main_root.join(".env.local"), "REAL=1\n").unwrap();

        let worktree_root = main_root.join(".claude").join("worktrees").join("wt");
        std::fs::create_dir_all(&worktree_root).unwrap();
        let gitdir = main_root.join(".git").join("worktrees").join("wt");
        std::fs::write(
            worktree_root.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let nested = worktree_root
            .join("crates")
            .join("trusty-common")
            .join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_env_local(&nested).unwrap();
        assert_eq!(found, main_root.join(".env.local"));
    }

    /// Why: the exact real-repo shape an independent review reproduced — THIS
    /// repo's own trusty-mpm worktree tooling uses a commondir named `.base`
    /// (not `.git`), with the linked worktree itself living several levels
    /// BELOW `.base` (e.g. `<root>/.base/.claude/worktrees/<name>`). Unlike
    /// [`worktree_pointer_reaches_env_local_in_main_checkout_root`], `main_root`
    /// here has NO `.git` directory of its own — `.base` is the only git
    /// storage, matching the real layout where `.base` is a rename of what
    /// would otherwise be `.git`.
    /// What: same shape as the `.git`-named case, but the commondir path
    /// segment is `.base` and the worktree is nested under `.base` itself.
    /// The search from deep inside the worktree must still find
    /// `main_root/.env.local`.
    /// Test: itself.
    #[test]
    fn worktree_pointer_with_base_commondir_reaches_env_local_in_main_checkout_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_root = tmp.path().join("trusty-tools");
        std::fs::create_dir_all(main_root.join(".base").join("worktrees").join("session-x"))
            .unwrap();
        std::fs::write(main_root.join(".env.local"), "REAL=1\n").unwrap();

        let worktree_root = main_root
            .join(".base")
            .join(".claude")
            .join("worktrees")
            .join("session-x");
        std::fs::create_dir_all(&worktree_root).unwrap();
        let gitdir = main_root.join(".base").join("worktrees").join("session-x");
        std::fs::write(
            worktree_root.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let nested = worktree_root
            .join("crates")
            .join("trusty-common")
            .join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_env_local(&nested).unwrap();
        assert_eq!(found, main_root.join(".env.local"));
    }

    /// Why: synthetic tempdir trees alone don't prove the real `git worktree
    /// add` on-disk shape is handled — this drives the actual `git` binary to
    /// build a real repo + a real linked worktree (mirroring exactly how this
    /// repo's own sessions run: `.claude/worktrees/<name>` under the main
    /// checkout) and asserts the main checkout's gitignored `.env.local`
    /// (standing in for a real `FIREWORKS_API_KEY`, per issue #2510's
    /// context) is reachable from deep inside the worktree.
    /// What: `git init` + one commit in a temp "main" repo, write
    /// `.env.local` there, `git worktree add` a linked worktree under
    /// `main/.claude/worktrees/fixture`, then search from a nested directory
    /// inside the worktree.
    /// Test: itself.
    #[test]
    fn real_git_worktree_add_reaches_main_checkout_env_local() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_repo = tmp.path().join("main");
        std::fs::create_dir_all(&main_repo).unwrap();

        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&main_repo)
                .env("GIT_AUTHOR_NAME", "trusty-tools-test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "trusty-tools-test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .status()
                .expect("git must be on PATH to run this test");
            assert!(status.success(), "git {args:?} failed");
        };

        run(&["init", "-q", "-b", "main"]);
        std::fs::write(main_repo.join("README.md"), "fixture\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);
        // Stands in for a real gitignored credential (e.g. FIREWORKS_API_KEY,
        // issue #2510's context) that must never be committed and is never
        // present inside a worktree checkout itself.
        std::fs::write(
            main_repo.join(".env.local"),
            "FIREWORKS_API_KEY=real-secret-value\n", // pragma: allowlist secret
        )
        .unwrap();

        let worktree_path = main_repo.join(".claude").join("worktrees").join("fixture");
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "fixture-branch",
            worktree_path.to_str().unwrap(),
        ]);

        let nested = worktree_path.join("crates").join("trusty-common");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_env_local(&nested)
            .expect("main checkout .env.local must be reachable from a real linked worktree");
        assert_eq!(found, main_repo.join(".env.local"));
    }

    /// Why: the exact real-repo shape an independent review reproduced against
    /// the first cut of this fix, driven through the REAL `git` binary rather
    /// than a synthetic tempdir — `git --git-dir=<root>/.base` (this repo's
    /// own trusty-mpm worktree convention) instead of a vanilla `.git`. This
    /// proves [`resolve_worktree_main_root`]'s structural (not name-based)
    /// resolution against the actual on-disk format git writes for this
    /// exact layout.
    /// What: `git --git-dir=main/.base --work-tree=main init` (creating
    /// `.base` directly — no `.git` ever exists in `main`), one commit, a
    /// gitignored-style `.env.local` at `main/`, then `git worktree add`
    /// nested under `main/.base/.claude/worktrees/fixture` (mirroring
    /// `trusty-tools/.base/.claude/worktrees/<session>`), then search from a
    /// nested directory inside that worktree.
    /// Test: itself.
    #[test]
    fn real_git_worktree_add_with_renamed_base_commondir_reaches_env_local() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_repo = tmp.path().join("main");
        std::fs::create_dir_all(&main_repo).unwrap();
        let git_dir = main_repo.join(".base");

        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg(format!("--git-dir={}", git_dir.display()))
                .arg(format!("--work-tree={}", main_repo.display()))
                .args(args)
                .env("GIT_AUTHOR_NAME", "trusty-tools-test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "trusty-tools-test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .status()
                .expect("git must be on PATH to run this test");
            assert!(status.success(), "git {args:?} failed");
        };

        run(&["init", "-q", "-b", "main"]);
        assert!(
            !main_repo.join(".git").exists(),
            "this test's whole point is that NO `.git` exists — only `.base`"
        );
        std::fs::write(main_repo.join("README.md"), "fixture\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);
        std::fs::write(
            main_repo.join(".env.local"),
            "FIREWORKS_API_KEY=real-secret-value\n", // pragma: allowlist secret
        )
        .unwrap();

        let worktree_path = main_repo
            .join(".base")
            .join(".claude")
            .join("worktrees")
            .join("fixture");
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "fixture-branch",
            worktree_path.to_str().unwrap(),
        ]);

        let nested = worktree_path.join("crates").join("trusty-common");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_env_local(&nested).expect(
            "main checkout .env.local must be reachable from a real .base-commondir worktree",
        );
        assert_eq!(found, main_repo.join(".env.local"));
    }

    /// Why: the boundary directory itself is still a legitimate place for the
    /// intended "repo-root, gitignored" `.env.local` — the fix must not
    /// exclude the boundary directory's own file, only ancestors BEYOND it.
    /// What: `.env.local` lives in the same directory as `.git`; the search
    /// from a nested child must still find it.
    /// Test: itself.
    #[test]
    fn walk_finds_env_local_at_the_boundary_itself() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".env.local"), "X=1\n").unwrap();
        let nested = repo.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_env_local(&nested).unwrap();
        assert_eq!(found, repo.join(".env.local"));
    }

    /// Why: a workspace root without an initialised `.git` (e.g. an extracted
    /// tarball, or a crate checked out standalone) must still bound the walk
    /// via its workspace `Cargo.toml`, per the issue spec ("`.git` (or
    /// workspace `Cargo.toml`) marker").
    /// What: `outer/.env.local` sits beyond `outer/repo/Cargo.toml` (a
    /// `[workspace]` manifest, no `.git`); the search from a nested child must
    /// stop at `repo` and never reach `outer/.env.local`.
    /// Test: itself.
    #[test]
    fn workspace_cargo_toml_is_a_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".env.local"), "OUTER=1\n").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        let nested = repo.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_workspace_env_local(&nested), None);
    }

    /// Why: a plain (non-workspace) `Cargo.toml` — an ordinary crate manifest
    /// with no `[workspace]` table — must NOT be treated as a boundary; only a
    /// workspace root or a `.git` root should stop the walk.
    /// What: `repo/Cargo.toml` has a `[package]` table only; the walk must
    /// climb past it and still find `outer/.env.local`.
    /// Test: itself.
    #[test]
    fn plain_package_cargo_toml_is_not_a_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".env.local"), "OUTER=1\n").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let nested = repo.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_env_local(&nested).unwrap();
        assert_eq!(found, tmp.path().join(".env.local"));
    }

    /// Why: the `config keys list` tier check must read a var out of a
    /// `.env.local` file WITHOUT touching the process environment.
    /// Test: itself.
    #[test]
    fn read_var_from_env_local_finds_value() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".env.local");
        std::fs::write(&path, "OPENAI_API_KEY=from-dotenv\nEMPTY=\n").unwrap();
        assert_eq!(
            read_var_from_env_local(&path, "OPENAI_API_KEY"),
            Some("from-dotenv".to_string())
        );
        // The read is non-mutating: the process env is untouched.
        assert!(std::env::var("OPENAI_API_KEY").is_err());
    }

    /// Why: an absent key (or one bound to an empty value) must read back as
    /// `None`, not as a spurious empty-string "configured" signal.
    /// Test: itself.
    #[test]
    fn read_var_from_env_local_absent_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".env.local");
        std::fs::write(&path, "EMPTY=\n").unwrap();
        assert_eq!(read_var_from_env_local(&path, "EMPTY"), None);
        assert_eq!(read_var_from_env_local(&path, "MISSING"), None);
    }

    /// Why: `load_env_from_path` must set a var not already in the process
    /// environment.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn load_env_from_path_sets_new_var() {
        let var = "TRUSTY_TEST_DOTENV_NEW_VAR";
        // SAFETY: `#[serial(dotenv_credential_env)]` guarantees no other
        // test in this crate mutates process env concurrently with this one.
        unsafe {
            std::env::remove_var(var);
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env_path = tmp.path().join(".env.local");
        std::fs::write(&env_path, format!("{var}=from-dotenv\n")).unwrap();
        assert!(load_env_from_path(&env_path));
        assert_eq!(std::env::var(var).unwrap(), "from-dotenv");
        unsafe {
            std::env::remove_var(var);
        }
    }

    /// Why: `dotenvy` must never override a variable already set in the
    /// process environment — this is the mechanism that gives "process env
    /// beats `.env.local`" precedence for free.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn load_env_from_path_does_not_override_existing() {
        let var = "TRUSTY_TEST_DOTENV_EXISTING_VAR";
        // SAFETY: see `load_env_from_path_sets_new_var`.
        unsafe {
            std::env::set_var(var, "already-set");
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env_path = tmp.path().join(".env.local");
        std::fs::write(&env_path, format!("{var}=from-dotenv\n")).unwrap();
        assert!(load_env_from_path(&env_path));
        assert_eq!(std::env::var(var).unwrap(), "already-set");
        unsafe {
            std::env::remove_var(var);
        }
    }
}
