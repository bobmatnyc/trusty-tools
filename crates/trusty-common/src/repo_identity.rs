//! Canonical repository identity — the join key trusty-search uses to relate
//! the multiple physical facets (live checkout, `.base` clone, session
//! worktrees) of the SAME repo (DOC-37, issue #2611).
//!
//! Why: trusty-search keys every index on a bare path basename
//! ([`crate::index_id::derive_index_id`]), so a live checkout, its tm-managed
//! `.base` clone, and each session worktree all register as unrelated indexes.
//! There is no way to answer "which of these indexes are the same repo?".
//! [`RepoIdentity`] supplies that missing join key: a canonical `owner/repo`
//! derived from the git origin remote (via the already-tested
//! [`crate::github_path::derive_github_path`], which transparently follows a
//! worktree's `gitdir:` pointer), with a content-hash fallback (the repo's
//! root-commit SHA) for repos that have no remote. It lives in `trusty-common`
//! because both trusty-search (index registration) and, later, trusty-mpm
//! (session-launch registration) must derive the identical value or the two
//! sides of the join silently diverge.
//!
//! What: [`RepoIdentity`] is a two-variant enum — `GitHub(owner/repo)` or
//! `ContentHash(sha)` — with a stable, reversible canonical string form
//! ([`RepoIdentity::canonical`] / [`RepoIdentity::parse`]) used as the stored
//! value in `indexes.toml` and the `?repo_identity=` daemon filter argument.
//! [`RepoIdentity::derive`] is the only I/O entry point (two best-effort git
//! shell-outs). Pure functions, no global state.
//!
//! Test: `cargo test -p trusty-common -- repo_identity::tests` covers canonical
//! round-tripping, both derive variants (github remote + remoteless content
//! hash via a real temp repo), and the outside-a-repo `None` fallback.

use std::path::Path;
use std::process::Command;

use crate::github_path::{self, GithubPath};

/// Scheme prefix marking the content-hash canonical form (`content:<sha>`).
///
/// Why: the two variants must be distinguishable in a single flat string (the
/// TOML field / query argument). A GitHub identity always contains a `/` and
/// never this prefix; a content hash never contains a `/`, so the prefix keeps
/// the two namespaces disjoint and the parse unambiguous.
/// What: the literal `"content:"`.
const CONTENT_HASH_PREFIX: &str = "content:";

/// Canonical identity of a repository, independent of its filesystem path.
///
/// Why: the basename-keyed `index_id` cannot tell that `.../repo`,
/// `.../repo/.base`, and `.../.base/.worktrees/<uuid>` are the same project.
/// A path-independent identity derived from git metadata can, giving
/// trusty-search a stable key to group and filter indexes by repo (DOC-37).
/// What: `GitHub` carries the slugified `{owner, repo}` from the origin remote;
/// `ContentHash` carries a git root-commit SHA for remoteless repos. Both map
/// to/from a single canonical string via [`Self::canonical`] / [`Self::parse`].
/// Test: `derive_uses_github_remote`, `derive_falls_back_to_content_hash`,
/// `canonical_round_trips`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoIdentity {
    /// `owner/repo` derived from the git origin remote URL.
    GitHub(GithubPath),
    /// Root-commit SHA fallback for a repo with no origin remote.
    ContentHash(String),
}

impl RepoIdentity {
    /// Derive the canonical identity of the repository containing `dir`.
    ///
    /// Why: index registration (and, later, tm session launch) needs one rule
    /// for "what repo is this?" that both sides compute identically. Preferring
    /// the origin remote means every worktree/clone of the same repo resolves to
    /// the same `owner/repo`; the content-hash fallback keeps remoteless local
    /// repos still groupable (all facets share a root commit).
    /// What: tries [`github_path::derive_github_path`] first (git
    /// `remote.origin.url`, worktree-`gitdir:`-aware); on no remote, falls back
    /// to the repo's first root-commit SHA
    /// (`git rev-list --max-parents=0 HEAD`, first line). Returns `None` only
    /// when `dir` is not a git repo, git is unavailable, or the repo has neither
    /// a remote nor any commit. Best-effort, no network.
    /// Test: `derive_uses_github_remote`, `derive_falls_back_to_content_hash`,
    /// `derive_none_outside_repo`.
    pub fn derive(dir: &Path) -> Option<RepoIdentity> {
        if let Some(gp) = github_path::derive_github_path(dir) {
            return Some(RepoIdentity::GitHub(gp));
        }
        root_commit_sha(dir).map(RepoIdentity::ContentHash)
    }

    /// Render the identity as its stable canonical string.
    ///
    /// Why: the identity is stored in `indexes.toml` and passed as the
    /// `?repo_identity=` filter; it needs one textual form that reverses cleanly
    /// via [`Self::parse`] and reads well in ops output.
    /// What: `GitHub` → `"<owner>/<repo>"`; `ContentHash` → `"content:<sha>"`.
    /// Test: `canonical_round_trips`.
    pub fn canonical(&self) -> String {
        match self {
            RepoIdentity::GitHub(gp) => gp.rel_path(),
            RepoIdentity::ContentHash(sha) => format!("{CONTENT_HASH_PREFIX}{sha}"),
        }
    }

    /// Parse a canonical string (from [`Self::canonical`]) back into a
    /// [`RepoIdentity`].
    ///
    /// Why: the daemon filter and any stored value must be comparable in
    /// canonical form; parsing then re-emitting `canonical()` normalises casing
    /// and slugging so `Owner/Repo` and `owner/repo` match the same indexes.
    /// What: a `content:` prefix (non-empty remainder) → `ContentHash`;
    /// otherwise the input is treated as an `owner/repo` path via
    /// [`github_path::parse_owner_repo`] → `GitHub`. Returns `None` for empty /
    /// unparseable input.
    /// Test: `canonical_round_trips`, `parse_normalises_case`,
    /// `parse_empty_is_none`.
    pub fn parse(s: &str) -> Option<RepoIdentity> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix(CONTENT_HASH_PREFIX) {
            let sha = rest.trim();
            return (!sha.is_empty()).then(|| RepoIdentity::ContentHash(sha.to_string()));
        }
        github_path::parse_owner_repo(trimmed).map(RepoIdentity::GitHub)
    }
}

impl std::fmt::Display for RepoIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Read the repository's first root-commit SHA (content-hash fallback).
///
/// Why: a repo with no origin remote still needs a stable, path-independent
/// identity so its facets group together; the root commit is shared by every
/// clone/worktree of the same history.
/// What: runs `git -C <dir> rev-list --max-parents=0 HEAD` and returns the
/// first line (a repo can have multiple root commits after a merge of unrelated
/// histories; the first is deterministic enough for grouping). `None` when git
/// fails or the repo has no commits.
/// Test: covered via `derive_falls_back_to_content_hash`.
fn root_commit_sha(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the canonical string is the on-disk / on-wire contract; both
    /// variants must survive a `canonical()` → `parse()` round-trip identically.
    /// Test: itself.
    #[test]
    fn canonical_round_trips() {
        let gh = RepoIdentity::GitHub(GithubPath {
            owner: "bobmatnyc".into(),
            repo: "trusty-tools".into(),
        });
        assert_eq!(gh.canonical(), "bobmatnyc/trusty-tools");
        assert_eq!(RepoIdentity::parse(&gh.canonical()), Some(gh));

        let ch = RepoIdentity::ContentHash("abc123def".into());
        assert_eq!(ch.canonical(), "content:abc123def");
        assert_eq!(RepoIdentity::parse(&ch.canonical()), Some(ch));
    }

    /// Why: the filter must match regardless of the caller's casing/punctuation,
    /// so parse has to normalise to the same slug derive produces.
    /// Test: itself.
    #[test]
    fn parse_normalises_case() {
        assert_eq!(
            RepoIdentity::parse("BobMatNyc/Trusty_Tools").map(|r| r.canonical()),
            Some("bobmatnyc/trusty-tools".to_string())
        );
    }

    /// Why: empty / garbage input has no identity and must not panic.
    /// Test: itself.
    #[test]
    fn parse_empty_is_none() {
        assert_eq!(RepoIdentity::parse(""), None);
        assert_eq!(RepoIdentity::parse("   "), None);
        assert_eq!(RepoIdentity::parse("content:"), None);
        assert_eq!(RepoIdentity::parse("content:   "), None);
    }

    /// Why: the primary path — a repo with an origin remote must derive its
    /// GitHub identity (worktree-aware via the underlying git shell-out).
    /// Test: itself (skips cleanly if git is unavailable on the runner).
    #[test]
    fn derive_uses_github_remote() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        if !git_ok(dir, &["init"]) {
            return; // no usable git on this runner
        }
        let _ = git_ok(
            dir,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ],
        );
        assert_eq!(
            RepoIdentity::derive(dir).map(|r| r.canonical()),
            Some("bobmatnyc/trusty-tools".to_string())
        );
    }

    /// Why: the fallback path — a repo with NO remote must still yield a stable
    /// content-hash identity from its root commit, so remoteless repos group.
    /// Test: itself (skips cleanly if git is unavailable).
    #[test]
    fn derive_falls_back_to_content_hash() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        if !git_ok(dir, &["init"]) {
            return;
        }
        // Identity requires a commit for the content-hash fallback.
        let _ = git_ok(dir, &["config", "user.email", "t@t.test"]);
        let _ = git_ok(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        let _ = git_ok(dir, &["add", "."]);
        if !git_ok(dir, &["commit", "-m", "init"]) {
            return; // commit failed (e.g. no identity configurable) — skip
        }
        match RepoIdentity::derive(dir) {
            Some(RepoIdentity::ContentHash(sha)) => {
                assert!(!sha.is_empty(), "root-commit sha must be non-empty");
                assert!(!sha.contains('/'), "content hash must have no slash");
            }
            other => panic!("expected ContentHash fallback, got {other:?}"),
        }
    }

    /// Why: a non-repo directory has no identity and must yield `None`, not
    /// panic, so callers fall back cleanly (index keeps working without one).
    /// Test: itself.
    #[test]
    fn derive_none_outside_repo() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        assert_eq!(RepoIdentity::derive(tmp.path()), None);
    }

    /// Run a git subcommand in `dir`, returning whether it succeeded.
    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
