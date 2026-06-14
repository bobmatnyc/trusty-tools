//! Tests for the GitHub REST client, repo resolver, and payload types.
//!
//! Loaded via `#[cfg(test)] #[path = "client_tests.rs"] mod tests;` in `client.rs`.

use std::path::PathBuf;

use crate::collect::errors::CollectError;
use crate::collect::github::repo_resolver::{
    extract_owner_repo_from_url, parse_slug, resolve_github_repos,
};
use crate::collect::github::types::{ApiPull, GitHubIssue, GitHubPrCommit, GitHubReview};
use crate::core::config::{GithubConfig, RepositoryConfig};

use super::{commit_shas_for_pull, GitHubClient};

fn gh(repo: Option<&str>, org: Option<&str>) -> GithubConfig {
    GithubConfig {
        token: None,
        org: org.map(str::to_string),
        orgs: vec![],
        repo: repo.map(str::to_string),
        fetch_prs: true,
        fetch_pr_reviews: true,
        review_fetch_concurrency: 1,
        ticket_regex: None,
    }
}

fn repo_cfg(path: &str, name: Option<&str>, org: Option<&str>) -> RepositoryConfig {
    RepositoryConfig {
        path: PathBuf::from(path),
        name: name.map(str::to_string),
        org: org.map(str::to_string),
        ..Default::default()
    }
}

/// Confirm that the wire shape returned by the GitHub Issues API
/// deserializes into `GitHubIssue` exactly.
///
/// Why: protects against silent schema drift if GitHub renames or
/// nests one of the fields we depend on.
/// What: parses a representative JSON document.
/// Test: assert that all six fields round-trip with expected values.
#[test]
fn github_issue_deserializes_full_payload() {
    let json = r#"{
        "number": 42,
        "title": "Crash on startup",
        "state": "open",
        "html_url": "https://github.com/o/r/issues/42",
        "labels": [
            {"name": "bug"},
            {"name": "high-priority"}
        ],
        "body": "Stack trace: ..."
    }"#;
    let issue: GitHubIssue = serde_json::from_str(json).expect("parses");
    assert_eq!(issue.number, 42);
    assert_eq!(issue.title, "Crash on startup");
    assert_eq!(issue.state, "open");
    assert_eq!(issue.html_url, "https://github.com/o/r/issues/42");
    assert_eq!(issue.labels.len(), 2);
    assert_eq!(issue.labels[0].name, "bug");
    assert_eq!(issue.labels[1].name, "high-priority");
    assert_eq!(issue.body.as_deref(), Some("Stack trace: ..."));
}

/// `body` and `labels` may be missing — GitHub omits empty arrays in
/// some response shapes. Confirm the deserializer tolerates that.
///
/// Why: serde defaults must apply, otherwise real API responses fail to parse.
/// What: parses a minimal JSON document missing the optional fields.
/// Test: assert defaults for `labels` (empty) and `body` (`None`).
#[test]
fn github_issue_tolerates_missing_optional_fields() {
    let json = r#"{
        "number": 7,
        "title": "Q",
        "state": "closed",
        "html_url": "https://github.com/o/r/issues/7"
    }"#;
    let issue: GitHubIssue = serde_json::from_str(json).expect("parses");
    assert_eq!(issue.number, 7);
    assert!(issue.labels.is_empty());
    assert!(issue.body.is_none());
}

/// Verify the wire shape of a PR review payload deserializes correctly.
///
/// Why: `submitted_at` may be `null` for pending reviews and `user`
/// may be absent for deleted accounts — both must tolerate absence.
/// What: parses a representative reviews JSON document.
/// Test: assert state, user.login, and optional fields parse as expected.
#[test]
fn github_review_deserializes() {
    let json = r#"{
        "id": 12345,
        "state": "APPROVED",
        "user": {"login": "octocat"},
        "submitted_at": "2024-01-01T00:00:00Z"
    }"#;
    let r: GitHubReview = serde_json::from_str(json).expect("parses");
    assert_eq!(r.id, 12345);
    assert_eq!(r.state, "APPROVED");
    assert_eq!(r.user.as_ref().map(|u| u.login.as_str()), Some("octocat"));
    assert_eq!(r.submitted_at.as_deref(), Some("2024-01-01T00:00:00Z"));

    // Missing optional fields tolerated.
    let pending = r#"{"id": 1, "state": "PENDING"}"#;
    let r2: GitHubReview = serde_json::from_str(pending).expect("parses pending");
    assert!(r2.user.is_none());
    assert!(r2.submitted_at.is_none());
}

/// Verify the wire shape of a PR commit payload deserializes correctly.
///
/// Why: PR commit responses nest the message and author under a `commit`
/// object — the flat git2 shape doesn't apply here.
/// What: parses a representative `/pulls/{n}/commits` element.
/// Test: assert sha, message, and author fields all extract.
#[test]
fn github_pr_commit_deserializes() {
    let json = r#"{
        "sha": "deadbeefcafebabe",
        "commit": {
            "message": "feat: do the thing",
            "author": {
                "name": "Ada Lovelace",
                "email": "ada@example.com",
                "date": "2024-01-01T00:00:00Z"
            }
        }
    }"#;
    let c: GitHubPrCommit = serde_json::from_str(json).expect("parses");
    assert_eq!(c.sha, "deadbeefcafebabe");
    assert_eq!(c.commit.message, "feat: do the thing");
    let author = c.commit.author.expect("author present");
    assert_eq!(author.name, "Ada Lovelace");
    assert_eq!(author.email, "ada@example.com");
    assert_eq!(author.date.as_deref(), Some("2024-01-01T00:00:00Z"));
}

// -----------------------------------------------------------------------
// Issue #87: multi-repo / org-wide resolution
// -----------------------------------------------------------------------

/// Why: `github.repo: owner/name` is the simplest case and must short-
/// circuit resolution to a single-entry list regardless of what's in
/// `repositories[]`.
/// What: passes a single slug, asserts a one-element vec.
/// Test: exact `(owner, repo)` parsed.
#[test]
fn resolve_github_repos_single_repo_mode() {
    let cfg = gh(Some("acme/widget"), None);
    let repos = resolve_github_repos(&cfg, &[]);
    assert_eq!(repos, vec![("acme".to_string(), "widget".to_string())]);
}

/// Why: when `github.repo` is unset, an `org`-only config must drive
/// resolution from `repositories[]` (path basename + `github.org`).
/// What: two repos with no explicit `org:` field, `github.org=acme`.
/// Test: both pairs returned with `acme` as owner.
#[test]
fn resolve_github_repos_org_mode_uses_path_basename() {
    let cfg = gh(None, Some("acme"));
    let repos = vec![
        repo_cfg("/tmp/widget", None, None),
        repo_cfg("/tmp/gadget", None, None),
    ];
    let resolved = resolve_github_repos(&cfg, &repos);
    assert_eq!(
        resolved,
        vec![
            ("acme".to_string(), "widget".to_string()),
            ("acme".to_string(), "gadget".to_string()),
        ]
    );
}

/// Why: per-repo `org:` should override `github.org` for that entry.
/// What: mix one repo with its own `org` and one without.
/// Test: first uses per-repo owner, second falls back to `github.org`.
#[test]
fn resolve_github_repos_per_repo_org_overrides() {
    let cfg = gh(None, Some("default-org"));
    let repos = vec![
        repo_cfg("/tmp/alpha", None, Some("specific-org")),
        repo_cfg("/tmp/beta", None, None),
    ];
    let resolved = resolve_github_repos(&cfg, &repos);
    assert_eq!(
        resolved,
        vec![
            ("specific-org".to_string(), "alpha".to_string()),
            ("default-org".to_string(), "beta".to_string()),
        ]
    );
}

/// Why: explicit `name:` on a repo entry must be preferred over the path
/// basename so renames and non-canonical directory layouts work.
/// What: repo with mismatched path and `name`.
/// Test: resolved name follows the explicit `name`.
#[test]
fn resolve_github_repos_uses_explicit_name() {
    let cfg = gh(None, Some("acme"));
    let repos = vec![repo_cfg(
        "/tmp/some-random-clone-dir",
        Some("real-repo-name"),
        None,
    )];
    let resolved = resolve_github_repos(&cfg, &repos);
    assert_eq!(
        resolved,
        vec![("acme".to_string(), "real-repo-name".to_string())]
    );
}

/// Why: with neither `github.repo` nor `github.org` (and no remote we
/// can read for these synthetic paths), resolution must yield an empty
/// vec so the caller can skip PR fetching gracefully.
/// What: empty github config + repos with no `org:` and unreadable paths.
/// Test: empty result.
#[test]
fn resolve_github_repos_returns_empty_when_unresolvable() {
    let cfg = gh(None, None);
    let repos = vec![repo_cfg("/tmp/no-such-clone", None, None)];
    let resolved = resolve_github_repos(&cfg, &repos);
    assert!(resolved.is_empty(), "got: {resolved:?}");
}

/// Why: with totally empty inputs, resolution must be a clean no-op.
/// What: no github config slugs, no repositories.
/// Test: empty result.
#[test]
fn resolve_github_repos_empty_inputs() {
    let cfg = gh(None, None);
    let resolved = resolve_github_repos(&cfg, &[]);
    assert!(resolved.is_empty());
}

/// Why: duplicate `(owner, repo)` pairs in `repositories[]` (e.g. same
/// clone listed twice) must dedupe so the fetcher doesn't double-pull.
/// What: two entries that resolve to the same owner/name.
/// Test: deduped to one element.
#[test]
fn resolve_github_repos_deduplicates() {
    let cfg = gh(None, Some("acme"));
    let repos = vec![
        repo_cfg("/clone-a/widget", None, None),
        repo_cfg("/clone-b/widget", None, None),
    ];
    let resolved = resolve_github_repos(&cfg, &repos);
    assert_eq!(resolved, vec![("acme".to_string(), "widget".to_string())]);
}

/// Why: the multi-repo constructor must validate non-empty input — an
/// empty list represents a programmer error from the orchestrator.
/// What: call `new_for_prs` with `vec![]`.
/// Test: returns `CollectError::Config`.
#[test]
fn new_for_prs_rejects_empty_repos() {
    let cfg = gh(None, None);
    match GitHubClient::new_for_prs(&cfg, vec![]) {
        Ok(_) => panic!("expected error for empty repos"),
        Err(CollectError::Config(msg)) => {
            assert!(msg.contains("at least one"), "unexpected msg: {msg}")
        }
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }
}

/// Why: `new_for_reviews` must build a working client without requiring
/// any dummy repo slugs; the previous workaround of passing
/// `("_dummy","_dummy")` was fragile and confusing.
/// What: call `new_for_reviews` and confirm the client builds successfully
/// and does not populate owner/repo/repos with dummy values.
/// Test: owner and repo are empty; repos vec is empty; no panic or error.
#[test]
fn new_for_reviews_builds_without_dummy_slugs() {
    let cfg = gh(None, None);
    let client = GitHubClient::new_for_reviews(&cfg).expect("client builds");
    assert!(
        client.owner.is_empty(),
        "owner should be empty for reviews-only client"
    );
    assert!(
        client.repo.is_empty(),
        "repo should be empty for reviews-only client"
    );
    assert!(
        client.repos.is_empty(),
        "repos should be empty for reviews-only client"
    );
}

/// Why: the multi-repo constructor must accept a populated list and
/// expose every entry on `repos`. The first entry doubles as the
/// "primary" repo for issue endpoints.
/// What: build a client with two repos and inspect the internal state.
/// Test: `repos.len() == 2`, primary owner/repo matches index 0.
#[test]
fn new_for_prs_stores_all_repos() {
    let cfg = gh(None, Some("acme"));
    let client = GitHubClient::new_for_prs(
        &cfg,
        vec![
            ("acme".into(), "alpha".into()),
            ("acme".into(), "beta".into()),
        ],
    )
    .expect("client builds");
    assert_eq!(client.repos.len(), 2);
    assert_eq!(client.owner, "acme");
    assert_eq!(client.repo, "alpha");
}

/// Why: the slug parser is a small but critical helper — bad slugs must
/// be rejected with a clear message rather than silently producing
/// `("", "repo")` or similar nonsense.
/// What: a handful of well- and ill-formed slugs.
/// Test: positives parse, negatives return `Config` errors.
#[test]
fn parse_slug_validates_input() {
    assert_eq!(
        parse_slug("owner/repo").unwrap(),
        ("owner".to_string(), "repo".to_string())
    );
    assert!(parse_slug("no-slash").is_err());
    assert!(parse_slug("/repo").is_err());
    assert!(parse_slug("owner/").is_err());
}

/// Why: GitHub remotes come in several URL flavors — the URL parser
/// must cover the common HTTPS and SSH forms and reject non-GitHub hosts.
/// What: probe each supported form and a couple of negative cases.
/// Test: each call returns the expected `(owner, repo)` or `None`.
#[test]
fn extract_owner_repo_from_url_handles_common_forms() {
    assert_eq!(
        extract_owner_repo_from_url("https://github.com/acme/widget.git"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("https://github.com/acme/widget"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("git@github.com:acme/widget.git"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("ssh://git@github.com/acme/widget.git"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("https://user@github.com/acme/widget"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    // Non-GitHub hosts: unsupported.
    assert!(extract_owner_repo_from_url("https://gitlab.com/acme/widget").is_none());
    assert!(extract_owner_repo_from_url("nonsense").is_none());
}

/// Confirm `commit_shas_for_pull` gates the merge SHA on `merged_at`.
///
/// Why: issue #101 — GitHub populates `merge_commit_sha` even for open
/// or closed-without-merge PRs (a `refs/pull/N/merge` test merge that
/// exists on no branch), which would write a non-joinable value into
/// `pull_requests.commit_shas`. Only merged PRs carry a joinable SHA.
/// What: maps `ApiPull` payloads through `commit_shas_for_pull`.
/// Test: non-merged PR with a populated SHA yields `"[]"`; a merged PR
/// with a SHA yields `r#"["some-sha"]"#`.
#[test]
fn commit_shas_gated_on_merged_at() {
    // Non-merged PR with a populated (test-merge) SHA -> empty array.
    let json = r#"{
        "number": 101,
        "title": "Open PR",
        "user": {"login": "octocat"},
        "state": "open",
        "created_at": "2024-01-15T10:30:00Z",
        "merged_at": null,
        "merge_commit_sha": "some-sha"
    }"#;
    let p: ApiPull = serde_json::from_str(json).expect("parses");
    assert!(p.merge_commit_sha.is_some());
    assert!(p.merged_at.is_none());
    assert_eq!(
        commit_shas_for_pull(&p).expect("encodes"),
        "[]",
        "non-merged PR with a populated SHA must not emit commit_shas",
    );

    // Closed-without-merge PR with a populated SHA -> empty array.
    let json = r#"{
        "number": 102,
        "title": "Closed-no-merge PR",
        "user": {"login": "octocat"},
        "state": "closed",
        "created_at": "2024-01-15T10:30:00Z",
        "merged_at": null,
        "merge_commit_sha": "some-sha"
    }"#;
    let p: ApiPull = serde_json::from_str(json).expect("parses");
    assert_eq!(
        commit_shas_for_pull(&p).expect("encodes"),
        "[]",
        "closed-without-merge PR must not emit commit_shas",
    );

    // Merged PR with a populated SHA -> joinable single-element array.
    let json = r#"{
        "number": 103,
        "title": "Merged PR",
        "user": {"login": "octocat"},
        "state": "closed",
        "created_at": "2024-01-15T10:30:00Z",
        "merged_at": "2024-01-16T12:00:00Z",
        "merge_commit_sha": "some-sha"
    }"#;
    let p: ApiPull = serde_json::from_str(json).expect("parses");
    assert!(p.merged_at.is_some());
    assert_eq!(
        commit_shas_for_pull(&p).expect("encodes"),
        r#"["some-sha"]"#,
        "merged PR with a SHA should emit a joinable commit_shas array",
    );

    // Merged PR with no SHA at all -> still empty array.
    let json = r#"{
        "number": 104,
        "title": "Merged PR missing SHA",
        "user": {"login": "octocat"},
        "state": "closed",
        "created_at": "2024-01-15T10:30:00Z",
        "merged_at": "2024-01-16T12:00:00Z",
        "merge_commit_sha": null
    }"#;
    let p: ApiPull = serde_json::from_str(json).expect("parses");
    assert_eq!(
        commit_shas_for_pull(&p).expect("encodes"),
        "[]",
        "merged PR without a SHA yields the empty array",
    );
}
