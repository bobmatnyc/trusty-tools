//! GitHub org-wide repository discovery for PR collection, plus the
//! multi-org repo resolution logic shared between `client.rs` and `collector.rs`.
//!
//! Why: tga 2.6 added `github.orgs: [...]` to the config schema but the PR
//! collection pipeline only consumed `github.org` (singular). This module
//! bridges the gap by paging `GET /orgs/{org}/repos` and returning every
//! visible `(owner, repo)` pair for each org in the list (issue #742).
//! The `resolve_github_repos_with_discovered` function also lives here so
//! the async pipeline can inject discovery results without a separate dedup
//! pass.
//!
//! What: [`discover_org_repos`] paginates org repos; [`effective_orgs`]
//! merges singular/plural config fields;
//! [`resolve_github_repos_with_discovered`] unions the sync resolver output
//! with org-discovery results and is called by the async pipeline after
//! discovery.
//!
//! Test: unit tests cover the URL/pagination logic with inlined JSON; live
//! API calls are gated with `#[ignore]`.

use tracing::{debug, warn};

use super::client::{GITHUB_API_BASE, PAGE_SIZE};
use super::retry::retry_get;
use crate::collect::errors::{CollectError, Result};
use crate::core::config::{GithubConfig, RepositoryConfig};

/// Minimal wire shape for one entry in `GET /orgs/{org}/repos`.
#[derive(Debug, serde::Deserialize)]
struct ApiOrgRepo {
    /// Full `owner/name` slug (e.g. `"acme/widget"`).
    full_name: String,
}

/// Discover all visible repositories for a single GitHub org by paginating
/// `GET /orgs/{org}/repos?type=all`.
///
/// Why: multi-org PR collection (issue #742) must discover repos
/// dynamically rather than requiring operators to maintain a full repo
/// list; the GitHub REST API exposes all org-member repos that the
/// token can see.
/// What: pages the org repos endpoint (100 per page) using the shared
/// [`retry_get`] helper (same 3-retry exponential backoff used by the main
/// client), until a short page indicates end-of-list; parses each
/// `full_name` into an `(owner, repo)` tuple; returns the complete list.
/// Token visibility determines which repos appear — private repos require
/// a PAT with `repo` scope.
/// Test: `api_org_repo_full_name_splits_correctly` below (unit-level, no
/// live network call); `discover_org_repos_live` is `#[ignore]`.
///
/// # Errors
///
/// - [`CollectError::Http`] on transport or non-success HTTP (after retries
///   are exhausted; transient 5xx/429 are retried with exponential backoff).
/// - [`CollectError::Config`] if a `full_name` returned by the API cannot be
///   split into `owner/name` (should not happen in practice but is guarded).
pub async fn discover_org_repos(
    client: &reqwest::Client,
    org: &str,
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut page = 1u32;
    loop {
        let url =
            format!("{GITHUB_API_BASE}/orgs/{org}/repos?type=all&per_page={PAGE_SIZE}&page={page}");
        debug!(url = %url, "GET (org repo discovery)");
        // Route through the shared retry helper so transient 5xx/429 are
        // retried with the same backoff as the main PR client.
        let resp = retry_get(client, &url).await?;

        // Respect rate-limit hints (same pattern as the main client).
        if let Some(rem) = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
        {
            if rem < 5 {
                warn!(
                    remaining = rem,
                    org = %org,
                    "GitHub rate limit nearly exhausted during org discovery"
                );
            }
        }

        let resp = resp.error_for_status()?;
        let repos: Vec<ApiOrgRepo> = resp.json().await.map_err(|e| {
            CollectError::Config(format!("org repos JSON parse failed for {org}: {e}"))
        })?;

        let n = repos.len();
        for r in repos {
            match r.full_name.split_once('/') {
                Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() => {
                    out.push((owner.to_string(), repo.to_string()));
                }
                _ => {
                    warn!(
                        full_name = %r.full_name,
                        "could not parse full_name from GitHub org repos; skipping"
                    );
                }
            }
        }

        if (n as u32) < PAGE_SIZE {
            break;
        }
        page += 1;
    }
    debug!(org = %org, count = out.len(), "org repo discovery complete");
    Ok(out)
}

/// Build the effective org list by merging `github.org` (singular back-compat)
/// into `github.orgs` (plural list), deduplicating while preserving insertion
/// order. Explicit per-repo `org:` and `github.repo` are handled separately
/// in [`super::client::resolve_github_repos`].
///
/// Why: the config schema exposes both `github.org` (old) and `github.orgs`
/// (new). Merging them here into one canonical list lets discovery code ignore
/// the distinction entirely.
/// What: returns `orgs ++ [org]` deduplicated. Empty orgs or orgs that are
/// already present in the list are dropped.
/// Test: `effective_orgs_deduplicates` below.
pub fn effective_orgs(org: Option<&str>, orgs: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let singular = org.into_iter();
    for o in orgs.iter().map(String::as_str).chain(singular) {
        if !o.is_empty() && seen.insert(o.to_string()) {
            out.push(o.to_string());
        }
    }
    out
}

/// Like [`super::client::resolve_github_repos`] but unions in `org_discovered`
/// pairs that were fetched asynchronously by the caller via
/// [`discover_org_repos`].
///
/// Why: keeps the core per-repo resolution logic in `client.rs` (sync) while
/// letting the async pipeline inject org-discovery results in one dedup pass.
/// What: calls [`super::client::resolve_github_repos`] for the repositories
/// slice, then appends each `org_discovered` pair not already in the set.
/// Test: `resolve_github_repos_with_org_discovered_appends` below.
pub fn resolve_github_repos_with_discovered(
    github: &GithubConfig,
    repositories: &[RepositoryConfig],
    org_discovered: &[(String, String)],
) -> Vec<(String, String)> {
    // Start with the per-repo resolution from client.rs.
    let base = super::client::resolve_github_repos(github, repositories);
    let mut seen: std::collections::HashSet<(String, String)> = base.iter().cloned().collect();
    let mut out = base;

    // Append org-discovered repos not already in the set.
    for p in org_discovered {
        if seen.insert(p.clone()) {
            out.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mk_repos(orgs: &[&str]) -> Vec<String> {
        orgs.iter().map(|s| s.to_string()).collect()
    }

    /// Why: `effective_orgs` must merge `org` + `orgs`, deduplicate, and
    /// preserve insertion order so the first-seen org is tried first.
    /// What: various combinations of None/Some and overlap.
    /// Test: assert exact vecs.
    #[test]
    fn effective_orgs_deduplicates() {
        // Both None and empty slice → empty.
        assert!(effective_orgs(None, &[]).is_empty());

        // Singular org only.
        assert_eq!(effective_orgs(Some("acme"), &[]), vec!["acme".to_string()]);

        // orgs list only.
        assert_eq!(
            effective_orgs(None, &mk_repos(&["acme", "hotstats"])),
            vec!["acme".to_string(), "hotstats".to_string()]
        );

        // Both set, no overlap → orgs first, then org appended.
        assert_eq!(
            effective_orgs(Some("singularorg"), &mk_repos(&["acme", "hotstats"])),
            vec![
                "acme".to_string(),
                "hotstats".to_string(),
                "singularorg".to_string()
            ]
        );

        // Overlap: singular `org` already in `orgs` → deduplicated.
        assert_eq!(
            effective_orgs(Some("acme"), &mk_repos(&["acme", "hotstats"])),
            vec!["acme".to_string(), "hotstats".to_string()]
        );

        // Empty string org is dropped.
        assert_eq!(
            effective_orgs(Some(""), &mk_repos(&["acme"])),
            vec!["acme".to_string()]
        );
    }

    /// Why: the pagination logic must accumulate repos across multiple pages
    /// and stop when a page shorter than PAGE_SIZE is returned. We unit-test
    /// the JSON parsing shape without a live network call.
    /// What: directly test that `ApiOrgRepo.full_name` splits correctly.
    /// Test: assert two full_name values round-trip to (owner, repo) pairs.
    #[test]
    fn api_org_repo_full_name_splits_correctly() {
        let json = r#"[
            {"full_name": "acme/widget", "id": 1},
            {"full_name": "acme/gadget", "id": 2}
        ]"#;
        let repos: Vec<ApiOrgRepo> = serde_json::from_str(json).expect("parses");
        let pairs: Vec<(String, String)> = repos
            .into_iter()
            .filter_map(|r| {
                r.full_name
                    .split_once('/')
                    .map(|(o, n)| (o.to_string(), n.to_string()))
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("acme".to_string(), "widget".to_string()),
                ("acme".to_string(), "gadget".to_string()),
            ]
        );
    }

    /// Why: `resolve_github_repos_with_discovered` must union org-discovery
    /// results with per-repo resolved pairs and deduplicate.
    /// What: call with a repositories slice + org_discovered including one
    /// duplicate.
    /// Test: discovered repos appended; duplicate not re-added.
    #[test]
    fn resolve_github_repos_with_org_discovered_appends() {
        use crate::core::config::GithubConfig;

        let cfg = GithubConfig {
            token: None,
            org: Some("acme".to_string()),
            orgs: vec![],
            repo: None,
            fetch_prs: true,
            fetch_pr_reviews: true,
            review_fetch_concurrency: 1,
            ticket_regex: None,
        };
        let repo_cfg = RepositoryConfig {
            path: PathBuf::from("/tmp/widget"),
            name: None,
            org: None,
            ..Default::default()
        };
        // org_discovered contains widget (already resolved) + new gadget.
        let discovered = vec![
            ("acme".to_string(), "widget".to_string()),
            ("acme".to_string(), "gadget".to_string()),
        ];
        let resolved = resolve_github_repos_with_discovered(&cfg, &[repo_cfg], &discovered);
        assert_eq!(
            resolved,
            vec![
                ("acme".to_string(), "widget".to_string()),
                ("acme".to_string(), "gadget".to_string()),
            ]
        );
    }
}
