//! Tests for [`super`] — project-derived index identity (#4207).
//!
//! Why: isolated in a sibling file (declared via `#[path = ...]` from
//! `project_index_id.rs`, the pattern `search_index_tests.rs` already uses in
//! this crate) so the derivation module stays inside the 500-SLOC production
//! cap while the collision suite can be as exhaustive as the design demands.
//! The cases below are not generic coverage — each one is a specific way
//! #4063's grouping-key-as-partitioning-key approach was proven to collide.
//!
//! What: real synthetic git topologies (`git init`, `git clone`,
//! `git worktree add` in temp dirs) for the cases that depend on git
//! behaviour, and direct construction of [`ProjectIdentity`] for the cases
//! that are pure. Nothing here touches a live daemon, the index registry, or
//! any currently-registered index.
//!
//! Test: this file.

use super::*;
use crate::github_path::GithubPath;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run a git subcommand in `dir`, reporting whether it succeeded.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Initialise a committed git repo at `dir` with `origin` pointing at `url`.
///
/// Returns `false` when git is unusable on this runner, so callers can skip
/// cleanly rather than fail (matching `repo_identity`'s existing convention).
fn init_repo(dir: &Path, url: &str) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    if !git_ok(dir, &["-c", "init.defaultBranch=main", "init"]) {
        return false;
    }
    let _ = git_ok(dir, &["config", "user.email", "t@t.test"]);
    let _ = git_ok(dir, &["config", "user.name", "t"]);
    if std::fs::write(dir.join("README.md"), "hi").is_err() {
        return false;
    }
    let _ = git_ok(dir, &["add", "."]);
    if !git_ok(dir, &["commit", "-m", "init"]) {
        return false;
    }
    git_ok(dir, &["remote", "add", "origin", url])
}

/// A pure identity with every field supplied — no filesystem, no git.
fn identity(owner: &str, repo: &str, root: &str, gh_user: Option<&str>) -> ProjectIdentity {
    ProjectIdentity {
        origin: Some(RepoIdentity::GitHub(GithubPath {
            owner: owner.into(),
            repo: repo.into(),
        })),
        root: PathBuf::from(root),
        gh_user: gh_user.map(str::to_string),
    }
}

// ---------------------------------------------------------------------------
// The collision cases that killed #4063
// ---------------------------------------------------------------------------

/// Why: THE case #4063 never fixed. Two independent clones of one repo are each
/// a main working tree, so no worktree suffix applies and the origin-derived
/// grouping key is identical — yet they are two different content trees, on
/// possibly different branches. Under a one-`root_path`-per-id registry, sharing
/// an id means the second clone's registration silently degrades to a *find*
/// against the first clone's root and every subsequent search, reindex, and
/// incremental update targets the wrong tree. This test asserts both halves:
/// the grouping key really does collide (so the test is exercising the real
/// hazard, not a strawman) and the derived index ids nonetheless do not.
/// Test: itself.
#[test]
fn sibling_clones_of_same_repo_derive_distinct_ids() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let upstream = tmp.path().join("origin.git");
    if std::fs::create_dir_all(&upstream).is_err() || !git_ok(&upstream, &["init", "--bare"]) {
        return; // no usable git on this runner
    }
    let upstream_url = upstream.to_string_lossy().into_owned();
    let seed = tmp.path().join("seed");
    if !init_repo(&seed, &upstream_url) {
        return;
    }
    if !git_ok(&seed, &["push", "origin", "HEAD:refs/heads/main"]) {
        return;
    }

    // Two real clones at different paths, both re-pointed at the same GitHub
    // origin — exactly the topology proven to collide on #4063.
    let a = tmp.path().join("widget");
    let b = tmp.path().join("widget-review");
    for dest in [&a, &b] {
        let dest_arg = dest.to_string_lossy().into_owned();
        let ok = Command::new("git")
            .args(["clone", &upstream_url, &dest_arg])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return;
        }
        assert!(git_ok(
            dest,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:acme/widget.git"
            ]
        ));
    }

    // The grouping key DOES collide — this is what made #4063's approach unsafe.
    let group_a = RepoIdentity::derive(&a).map(|r| r.canonical());
    let group_b = RepoIdentity::derive(&b).map(|r| r.canonical());
    assert_eq!(
        group_a, group_b,
        "precondition: sibling clones must share the repo-level grouping key"
    );
    assert_eq!(group_a, Some("acme/widget".to_string()));

    // The partitioning key does NOT.
    let id_a = derive_project_index_id(&a);
    let id_b = derive_project_index_id(&b);
    assert_ne!(
        id_a, id_b,
        "sibling clones are distinct content trees and must not share an index id"
    );
    // Both still carry the readable repo label.
    assert!(id_a.starts_with("acme-widget-"), "unexpected id: {id_a}");
    assert!(id_b.starts_with("acme-widget-"), "unexpected id: {id_b}");
}

/// Why: the second collision #4063 hit. `git config remote.origin.url` read from
/// a linked worktree transparently returns the SHARED repo config, so every
/// worktree of one repo derives the same origin. Under the 1.3.0 one-worktree-
/// per-writing-agent model this is the common case, not an edge case.
/// Test: itself.
#[test]
fn linked_worktrees_of_same_repo_derive_distinct_ids() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let main = tmp.path().join("repo");
    if !init_repo(&main, "git@github.com:acme/widget.git") {
        return;
    }
    let wt_a = tmp.path().join("wt-a");
    let wt_b = tmp.path().join("wt-b");
    for (dir, branch) in [(&wt_a, "feat-a"), (&wt_b, "feat-b")] {
        let dir_arg = dir.to_string_lossy().into_owned();
        if !git_ok(&main, &["worktree", "add", "-b", branch, &dir_arg]) {
            return;
        }
    }

    // Precondition: all three facets share the grouping key.
    let group: Vec<_> = [&main, &wt_a, &wt_b]
        .into_iter()
        .map(|d| RepoIdentity::derive(d).map(|r| r.canonical()))
        .collect();
    assert_eq!(group[0], group[1]);
    assert_eq!(group[1], group[2]);

    let ids: Vec<String> = [&main, &wt_a, &wt_b]
        .into_iter()
        .map(|d| derive_project_index_id(d))
        .collect();
    assert_ne!(ids[0], ids[1], "main checkout vs worktree A: {ids:?}");
    assert_ne!(ids[0], ids[2], "main checkout vs worktree B: {ids:?}");
    assert_ne!(ids[1], ids[2], "worktree A vs worktree B: {ids:?}");
}

/// Why: the account is the third component of the #4207 identity triple. Two
/// checkouts that agree on origin and root but are operated by different GitHub
/// accounts must not share an index.
/// Test: itself.
#[test]
fn different_gh_users_derive_distinct_ids() {
    let one = identity("acme", "widget", "/srv/widget", Some("a@example.test"));
    let two = identity("acme", "widget", "/srv/widget", Some("b@example.test"));
    assert_ne!(one.index_id(), two.index_id());

    // An unresolved account must also be distinct from any resolved one — the
    // presence tag in the digest preimage is what guarantees this.
    let none = identity("acme", "widget", "/srv/widget", None);
    assert_ne!(none.index_id(), one.index_id());
    assert_ne!(none.index_id(), two.index_id());
}

/// Why: the original defect this epic exists to fix — the bare-basename id
/// collides for any two unrelated projects whose directories share a name
/// (`docs`, `web`, `api` are the everyday cases).
/// Test: itself.
#[test]
fn unrelated_projects_sharing_a_basename_derive_distinct_ids() {
    let a = ProjectIdentity {
        origin: None,
        root: PathBuf::from("/srv/alpha/docs"),
        gh_user: None,
    };
    let b = ProjectIdentity {
        origin: None,
        root: PathBuf::from("/srv/beta/docs"),
        gh_user: None,
    };
    assert_ne!(a.index_id(), b.index_id());
    // Both keep the readable label; only the digest separates them.
    assert!(a.index_id().starts_with("docs-"));
    assert!(b.index_id().starts_with("docs-"));
}

/// Why: #4063 shipped a bare `format!("{owner}-{repo}")` join, so `foo-bar`/`baz`
/// and `foo`/`bar-baz` produced one id for two different repos. The label here is
/// deliberately allowed to collide; the length-framed digest preimage is what
/// must not.
/// Test: itself.
#[test]
fn label_ambiguity_does_not_collide() {
    let a = identity("foo-bar", "baz", "/srv/x", None);
    let b = identity("foo", "bar-baz", "/srv/x", None);
    assert_eq!(a.label(), b.label(), "precondition: labels collide");
    assert_ne!(a.index_id(), b.index_id(), "digests must not collide");
}

// ---------------------------------------------------------------------------
// Stability, degradation, and format
// ---------------------------------------------------------------------------

/// Why: an index id that changes between runs orphans the index it names. The
/// derivation must be reproducible for an unchanged project.
/// Test: itself.
#[test]
fn derivation_is_deterministic_across_calls() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    if !init_repo(&repo, "git@github.com:acme/widget.git") {
        return;
    }
    let first = derive_project_index_id(&repo);
    let second = derive_project_index_id(&repo);
    assert_eq!(first, second);
    // A nested subdirectory resolves to the same project root, hence same id.
    let nested = repo.join("src/deep");
    std::fs::create_dir_all(&nested).expect("nested dir");
    assert_eq!(derive_project_index_id(&nested), first);
}

/// Why: the id rule is a stored contract — every already-registered index is
/// named by it. A golden value turns an accidental change to the framing, the
/// hash, or the field order into a test failure instead of a silent mass
/// orphaning. Update it ONLY together with `SCHEME_VERSION` and a migration.
/// Test: itself.
#[test]
fn index_id_is_pinned_for_a_known_input() {
    let pinned = identity(
        "bobmatnyc",
        "trusty-tools",
        "/Users/me/code/trusty-tools",
        Some("bob@example.test"),
    );
    assert_eq!(pinned.index_id(), "bobmatnyc-trusty-tools-f3ef22158eced5cb");
}

/// Why: a directory with no git origin at all (scratch dirs, unpacked archives)
/// must still receive a stable, well-formed id rather than panicking or
/// degrading to the empty string.
/// Test: itself.
#[test]
fn directory_without_git_origin_derives_stable_id() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let plain = tmp.path().join("loose-files");
    std::fs::create_dir_all(&plain).expect("dir");

    let ident = ProjectIdentity::derive(&plain);
    assert_eq!(ident.origin, None, "no git repo means no grouping key");

    let id = ident.index_id();
    assert!(id.starts_with("loose-files-"), "unexpected id: {id}");
    assert_eq!(derive_project_index_id(&plain), id, "must be reproducible");
}

/// Why: two paths naming ONE content tree (a symlink and its target) must derive
/// one id — otherwise a caller reaching the project through a symlinked path
/// would create a second index over identical content.
/// Test: itself.
#[cfg(unix)]
#[test]
fn symlinked_root_derives_same_id_as_real_root() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let real = tmp.path().join("repo");
    if !init_repo(&real, "git@github.com:acme/widget.git") {
        return;
    }
    let link = tmp.path().join("repo-link");
    if std::os::unix::fs::symlink(&real, &link).is_err() {
        return;
    }
    assert_eq!(
        derive_project_index_id(&link),
        derive_project_index_id(&real)
    );
}

/// Why: the id is interpolated into an axum `/indexes/{id}` route and into
/// filenames; a slash, space, or uppercase byte would break routing or produce
/// two ids that differ only by case.
/// Test: itself.
#[test]
fn index_id_is_a_single_url_safe_segment() {
    let id = identity("Acme Corp", "My_Widget!", "/srv/x", Some("a@b.test")).index_id();
    assert!(!id.is_empty());
    assert!(
        id.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "id must be lowercase alnum + hyphen: {id}"
    );
    assert!(
        !id.starts_with('-') && !id.ends_with('-'),
        "bad edges: {id}"
    );
    assert!(
        id.starts_with("acme-corp-my-widget-"),
        "unexpected id: {id}"
    );
}

/// Why: a project that slugifies to nothing (a filesystem root, or a purely
/// non-ASCII directory name) must not yield an id that begins with the digest
/// separator or is otherwise malformed.
/// Test: itself.
#[test]
fn empty_label_falls_back_to_placeholder() {
    let ident = ProjectIdentity {
        origin: None,
        root: PathBuf::from("/"),
        gh_user: None,
    };
    assert_eq!(ident.label(), FALLBACK_LABEL);
    assert!(ident.index_id().starts_with("project-"));

    let unicode = ProjectIdentity {
        origin: None,
        root: PathBuf::from("/srv/プロジェクト"),
        gh_user: None,
    };
    assert!(unicode.index_id().starts_with("project-"));
    // Distinct roots still partition even when both fall back to the label.
    assert_ne!(ident.index_id(), unicode.index_id());
}

/// Why: a very long owner/repo pair must not produce an unbounded id; the label
/// truncates while the digest keeps uniqueness intact.
/// Test: itself.
#[test]
fn long_label_is_truncated_without_losing_uniqueness() {
    let long = "x".repeat(200);
    let a = identity(&long, "repo", "/srv/a", None);
    let b = identity(&long, "repo", "/srv/b", None);
    let (id_a, id_b) = (a.index_id(), b.index_id());
    assert_eq!(a.label().chars().count(), MAX_LABEL_LEN);
    assert_ne!(id_a, id_b);
    assert_eq!(id_a.len(), MAX_LABEL_LEN + 1 + 16);
}

/// Why: the digest is the whole uniqueness guarantee; identical inputs must
/// produce an identical value within and across processes.
/// Test: itself.
#[test]
fn digest_is_stable_for_identical_inputs() {
    let a = identity("acme", "widget", "/srv/x", Some("a@b.test"));
    let b = identity("acme", "widget", "/srv/x", Some("a@b.test"));
    assert_eq!(a.digest(), b.digest());
    assert_eq!(a.index_id(), b.index_id());
    // fnv1a_64 itself: same bytes in, same value out; different bytes, different.
    assert_eq!(fnv1a_64(b"abc"), fnv1a_64(b"abc"));
    assert_ne!(fnv1a_64(b"abc"), fnv1a_64(b"abd"));
}

/// Why: per-account checkouts are configured with a repo-local `user.email`
/// override, so the account component must honour git's repo-local-over-global
/// precedence rather than reading only the global identity.
/// Test: itself.
#[test]
fn resolve_gh_user_reads_repo_local_git_identity() {
    if std::env::var("TRUSTY_GH_USER").is_ok() {
        return; // an explicit override is in effect; the git path is shadowed
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    if !init_repo(&repo, "git@github.com:acme/widget.git") {
        return;
    }
    assert!(git_ok(
        &repo,
        &["config", "user.email", "local@example.test"]
    ));
    assert_eq!(
        resolve_gh_user(&repo),
        Some("local@example.test".to_string())
    );
}
