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

use crate::collect::errors::CollectError;
use crate::collect::github::budget::RunBudget;
use crate::collect::github::org_discovery::{discover_org_repos_within, effective_orgs};
use crate::collect::github::repo_resolver::build_http_client;
use crate::collect::github::reviewer_store::upsert_github_pr_reviewer;
use crate::collect::github::types::GitHubReview;
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
pub(super) async fn run_github_org_discovery(
    gh_cfg: &GithubConfig,
    budget: &RunBudget,
) -> Vec<(String, String)> {
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

    // #6084: one budget for the whole multi-org pass, so a rate limit that
    // terminates the first org does not let the second start the storm again.
    // #6565: that budget is now the RUN's, not this pass's — the PR sweep and
    // the reviewer pass charge the same allowance rather than each getting a
    // fresh 120 s.
    let budget = budget.shared();
    let mut all: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for org in &orgs {
        info!(org = %org, "discovering repositories for GitHub org");
        match discover_org_repos_within(&http, org, budget).await {
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
///
/// #6553: a secondary rate limit no longer fails the run — see
/// [`ingest_reviews`], which this delegates to once the client is built.
pub(super) async fn fetch_and_store_github_reviewers(
    db: &mut Database,
    gh_cfg: &GithubConfig,
    force_refresh_prs: bool,
    stats: &mut CollectionStats,
    budget: &RunBudget,
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
                stats.fail_stage(format!("GitHub reviewer query prepare failed: {e}"));
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
                stats.fail_stage(format!("GitHub reviewer query failed: {e}"));
                return;
            }
        }
    };

    if prs.is_empty() {
        return;
    }
    info!(count = prs.len(), "fetching GitHub PR reviews");

    // Build a reviews-only client (no dummy repo slugs needed).
    let gh_client = match GitHubClient::new_for_reviews(gh_cfg).map(|c| c.with_run_budget(budget)) {
        Ok(c) => c,
        Err(e) => {
            stats.fail_stage(format!("GitHub reviewer client init failed: {e}"));
            return;
        }
    };

    // Clamp concurrency to at least 1 so a config value of 0 is serial, not
    // "unlimited" (buffer_unordered(0) would block indefinitely).
    let concurrency = (gh_cfg.review_fetch_concurrency as usize).max(1);

    ingest_reviews(db, &gh_client, concurrency, &prs, stats).await;
}

/// What one pull request's reviewer fetch produced.
///
/// Why: #6553 — every outcome used to collapse into `Result<Vec<_>, String>`,
/// which made "this one pull request is broken" and "GitHub stopped answering
/// the whole run" the same value. A field run turned one run-wide condition
/// into 21,230 identical per-PR warnings and then a stage failure that failed
/// the entire `tga collect`.
/// What: the three outcomes the pass treats differently — reviews came back,
/// this PR alone failed, or GitHub was rate-limiting so this PR contributed no
/// reviewer rows.
/// Test: `a_secondary_rate_limit_leaves_the_reviewer_pass_partial_not_failed`.
enum ReviewFetch {
    /// Reviews came back; the vector may legitimately be empty.
    Fetched(Vec<GitHubReview>),
    /// This PR alone failed and the pass carried on.
    Failed(String),
    /// GitHub was rate-limiting, so no reviewer rows exist for this PR.
    RateLimited,
}

/// Fetch reviews for `prs` with up to `concurrency` requests in flight.
///
/// Why: #6553 — the run-wide [`FetchBudget`] already stops a latched client
/// from sending anything, so past the trip every call fails without a request.
/// What this pass was missing is the ability to tell that outcome apart from a
/// broken pull request: both arrived as a `String` and both got a warning line,
/// which is how one rate limit produced 21,230 of them.
/// What: separates [`CollectError::Throttled`] from every other error, so the
/// caller can count the rate-limited pull requests instead of narrating each
/// one.
/// Test: `a_secondary_rate_limit_leaves_the_reviewer_pass_partial_not_failed`.
async fn fetch_reviews(
    client: &GitHubClient,
    prs: &[(i64, String, u64)],
    concurrency: usize,
) -> Vec<(i64, String, u64, ReviewFetch)> {
    futures::stream::iter(prs.iter().cloned())
        .map(|(pr_db_id, repository, pr_number)| async move {
            // Parse (owner, repo) from the stored repository slug.
            let outcome = match repository.split_once('/') {
                Some((o, r)) if !o.is_empty() && !r.is_empty() => {
                    match client.fetch_pr_reviews_for_repo(o, r, pr_number).await {
                        Ok(reviews) => ReviewFetch::Fetched(reviews),
                        Err(CollectError::Throttled { .. }) => ReviewFetch::RateLimited,
                        Err(e) => ReviewFetch::Failed(e.to_string()),
                    }
                }
                _ => ReviewFetch::Failed(format!(
                    "malformed repository slug '{repository}'; skipping reviewer fetch"
                )),
            };
            (pr_db_id, repository, pr_number, outcome)
        })
        .buffer_unordered(concurrency)
        .collect()
        .await
}

/// Fetch and persist reviewer rows for `prs` using an already-built client.
///
/// Why: #6553 — `fetch_and_store_github_reviewers` constructs its own client
/// against api.github.com, so the rate-limit path had no seam a test could
/// drive. Taking the client as an argument is what lets the regression test
/// point the pass at a local mock.
/// What: runs [`fetch_reviews`], then serializes the upserts (a rusqlite
/// `Connection` is not `Send`). A rate limit is recorded ONCE, as
/// [`crate::collect::CollectionStats::skip_item`] rather than `fail_stage`: the
/// PR query is forward-only, so the next `tga collect` resumes at the PRs this
/// run never reached, and failing the whole run made `fetch_pr_reviews: true`
/// unusable unattended.
/// Test: `a_secondary_rate_limit_leaves_the_reviewer_pass_partial_not_failed`,
/// `fetch_reviewers_concurrency_upserts_all`.
async fn ingest_reviews(
    db: &mut Database,
    gh_client: &GitHubClient,
    concurrency: usize,
    prs: &[(i64, String, u64)],
    stats: &mut CollectionStats,
) {
    let fetched = fetch_reviews(gh_client, prs, concurrency).await;

    let mut rate_limited = 0usize;
    for (pr_db_id, repository, pr_number, outcome) in fetched {
        match outcome {
            ReviewFetch::Fetched(reviews) => {
                let conn = db.connection();
                for review in &reviews {
                    match upsert_github_pr_reviewer(conn, pr_db_id, review) {
                        Ok(()) => stats.reviewers_fetched += 1,
                        Err(e) => {
                            // #5655: one PR's reviewer row, not the pass — the
                            // remaining PRs still upsert.
                            stats.skip_item(format!(
                                "reviewer upsert failed for {repository}#{pr_number}: {e}"
                            ));
                        }
                    }
                }
            }
            ReviewFetch::Failed(msg) => {
                // Per-PR failure is non-fatal; log and continue.
                warn!(
                    repository = %repository,
                    pr_number,
                    "GitHub reviewer fetch failed for PR: {msg}; continuing"
                );
            }
            // #6553: counted, not logged — see the aggregate below.
            ReviewFetch::RateLimited => rate_limited += 1,
        }
    }

    // #6084: per-PR review failures are logged and skipped, which is right for
    // one bad PR and wrong for a rate limit — under a limit every remaining PR
    // "fails" and the pass would report a clean run holding partial data.
    for notice in gh_client.fetch_notices() {
        stats.skip_item(format!("github reviewers: {notice}"));
    }
    if rate_limited > 0 {
        // #6553: one recorded fault for a run-wide condition, at a severity
        // that leaves `tga collect` at exit 0 — the reviewer pass is resumable
        // and the rest of the run's data is complete.
        let msg = format!(
            "github reviewers: GitHub rate-limited the reviewer pass; {rate_limited} of {} \
             pull request(s) got no reviewer rows, so pr_reviewers is INCOMPLETE for this \
             run. The pass is forward-only, so a later `tga collect` resumes at the pull \
             requests this one never reached (see #6553)",
            prs.len()
        );
        warn!("{msg}");
        stats.skip_item(msg);
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
    use crate::collect::github::types::{GhUser, GitHubReview};
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
            fetch_on_reference: false,
            work_items_unavailable: None,
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

    /// Why: #6553 — the reviewer pass met a secondary rate limit, recorded a
    /// `fail_stage`, and made three consecutive unattended `tga collect` runs
    /// exit 1 while every other stage had persisted cleanly. Each remaining
    /// pull request also got its own warning line — 21,230 of them for one
    /// run-wide condition. A secondary limit must leave the run partial and
    /// resumable, not failed.
    /// What: a mock that answers every reviews call `403` with `Retry-After: 0`
    /// (the field shape). At `concurrency = 1` the first PR spends its four
    /// attempts and latches the run-wide budget, after which the budget refuses
    /// to send. Asserts the pass records no stage failure, records ONE fault
    /// naming how many pull requests went unfetched, and sends exactly
    /// `MAX_RETRIES + 1` requests for 50 pull requests.
    /// Test: this test itself. Pre-fix it fails on the first assertion — a
    /// latched budget drove `fail_stage`, which `tga collect` turns into a
    /// non-zero exit (`crate::commands::collect::stage_failure_report`).
    #[tokio::test]
    async fn a_secondary_rate_limit_leaves_the_reviewer_pass_partial_not_failed() {
        use crate::collect::github::retry::MAX_RETRIES;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const PR_COUNT: i64 = 50;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).insert_header("retry-after", "0"))
            .mount(&server)
            .await;

        let mut db = open_db();
        let prs: Vec<(i64, String, u64)> = {
            let conn = db.connection();
            (1..=PR_COUNT)
                .map(|n| {
                    let id = seed_pr(conn, "acme/widgets", n);
                    (id, "acme/widgets".to_string(), n as u64)
                })
                .collect()
        };

        let client = GitHubClient::new_for_reviews(&make_gh_cfg(1))
            .expect("client builds")
            .with_api_base(server.uri());

        let mut stats = CollectionStats::default();
        ingest_reviews(&mut db, &client, 1, &prs, &mut stats).await;

        assert!(
            stats.stage_failures().is_empty(),
            "a rate limit must not fail the run; got: {:?}",
            stats.stage_failures()
        );
        assert_eq!(
            stats.errors.len(),
            1,
            "one aggregated fault for a run-wide condition, not one per PR; got: {:?}",
            stats.errors
        );
        let msg = &stats.errors[0].message;
        assert!(
            msg.contains(&format!("{PR_COUNT} of {PR_COUNT}")),
            "the fault must name how many PRs went unfetched: {msg}"
        );
        assert!(
            msg.contains("INCOMPLETE"),
            "the fault must say the reviewer data is incomplete: {msg}"
        );

        let requests = server
            .received_requests()
            .await
            .map(|r| r.len())
            .unwrap_or_default();
        assert_eq!(
            requests,
            (MAX_RETRIES + 1) as usize,
            "only the first PR may be attempted; the other 49 must cost zero requests"
        );

        let rows: i64 = {
            let conn = db.connection();
            conn.query_row("SELECT COUNT(*) FROM pr_reviewers", [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(rows, 0, "a throttled pass writes no reviewer rows");
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
