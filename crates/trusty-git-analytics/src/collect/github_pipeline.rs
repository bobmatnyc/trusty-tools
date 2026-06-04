//! Async pipeline helpers that bridge the GitHub client and the collection
//! orchestrator for the two new capabilities added in issue #742:
//!
//! 1. **Org-discovery** (`run_github_org_discovery`): paginates
//!    `GET /orgs/{org}/repos` for every org in the effective org list and
//!    returns the union of discovered `(owner, repo)` pairs.
//! 2. **Reviewer ingestion** (`fetch_and_store_github_reviewers`): after PRs
//!    are stored, fetches reviews for each GitHub PR serially and upserts
//!    `pr_reviewers` rows.
//!
//! Both functions were originally `CollectionPipeline` methods. Extracting
//! them keeps `collector.rs` within the 500-line budget while grouping the
//! GitHub-specific async code here.

use tracing::{info, warn};

use crate::collect::github::org_discovery::{discover_org_repos, effective_orgs};
use crate::collect::github::reviewer_store::upsert_github_pr_reviewer;
use crate::core::config::GithubConfig;
use crate::core::db::Database;

use crate::collect::collector::CollectionStats;

/// Run GitHub org-discovery for every org in `effective_orgs`, returning
/// the union of all discovered `(owner, repo)` pairs.
///
/// Why: `github.orgs` (issue #742) lets operators list multiple GitHub
/// orgs; each org requires a separate `GET /orgs/{org}/repos` call that
/// must run before the PR client is constructed.
/// What: calls [`discover_org_repos`] serially for each org in the effective
/// list (`orgs` ++ `org`, deduped); returns the combined pairs. Per-org
/// failures are logged and skipped (partial-success).
/// Test: the underlying discovery function is tested in
/// `github::org_discovery::tests`; the serial aggregation here is
/// exercised end-to-end by integration tests with `#[ignore]`.
pub(super) async fn run_github_org_discovery(gh_cfg: &GithubConfig) -> Vec<(String, String)> {
    let orgs = effective_orgs(gh_cfg.org.as_deref(), &gh_cfg.orgs);
    if orgs.is_empty() {
        return Vec::new();
    }

    // Build a temporary HTTP client for discovery using the same auth token
    // as the PR client so visibility is consistent.
    let http = match crate::collect::github::client::build_http_client_for_discovery(gh_cfg) {
        Ok(c) => c,
        Err(e) => {
            warn!("GitHub org-discovery: could not build HTTP client: {e}");
            return Vec::new();
        }
    };

    let mut all: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for org in &orgs {
        info!(org = %org, "discovering repositories for GitHub org");
        match discover_org_repos(&http, org).await {
            Ok(repos) => {
                info!(org = %org, count = repos.len(), "org discovery complete");
                for p in repos {
                    if seen.insert(p.clone()) {
                        all.push(p);
                    }
                }
            }
            Err(e) => {
                warn!(
                    org = %org,
                    error = %e,
                    "org discovery failed; continuing with other orgs"
                );
            }
        }
    }
    all
}

/// Serial reviewer-ingestion pass for all stored GitHub PRs.
///
/// Why: GitHub's reviews endpoint (`GET /repos/{o}/{r}/pulls/{n}/reviews`)
/// is one additional API call per PR; fetching serially keeps us inside the
/// 5 000 req/hour authenticated rate limit by default.
/// What: queries all GitHub PRs from the DB (or just new ones when
/// `force_refresh_prs=false`), fetches reviews for each via
/// `fetch_pr_reviews_for_repo`, upserts into `pr_reviewers`. Per-PR HTTP
/// failures are logged and non-fatal.
/// Test: round-trip covered by `reviewer_store` unit tests; the API path is
/// gated `#[ignore]`.
pub(super) async fn fetch_and_store_github_reviewers(
    db: &mut Database,
    gh_cfg: &GithubConfig,
    force_refresh_prs: bool,
    stats: &mut CollectionStats,
) {
    // Gather all GitHub PRs that need reviewer data.  When force_refresh_prs
    // is true we re-fetch all; otherwise we only fetch PRs that have no
    // reviewer rows yet (forward-only).
    let prs: Vec<(i64, String, u64)> = {
        let conn = db.connection();
        let query = if force_refresh_prs {
            "SELECT id, repository, pr_number FROM pull_requests \
             WHERE provider = 'github' ORDER BY id"
        } else {
            "SELECT p.id, p.repository, p.pr_number \
             FROM pull_requests p \
             WHERE p.provider = 'github' \
               AND NOT EXISTS ( \
                   SELECT 1 FROM pr_reviewers r \
                   WHERE r.pr_id = p.id AND r.provider = 'github' \
               ) \
             ORDER BY p.id"
        };
        let mut stmt = match conn.prepare(query) {
            Ok(s) => s,
            Err(e) => {
                stats
                    .errors
                    .push(format!("GitHub reviewer query prepare failed: {e}"));
                return;
            }
        };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u64,
            ))
        });
        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                stats
                    .errors
                    .push(format!("GitHub reviewer query failed: {e}"));
                return;
            }
        }
    };

    if prs.is_empty() {
        return;
    }
    info!(count = prs.len(), "fetching GitHub PR reviews");

    // Build a GitHubClient for the reviews endpoint.
    // `fetch_pr_reviews_for_repo` takes explicit (owner, repo, pr_number) so
    // the primary repo fields in the client don't matter here.
    let dummy_repos = vec![("_dummy".to_string(), "_dummy".to_string())];
    let gh_client = match crate::collect::github::GitHubClient::new_for_prs(gh_cfg, dummy_repos) {
        Ok(c) => c,
        Err(e) => {
            stats.errors.push(format!(
                "GitHub reviewer client (for reviews) init failed: {e}"
            ));
            return;
        }
    };

    for (pr_db_id, repository, pr_number) in &prs {
        // Parse (owner, repo) from the stored repository slug.
        let (owner, repo) = match repository.split_once('/') {
            Some((o, r)) if !o.is_empty() && !r.is_empty() => (o, r),
            _ => {
                warn!(
                    repository = %repository,
                    "GitHub PR has malformed repository slug; skipping reviewer fetch"
                );
                continue;
            }
        };

        match gh_client
            .fetch_pr_reviews_for_repo(owner, repo, *pr_number)
            .await
        {
            Ok(reviews) => {
                let conn = db.connection();
                for review in &reviews {
                    match upsert_github_pr_reviewer(conn, *pr_db_id, review) {
                        Ok(()) => stats.reviewers_fetched += 1,
                        Err(e) => {
                            stats.errors.push(format!(
                                "reviewer upsert failed for {repository}#{pr_number}: {e}"
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                // Per-PR failure is non-fatal; log and continue.
                warn!(
                    repository = %repository,
                    pr_number,
                    error = %e,
                    "GitHub reviewer fetch failed for PR; continuing"
                );
            }
        }
    }

    if stats.reviewers_fetched > 0 {
        info!(
            count = stats.reviewers_fetched,
            "stored GitHub PR reviewers"
        );
    }
}
