//! Tests for [`super`] — project-derived index identity (#4207).
//!
//! Why: isolated in a sibling file (declared via `#[path = ...]` from
//! `project_index_id.rs`, the pattern `search_index_tests.rs` already uses in
//! this crate) so the derivation module stays inside the 500-SLOC production
//! cap while the collision suite can be as exhaustive as the design demands.
//! The cases below are not generic coverage — each one is either a specific way
//! #4063's grouping-key-as-partitioning-key approach was proven to collide, or
//! a pinned drift case the migration slice will inherit.
//!
//! What: real synthetic git topologies (`git init`, `git clone`,
//! `git worktree add` in temp dirs) for the cases that depend on git behaviour,
//! and direct construction of [`ProjectIdentity`] for the cases that are pure.
//! Nothing here touches a live daemon, the index registry, or any
//! currently-registered index.
//!
//! **No test in this file may pass vacuously.** An earlier revision let six
//! git-topology tests `return` — reporting `ok` — whenever any git step failed,
//! including the two that carry the entire partitioning guarantee. A guard that
//! can silently not-run is not a guard. [`git`] therefore panics on a failed
//! spawn or a non-zero exit, so a runner with no git, or with a hostile
//! `safe.directory` / hook / signing configuration, goes RED rather than green.
//!
//! Test: this file.

use super::*;
use crate::github_path::GithubPath;

// ---------------------------------------------------------------------------
// Helpers — every one of these fails loudly; none skip.
// ---------------------------------------------------------------------------

/// Run a git subcommand in `dir`, panicking with full context on any failure.
///
/// Why: see the "no test may pass vacuously" note above. Returns stdout so
/// callers can assert on it.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "git {args:?} in {dir:?} could not spawn: {e}. These tests guard \
                 the #4207 partitioning key and must never pass without running."
            )
        });
    assert!(
        out.status.success(),
        "git {args:?} in {dir:?} exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Create `dir` and `git init` it with a deterministic, signing-free identity.
///
/// `commit.gpgsign=false` is forced per-repo so a developer's global signing
/// config cannot fail these tests for an unrelated reason — they fail loudly
/// now, so every avoidable environmental failure must be designed out rather
/// than skipped around.
fn init_empty_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create repo dir");
    git(dir, &["-c", "init.defaultBranch=main", "init"]);
    git(dir, &["config", "user.email", "t@t.test"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Write `name` into `dir` and commit it.
fn commit_file(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write file");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "commit"]);
}

/// Initialise a committed repo at `dir` whose `origin` points at `url`.
fn init_repo(dir: &Path, url: &str) {
    init_empty_repo(dir);
    commit_file(dir, "README.md", "hi");
    git(dir, &["remote", "add", "origin", url]);
}

/// A pure identity with every field supplied — no filesystem, no git.
fn identity(owner: &str, repo: &str, root: &str, operator: Option<&str>) -> ProjectIdentity {
    ProjectIdentity {
        origin: Some(RepoIdentity::GitHub(GithubPath {
            owner: owner.into(),
            repo: repo.into(),
        })),
        root: PathBuf::from(root),
        operator: operator.map(str::to_string),
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
    std::fs::create_dir_all(&upstream).expect("create upstream dir");
    git(&upstream, &["init", "--bare"]);
    let upstream_url = upstream.to_string_lossy().into_owned();

    let seed = tmp.path().join("seed");
    init_empty_repo(&seed);
    commit_file(&seed, "README.md", "hi");
    git(&seed, &["remote", "add", "origin", &upstream_url]);
    git(&seed, &["push", "origin", "HEAD:refs/heads/main"]);

    // Two real clones at different paths, both re-pointed at the same GitHub
    // origin — exactly the topology proven to collide on #4063.
    let a = tmp.path().join("widget");
    let b = tmp.path().join("widget-review");
    for dest in [&a, &b] {
        let dest_arg = dest.to_string_lossy().into_owned();
        git(tmp.path(), &["clone", &upstream_url, &dest_arg]);
        git(
            dest,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:acme/widget.git",
            ],
        );
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
    init_repo(&main, "git@github.com:acme/widget.git");

    let wt_a = tmp.path().join("wt-a");
    let wt_b = tmp.path().join("wt-b");
    for (dir, branch) in [(&wt_a, "feat-a"), (&wt_b, "feat-b")] {
        let dir_arg = dir.to_string_lossy().into_owned();
        git(&main, &["worktree", "add", "-b", branch, &dir_arg]);
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

/// Why: the operator is the third component of the #4207 identity triple. Two
/// checkouts that agree on origin and root but are operated by different
/// accounts must not share an index.
/// Test: itself.
#[test]
fn different_operators_derive_distinct_ids() {
    let one = identity("acme", "widget", "/srv/widget", Some("a@example.test"));
    let two = identity("acme", "widget", "/srv/widget", Some("b@example.test"));
    assert_ne!(one.index_id(), two.index_id());

    // An unresolved operator must also be distinct from any resolved one — the
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
        operator: None,
    };
    let b = ProjectIdentity {
        origin: None,
        root: PathBuf::from("/srv/beta/docs"),
        operator: None,
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
// Pinned DRIFT cases — one tree deriving different ids over time (PR #4262 HIGH)
//
// These are not bugs being fixed; they are known, accepted consequences of
// deriving identity partly from mutable git state, pinned here so the migration
// slice inherits a TRUE guarantee instead of a surprise. Each corresponds to a
// row in the `Known limitation` table on `ProjectIdentity::index_id`. If one of
// these ever starts asserting equality, the derivation changed and that table
// is stale.
// ---------------------------------------------------------------------------

/// Why: a fresh `git init` has no remote AND no commits, so `origin` is `None`;
/// the first commit gives `RepoIdentity` a root-commit hash to return. An index
/// registered in that window is orphaned by the first commit.
/// Test: itself.
#[test]
fn id_changes_when_first_commit_lands() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("nocommit");
    init_empty_repo(&repo);

    let before = ProjectIdentity::derive(&repo);
    assert_eq!(before.origin, None, "no commits, no remote ⇒ no origin");

    commit_file(&repo, "README.md", "hi");
    let after = ProjectIdentity::derive(&repo);
    assert!(
        matches!(after.origin, Some(RepoIdentity::ContentHash(_))),
        "first commit must yield a content-hash origin, got {:?}",
        after.origin
    );
    assert_ne!(
        before.index_id(),
        after.index_id(),
        "KNOWN DRIFT: the first commit re-derives the id for the same tree"
    );
}

/// Why: the everyday `git init` → work → `gh repo create` sequence. It moves
/// `origin` from `ContentHash` to `GitHub`, changing BOTH the label and the
/// digest — so an index registered before the remote exists is silently
/// orphaned the moment it is added, with no self-heal. This is the single most
/// important row of the drift table for the migration slice.
/// Test: itself.
#[test]
fn id_changes_when_origin_remote_is_added() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("nocommit");
    init_empty_repo(&repo);
    commit_file(&repo, "README.md", "hi");

    let before = ProjectIdentity::derive(&repo);
    assert!(matches!(before.origin, Some(RepoIdentity::ContentHash(_))));

    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:acme/widget.git"],
    );
    let after = ProjectIdentity::derive(&repo);
    assert!(matches!(after.origin, Some(RepoIdentity::GitHub(_))));

    assert_ne!(
        before.index_id(),
        after.index_id(),
        "KNOWN DRIFT: adding the origin remote re-derives the id"
    );
    // The label changes too, not merely the digest.
    assert!(before.index_id().starts_with("nocommit-"));
    assert!(after.index_id().starts_with("acme-widget-"));
}

/// Why: for a remoteless repo the origin is the FIRST root-commit sha, so a new
/// root commit (`git checkout --orphan`) moves it. Narrower than the two rows
/// above — it needs a remoteless repo — but pinned for completeness.
/// Test: itself.
#[test]
fn id_changes_on_orphan_root_commit() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("orphan");
    init_empty_repo(&repo);
    commit_file(&repo, "README.md", "hi");

    let before = ProjectIdentity::derive(&repo);
    git(&repo, &["checkout", "--orphan", "second-root"]);
    commit_file(&repo, "OTHER.md", "other");
    let after = ProjectIdentity::derive(&repo);

    assert_ne!(
        before.origin, after.origin,
        "precondition: the root commit must actually have moved"
    );
    assert_ne!(
        before.index_id(),
        after.index_id(),
        "KNOWN DRIFT: a new root commit re-derives the id for a remoteless repo"
    );
}

// ---------------------------------------------------------------------------
// Stability, degradation, and format
// ---------------------------------------------------------------------------

/// Why: an index id that changes between runs orphans the index it names. For an
/// UNCHANGED project the derivation must be reproducible — the drift tests above
/// enumerate exactly which changes are allowed to move it, and this pins that
/// nothing else does.
/// Test: itself.
#[test]
fn derivation_is_deterministic_across_calls() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_repo(&repo, "git@github.com:acme/widget.git");

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
/// hash, the field order, or `SCHEME_VERSION` into a test failure instead of a
/// silent mass orphaning. Update it ONLY together with `SCHEME_VERSION` and a
/// migration.
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

/// Why: two paths naming ONE content tree via a SYMLINK must derive one id —
/// otherwise a caller reaching the project through a symlinked path would create
/// a second index over identical content. Note the deliberately narrow scope:
/// `canonicalize` does NOT resolve macOS firmlinks or Linux bind mounts, which
/// [`ProjectIdentity::derive`] documents as a known limitation.
/// Test: itself.
#[cfg(unix)]
#[test]
fn symlinked_root_derives_same_id_as_real_root() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let real = tmp.path().join("repo");
    init_repo(&real, "git@github.com:acme/widget.git");

    let link = tmp.path().join("repo-link");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink");
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
        operator: None,
    };
    assert_eq!(ident.label(), FALLBACK_LABEL);
    assert!(ident.index_id().starts_with("project-"));

    let unicode = ProjectIdentity {
        origin: None,
        root: PathBuf::from("/srv/プロジェクト"),
        operator: None,
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
/// override, so the operator component must honour git's repo-local-over-global
/// precedence rather than reading only the global identity.
/// Test: itself.
#[test]
fn resolve_operator_identity_prefers_repo_local_git_identity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_repo(&repo, "git@github.com:acme/widget.git");

    git(&repo, &["config", "user.email", "local@example.test"]);
    assert_eq!(
        resolve_operator_identity(&repo),
        Some("local@example.test".to_string())
    );
}

/// Why: an earlier revision honoured a `TRUSTY_GH_USER` environment override,
/// which let two live callers on the SAME tree at the SAME instant derive
/// different ids purely from differing process environments (the trusty-mpm
/// daemon's launchd env vs a `tm` CLI's shell env) — re-creating the #1373
/// "callers silently diverge" failure this module exists to prevent. The
/// override was REMOVED rather than merely tested; this pins that it is gone, so
/// a future convenience re-adding an env read fails here instead of silently
/// re-partitioning one tree into two indexes.
///
/// Asserted against the source text rather than by setting the variable:
/// `std::env::set_var` is process-global (and `unsafe` in edition 2024), so a
/// test that set it would corrupt every other test running in parallel.
/// Test: itself.
#[test]
fn resolve_operator_identity_ignores_ambient_env_override() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_repo(&repo, "git@github.com:acme/widget.git");
    git(&repo, &["config", "user.email", "local@example.test"]);
    assert_eq!(
        resolve_operator_identity(&repo),
        Some("local@example.test".to_string()),
        "the git identity is the only operator source"
    );

    // The production module must contain no environment read at all — an env
    // lookup is exactly the non-hermetic branch that was removed.
    let src = include_str!("project_index_id.rs");
    let reads_env = src
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("*")
        })
        .any(|l| l.contains("env::var") || l.contains("env!("));
    assert!(
        !reads_env,
        "derivation must stay hermetic: no environment reads in project_index_id.rs"
    );
}
