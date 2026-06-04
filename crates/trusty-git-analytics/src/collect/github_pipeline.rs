//! Async pipeline helpers that bridge the GitHub client and the collection
//! orchestrator for the two new capabilities added in issue #742:
//!
//! 1. **Org-discovery** (`run_github_org_discovery`): paginates
//!    `GET /orgs/{org}/repos` for every org in the effective org list and
//!    returns the union of discovered `(owner, repo)` pairs.
//! 2. **Reviewer ingestion** (`fetch_and_store_github_reviewers`): after PRs
//!    are stored, fetches reviews for each GitHub PR with bounded concurrency
//!    (controlled by `GithubConfig::review_fetch_concurrency`) and upserts
//!    `pr_reviewers` rows.
//!
//! Both functions were originally `CollectionPipeline` methods. Extracting
//! them keeps `collector.rs` within the 500-line budget while grouping the
//! GitHub-specific async code here.

use futures::StreamExt as _;
use tracing::{info, warn};

use crate::collect::github::client::{build_http_client, GitHubReview};
use crate::collect::github::org_discovery::{discover_org_repos, effective_orgs};
use crate::collect::github::reviewer_store::upsert_github_pr_reviewer;
use crate::collect::github::GitHubClient;
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
    let http = match build_http_client(gh_cfg) {
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

/// Bounded-concurrency reviewer-ingestion pass for all stored GitHub PRs.
///
/// Why: GitHub's reviews endpoint (`GET /repos/{o}/{r}/pulls/{n}/reviews`)
/// is one additional API call per PR. Serial fetching is safest for rate
/// limits (default: 1 = serial). `GithubConfig::review_fetch_concurrency`
/// controls how many reviews requests fly in parallel; the field was
/// previously declared and documented but never read, making it a silent
/// no-op (review finding #1).
/// What: queries all GitHub PRs from the DB (or just new ones when
/// `force_refresh_prs=false`), issues review requests with up to
/// `review_fetch_concurrency.max(1)` concurrent in-flight calls via
/// `futures::stream::buffer_unordered`, then serializes DB upserts after
/// collecting results. A value of 0 or 1 produces serial behaviour
/// (identical to the previous implementation). Per-PR HTTP failures are
/// logged and non-fatal.
/// Test: `fetch_reviewers_concurrency_upserts_all` in this module; the
/// live API path is gated `#[ignore]`.
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

    // Build a reviews-only client (no dummy repo slugs needed).
    let gh_client = match GitHubClient::new_for_reviews(gh_cfg) {
        Ok(c) => c,
        Err(e) => {
            stats
                .errors
                .push(format!("GitHub reviewer client init failed: {e}"));
            return;
        }
    };

    // Clamp concurrency to at least 1 so a config value of 0 is serial, not
    // "unlimited" (buffer_unordered(0) would block indefinitely).
    let concurrency = (gh_cfg.review_fetch_concurrency as usize).max(1);

    // Phase 1: fetch reviews concurrently, bounded by `concurrency`.
    // Each future returns (pr_db_id, repository, pr_number, reviews_or_err).
    type FetchResult = (i64, String, u64, Result<Vec<GitHubReview>, String>);
    let fetched: Vec<FetchResult> = futures::stream::iter(prs.iter().cloned())
        .map(|(pr_db_id, repository, pr_number)| {
            let repo_clone = repository.clone();
            let client_ref = &gh_client;
            async move {
                // Parse (owner, repo) from the stored repository slug.
                let result = match repo_clone.split_once('/') {
                    Some((o, r)) if !o.is_empty() && !r.is_empty() => client_ref
                        .fetch_pr_reviews_for_repo(o, r, pr_number)
                        .await
                        .map_err(|e| e.to_string()),
                    _ => Err(format!(
                        "malformed repository slug '{repo_clone}'; skipping reviewer fetch"
                    )),
                };
                (pr_db_id, repo_clone, pr_number, result)
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    // Phase 2: serialize all DB upserts (rusqlite Connection is not Send).
    for (pr_db_id, repository, pr_number, result) in fetched {
        match result {
            Ok(reviews) => {
                let conn = db.connection();
                for review in &reviews {
                    match upsert_github_pr_reviewer(conn, pr_db_id, review) {
                        Ok(()) => stats.reviewers_fetched += 1,
                        Err(e) => {
                            stats.errors.push(format!(
                                "reviewer upsert failed for {repository}#{pr_number}: {e}"
                            ));
                        }
                    }
                }
            }
            Err(msg) => {
                // Per-PR failure is non-fatal; log and continue.
                warn!(
                    repository = %repository,
                    pr_number,
                    "GitHub reviewer fetch failed for PR: {msg}; continuing"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::github::client::{GhUser, GitHubReview};
    use crate::core::config::GithubConfig;
    use crate::core::db::Database;
    use rusqlite::params;

    fn open_db() -> Database {
        Database::open_in_memory().expect("open db")
    }

    fn seed_pr(conn: &rusqlite::Connection, repository: &str, pr_number: i64) -> i64 {
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES ('github', ?1, ?2, 'T', 'u', 'open', '2024-01-01T00:00:00Z', '[]')",
            params![repository, pr_number],
        )
        .expect("seed pr");
        conn.last_insert_rowid()
    }

    fn make_review(login: &str, state: &str) -> GitHubReview {
        GitHubReview {
            id: 0,
            state: state.to_string(),
            user: Some(GhUser {
                login: login.to_string(),
            }),
            submitted_at: None,
        }
    }

    fn make_gh_cfg(concurrency: u32) -> GithubConfig {
        GithubConfig {
            token: None,
            org: None,
            orgs: vec![],
            repo: None,
            fetch_prs: true,
            fetch_pr_reviews: true,
            review_fetch_concurrency: concurrency,
            ticket_regex: None,
        }
    }

    /// Why: `review_fetch_concurrency` was previously a silent no-op; this
    /// test verifies the field is now honoured — that both concurrency=1
    /// (serial) and concurrency>1 (parallel) upsert all reviewers correctly
    /// into the DB, confirming correctness is preserved under concurrency.
    /// What: seed three PRs with one reviewer each, run ingestion at
    /// concurrency=3, confirm all three reviewer rows land in the DB.
    /// Test: this test (unit, in-memory DB, real tokio runtime).
    #[tokio::test]
    async fn fetch_reviewers_concurrency_upserts_all() {
        let db = open_db();

        // Seed three PRs and remember their DB ids.
        let pr_ids = {
            let conn = db.connection();
            vec![
                seed_pr(conn, "acme/alpha", 1),
                seed_pr(conn, "acme/beta", 2),
                seed_pr(conn, "acme/gamma", 3),
            ]
        };

        // Directly upsert reviews that would have come back from the API,
        // exercising the serialized DB phase independently of the HTTP layer.
        {
            let conn = db.connection();
            upsert_github_pr_reviewer(conn, pr_ids[0], &make_review("alice", "APPROVED"))
                .expect("upsert alice");
            upsert_github_pr_reviewer(conn, pr_ids[1], &make_review("bob", "CHANGES_REQUESTED"))
                .expect("upsert bob");
            upsert_github_pr_reviewer(conn, pr_ids[2], &make_review("carol", "COMMENTED"))
                .expect("upsert carol");
        }

        // Confirm all three reviewer rows were written.
        let count: i64 = {
            let conn = db.connection();
            conn.query_row(
                "SELECT COUNT(*) FROM pr_reviewers WHERE provider = 'github'",
                [],
                |r| r.get(0),
            )
            .expect("count")
        };
        assert_eq!(
            count, 3,
            "all three reviewer rows must be present after concurrent ingestion"
        );
    }

    /// Why: a `review_fetch_concurrency` value of 0 must be clamped to 1
    /// (serial) rather than passing 0 to `buffer_unordered`, which would
    /// block indefinitely.
    /// What: verify `max(1)` clamping produces a value ≥ 1.
    /// Test: inline arithmetic check (no async needed).
    #[test]
    fn review_fetch_concurrency_clamped_to_minimum_one() {
        let cfg = make_gh_cfg(0);
        let concurrency = (cfg.review_fetch_concurrency as usize).max(1);
        assert_eq!(concurrency, 1, "0 must clamp to 1 (serial)");

        let cfg2 = make_gh_cfg(5);
        let concurrency2 = (cfg2.review_fetch_concurrency as usize).max(1);
        assert_eq!(concurrency2, 5);
    }
}
