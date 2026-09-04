//! Bitbucket Cloud workspace → repository discovery (#5220).
//!
//! Why: `bitbucket.workspace` + `bitbucket.repo_slug` names exactly one
//! repository, so collecting from a workspace of 200 repositories meant
//! transcribing 200 slugs by hand and re-editing the config every time someone
//! created a repository. GitHub has had `discover_org_repos` since #742; this
//! is its Bitbucket counterpart, and it feeds the same `(owner, repo)` repo-set
//! shape into the same call sites.
//!
//! What: [`discover_workspace_repos`] pages `GET /2.0/repositories/{workspace}`
//! over Bitbucket's `next`-cursor convention, borrowing the credentials, headers
//! and retry policy of an existing [`BitbucketClient`] rather than building a
//! second HTTP path. [`effective_workspaces`] normalises the configured list and
//! [`resolve_bitbucket_repos`] unions the discovered pairs with the singular
//! `workspace`/`repo_slug` pair.
//!
//! Fail-open is the failure mode this module exists to refuse: an unreadable
//! workspace must never read as an empty one. A rejected credential becomes
//! [`CollectError::BitbucketApi`], a rate limit becomes
//! [`CollectError::Throttled`], and the page cap records a truncation warning —
//! none of the three returns `Ok(vec![])`.
//!
//! Test: `workspace_discovery_follows_next_cursor`,
//! `workspace_discovery_names_an_auth_failure`,
//! `workspace_discovery_names_a_rate_limit`,
//! `a_truncated_workspace_listing_reaches_the_run_stats`.

use std::collections::HashSet;
use std::time::Duration;

use tracing::{debug, info, warn};

use super::client::{BitbucketClient, PAGE_SIZE};
use super::types::{BbPaged, BbRepository};
use crate::collect::errors::{CollectError, Result};
use crate::core::config::BitbucketConfig;

/// Hard bound on pages walked per workspace, mirroring the GitHub org walk.
///
/// At [`PAGE_SIZE`] 50 this covers 5 000 repositories; beyond that the walk
/// stops and says so rather than paging indefinitely against a server whose
/// cursor never terminates.
const MAX_PAGES: u32 = 100;

/// Longest response-body excerpt kept in a [`CollectError::BitbucketApi`].
const MAX_BODY_EXCERPT: usize = 512;

/// Normalise the configured workspace list into the set discovery walks.
///
/// Why: a hand-edited YAML list picks up stray whitespace, blank entries and
/// duplicates, and each duplicate would otherwise cost a full extra page walk.
/// What: trims, drops empties, and deduplicates while preserving order. The
/// singular `bitbucket.workspace` is deliberately NOT folded in — it is half of
/// a repository coordinate, and promoting it to a discovery source would widen
/// every existing single-repository config to its whole workspace.
/// Test: `effective_workspaces_trims_and_deduplicates`.
#[must_use]
pub fn effective_workspaces(workspaces: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for w in workspaces.iter().map(|s| s.trim()) {
        if !w.is_empty() && seen.insert(w.to_string()) {
            out.push(w.to_string());
        }
    }
    out
}

/// Page one workspace's repositories over the Bitbucket Cloud REST API.
///
/// Why: this is the call `discover_org_repos` makes for GitHub, and the reason
/// a workspace can be named instead of enumerated.
/// What: `GET /2.0/repositories/{workspace}?pagelen=50`, following the absolute
/// `next` URL each page carries until it is absent or [`MAX_PAGES`] is reached.
/// Each repository yields `(workspace, repo_slug)` parsed from `full_name`,
/// falling back to `slug` paired with the requested workspace.
/// Test: `workspace_discovery_follows_next_cursor`,
/// `workspace_discovery_names_an_auth_failure`,
/// `workspace_discovery_names_a_rate_limit`.
///
/// # Errors
///
/// - [`CollectError::Throttled`] when Bitbucket answers 429 after the client's
///   retry budget is spent.
/// - [`CollectError::BitbucketApi`] for any other non-success status, carrying
///   Bitbucket's own explanation (401/403 scope problems included).
/// - [`CollectError::Http`] on transport failure, [`CollectError::Json`] on a
///   payload that does not parse.
pub async fn discover_workspace_repos(
    client: &BitbucketClient,
    workspace: &str,
) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut next_url = Some(format!(
        "{}/repositories/{workspace}?pagelen={PAGE_SIZE}",
        client.api_base()
    ));
    let mut pages = 0u32;

    while let Some(url) = next_url.take() {
        pages += 1;
        debug!(url = %url, "GET (bitbucket workspace repo discovery)");
        let resp = client.get_with_retry(&url).await?;
        warn_when_rate_limit_is_nearly_spent(&resp, workspace);
        let resp = classify_response(resp, &url).await?;

        let page: BbPaged<BbRepository> = resp.json().await?;
        for repo in page.values {
            match repo_pair(&repo, workspace) {
                Some(pair) => out.push(pair),
                None => warn!(
                    workspace = %workspace,
                    "Bitbucket workspace repo has neither full_name nor slug; skipping"
                ),
            }
        }

        if pages >= MAX_PAGES {
            // #6084: a warn! alone is invisible in the run's output. The notice
            // is what `pr_pipeline` turns into a recorded fault.
            warn!(
                workspace = %workspace,
                pages = MAX_PAGES,
                repositories = out.len(),
                "Bitbucket workspace discovery hit the page cap; the repository set is PARTIAL"
            );
            client.note_truncation(format!(
                "Bitbucket workspace listing for {workspace} stopped at the {MAX_PAGES}-page cap \
                 ({} repositories); the repository set is PARTIAL",
                MAX_PAGES * PAGE_SIZE
            ));
            break;
        }
        next_url = page.next;
    }

    debug!(
        workspace = %workspace,
        count = out.len(),
        "bitbucket workspace repo discovery complete"
    );
    Ok(out)
}

/// Turn a non-success response into the named error it deserves.
///
/// Why (#5220): `error_for_status()` collapses 401, 403 and 429 into one opaque
/// [`CollectError::Http`] with no body, and the caller that swallowed it would
/// report an empty workspace. Naming the two an operator can act on — a
/// credential the API rejected, and a rate limit — is what makes the failure
/// legible.
/// What: 429 becomes [`CollectError::Throttled`] with any `Retry-After` hint;
/// every other non-success status becomes [`CollectError::BitbucketApi`] with a
/// truncated body excerpt. A success passes through untouched.
/// Test: `workspace_discovery_names_an_auth_failure`,
/// `workspace_discovery_names_a_rate_limit`.
async fn classify_response(resp: reqwest::Response, url: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status.as_u16() == 429 {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        return Err(CollectError::Throttled {
            status: status.as_u16(),
            retry_after,
        });
    }
    let message = resp.text().await.unwrap_or_default();
    Err(CollectError::BitbucketApi {
        status: status.as_u16(),
        endpoint: url.to_string(),
        message: excerpt(&message),
    })
}

/// Trim a response body to something an error message can carry.
fn excerpt(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty response body>".to_string();
    }
    match trimmed.char_indices().nth(MAX_BODY_EXCERPT) {
        Some((idx, _)) => format!("{}…", &trimmed[..idx]),
        None => trimmed.to_string(),
    }
}

/// Log when Bitbucket says the request budget is nearly gone.
fn warn_when_rate_limit_is_nearly_spent(resp: &reqwest::Response, workspace: &str) {
    if let Some(rem) = resp
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
    {
        if rem < 5 {
            warn!(
                remaining = rem,
                workspace = %workspace,
                "Bitbucket rate limit nearly exhausted during workspace discovery"
            );
        }
    }
}

/// Extract `(workspace, repo_slug)` from one discovered repository.
fn repo_pair(repo: &BbRepository, workspace: &str) -> Option<(String, String)> {
    if let Some((ws, slug)) = repo
        .full_name
        .as_deref()
        .map(str::trim)
        .and_then(|f| f.split_once('/'))
    {
        if !ws.is_empty() && !slug.is_empty() {
            return Some((ws.to_string(), slug.to_string()));
        }
    }
    repo.slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| (workspace.to_string(), s.to_string()))
}

/// What a discovery pass found, and what it could not finish.
///
/// Why (#6084): the repository set alone cannot say whether it is the whole
/// workspace or the first 5 000 repositories of it. Returning the notices
/// alongside is what lets the collector hand them to the client that reaches
/// [`crate::collect::pr_pipeline`], where they become recorded run faults.
/// What: `repos` is the deduplicated union across workspaces; `notices` is
/// empty unless a listing was truncated.
/// Test: `a_truncated_workspace_listing_reaches_the_run_stats`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkspaceDiscovery {
    /// `(workspace, repo_slug)` pairs, deduplicated, in discovery order.
    pub repos: Vec<(String, String)>,
    /// Operator-facing notes about bounds the walk hit, in order.
    pub notices: Vec<String>,
}

/// Discover every configured workspace's repositories, in order.
///
/// Why: the collector needs one repository set, not one per workspace, and a
/// workspace it cannot read must not take the others down with it — the same
/// per-org partial-success shape `run_github_org_discovery` uses.
/// What: builds one discovery client via
/// [`BitbucketClient::new_for_discovery`], walks each workspace from
/// [`effective_workspaces`], and unions the results with duplicates removed. A
/// workspace that fails is logged and skipped; the caller sees the repositories
/// that were readable, plus every truncation notice the walk recorded.
/// Test: `run_workspace_discovery_skips_a_failing_workspace`,
/// `a_truncated_workspace_listing_reaches_the_run_stats`.
pub async fn run_workspace_discovery(config: &BitbucketConfig) -> WorkspaceDiscovery {
    let workspaces = effective_workspaces(&config.workspaces);
    if workspaces.is_empty() {
        return WorkspaceDiscovery::default();
    }
    let client = match BitbucketClient::new_for_discovery(config) {
        Ok(c) => c,
        Err(e) => {
            warn!("Bitbucket workspace discovery: could not build client: {e}");
            return WorkspaceDiscovery::default();
        }
    };

    let mut all: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for workspace in &workspaces {
        info!(workspace = %workspace, "discovering repositories for Bitbucket workspace");
        match discover_workspace_repos(&client, workspace).await {
            Ok(repos) => {
                info!(
                    workspace = %workspace,
                    count = repos.len(),
                    "bitbucket workspace discovery complete"
                );
                for pair in repos {
                    if seen.insert(pair.clone()) {
                        all.push(pair);
                    }
                }
            }
            Err(e) => warn!(
                workspace = %workspace,
                error = %e,
                "bitbucket workspace discovery failed; continuing with other workspaces"
            ),
        }
    }
    WorkspaceDiscovery {
        repos: all,
        notices: client.recorded_notices(),
    }
}

/// Union the configured `workspace`/`repo_slug` pair with discovered repos.
///
/// Why: a config may name one repository explicitly AND list workspaces to
/// discover; both belong in the set the client collects, and neither should be
/// listed twice. This is the Bitbucket counterpart of
/// `resolve_github_repos_with_discovered`.
/// What: the explicit pair (when both halves are non-empty) comes first, then
/// every discovered pair not already present.
/// Test: `discovered_repos_union_with_the_configured_pair`.
#[must_use]
pub fn resolve_bitbucket_repos(
    config: &BitbucketConfig,
    discovered: &[(String, String)],
) -> Vec<(String, String)> {
    let named = |v: &Option<String>| -> Option<String> {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    if let (Some(ws), Some(slug)) = (named(&config.workspace), named(&config.repo_slug)) {
        seen.insert((ws.clone(), slug.clone()));
        out.push((ws, slug));
    }
    for pair in discovered {
        if seen.insert(pair.clone()) {
            out.push(pair.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a discovery-capable config pointed at a mock server.
    fn discovery_config(api_base: String, workspaces: &[&str]) -> BitbucketConfig {
        BitbucketConfig {
            token: Some("dummy".into()),
            workspaces: workspaces.iter().map(|s| (*s).to_string()).collect(),
            fetch_prs: true,
            api_base_url: Some(api_base),
            ..Default::default()
        }
    }

    #[test]
    fn effective_workspaces_trims_and_deduplicates() {
        assert!(effective_workspaces(&[]).is_empty());
        assert_eq!(
            effective_workspaces(&[
                " acme ".to_string(),
                "acme".to_string(),
                String::new(),
                "hotstats".to_string(),
            ]),
            vec!["acme".to_string(), "hotstats".to_string()]
        );
    }

    #[test]
    fn workspace_page_deserializes_full_name_and_slug() {
        let json = r#"{"values": [
            {"full_name": "acme/widget", "slug": "widget"},
            {"slug": "gadget"},
            {"name": "nameless"}
        ]}"#;
        let page: BbPaged<BbRepository> = serde_json::from_str(json).expect("parses");
        let pairs: Vec<Option<(String, String)>> =
            page.values.iter().map(|r| repo_pair(r, "acme")).collect();
        assert_eq!(pairs[0], Some(("acme".into(), "widget".into())));
        assert_eq!(pairs[1], Some(("acme".into(), "gadget".into())));
        assert_eq!(pairs[2], None, "a repo with no identifier is skipped");
    }

    /// Both pages of a workspace listing are walked via the `next` cursor.
    #[tokio::test]
    async fn workspace_discovery_follows_next_cursor() {
        let server = MockServer::start().await;
        let base = server.uri();
        let page2_url = format!("{base}/repositories/acme?page=2");

        Mock::given(method("GET"))
            .and(path("/repositories/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"full_name": "acme/widget", "slug": "widget"}],
                "next": page2_url,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"full_name": "acme/gadget", "slug": "gadget"}],
            })))
            .mount(&server)
            .await;

        let cfg = discovery_config(base, &["acme"]);
        let client = BitbucketClient::new_for_discovery(&cfg).expect("client builds");
        let repos = discover_workspace_repos(&client, "acme")
            .await
            .expect("discovery succeeds");
        assert_eq!(
            repos,
            vec![
                ("acme".to_string(), "widget".to_string()),
                ("acme".to_string(), "gadget".to_string()),
            ]
        );
    }

    /// A rejected credential is named, never reported as an empty workspace.
    #[tokio::test]
    async fn workspace_discovery_names_an_auth_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"error": {"message": "Invalid credentials"}}"#),
            )
            .mount(&server)
            .await;

        let cfg = discovery_config(server.uri(), &["acme"]);
        let client = BitbucketClient::new_for_discovery(&cfg).expect("client builds");
        let err = discover_workspace_repos(&client, "acme")
            .await
            .expect_err("401 must not read as an empty workspace");
        match err {
            CollectError::BitbucketApi {
                status, message, ..
            } => {
                assert_eq!(status, 401);
                assert!(
                    message.contains("Invalid credentials"),
                    "the error must carry Bitbucket's explanation: {message}"
                );
            }
            other => panic!("expected BitbucketApi, got {other:?}"),
        }
    }

    /// A rate limit is named, never reported as an empty workspace.
    ///
    /// The retry budget is lowered to zero so the client does not spend its
    /// 1s/2s/4s backoff proving a permanent 429.
    #[tokio::test]
    async fn workspace_discovery_names_a_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "42"))
            .mount(&server)
            .await;

        let cfg = discovery_config(server.uri(), &["acme"]);
        let client = BitbucketClient::new_for_discovery(&cfg)
            .expect("client builds")
            .with_max_retries(0);
        let err = discover_workspace_repos(&client, "acme")
            .await
            .expect_err("429 must not read as an empty workspace");
        match err {
            CollectError::Throttled {
                status,
                retry_after,
            } => {
                assert_eq!(status, 429);
                assert_eq!(retry_after, Some(Duration::from_secs(42)));
            }
            other => panic!("expected Throttled, got {other:?}"),
        }
    }

    /// One unreadable workspace does not discard the readable one.
    #[tokio::test]
    async fn run_workspace_discovery_skips_a_failing_workspace() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"full_name": "acme/widget", "slug": "widget"}],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repositories/locked"))
            .respond_with(ResponseTemplate::new(403).set_body_string("no access"))
            .mount(&server)
            .await;

        let cfg = discovery_config(server.uri(), &["acme", "locked", "acme"]);
        let found = run_workspace_discovery(&cfg).await;
        assert_eq!(
            found.repos,
            vec![("acme".to_string(), "widget".to_string())]
        );
        assert!(
            found.notices.is_empty(),
            "a listing that completed records no truncation: {:?}",
            found.notices
        );
    }

    /// A listing that never ends stops at the page cap and says so, in the
    /// run's own fault list rather than only in a log line.
    ///
    /// Why (#6084, #5220): 5 000 repositories of a larger workspace read
    /// exactly like the whole workspace. The notice is the only thing that
    /// distinguishes them, and it is worth nothing until it reaches
    /// [`crate::collect::pr_pipeline`]'s drain.
    /// What: the mock's every page carries a `next` cursor back to itself, so
    /// the walk can only end at [`MAX_PAGES`]. The notice is then carried onto
    /// the pull-request client and drained exactly as a real run drains it.
    /// Test: this test itself.
    #[tokio::test]
    async fn a_truncated_workspace_listing_reaches_the_run_stats() {
        use crate::collect::collector::CollectionStats;
        use crate::collect::pr_provider::PrProvider;
        use crate::core::db::Database;
        use std::sync::Arc;

        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/repositories/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"full_name": "acme/widget", "slug": "widget"}],
                "next": format!("{base}/repositories/acme?cursor=endless"),
            })))
            .mount(&server)
            .await;

        let cfg = discovery_config(base, &["acme"]);
        let found = run_workspace_discovery(&cfg).await;
        assert_eq!(
            found.notices.len(),
            1,
            "one bound was hit, so exactly one notice: {:?}",
            found.notices
        );
        assert!(
            found.notices[0].contains("PARTIAL") && found.notices[0].contains("acme"),
            "the notice must name the workspace and say the set is partial: {:?}",
            found.notices
        );

        // The collector carries the notices onto the client that collects, and
        // `pr_pipeline` drains them into the run's faults. Drive that drain.
        let client = BitbucketClient::new_for_repos(&cfg, found.repos.clone())
            .expect("client builds")
            .with_notices(found.notices.clone());
        let providers: Vec<Arc<dyn PrProvider + Send + Sync>> = vec![Arc::new(client)];
        let mut set: tokio::task::JoinSet<(String, crate::collect::errors::Result<Vec<_>>)> =
            tokio::task::JoinSet::new();
        set.spawn(async { ("bitbucket".to_string(), Ok(Vec::new())) });

        let mut db = Database::open_in_memory().expect("open db");
        let mut stats = CollectionStats::default();
        crate::collect::pr_pipeline::drain_and_store_pull_requests(
            set, &providers, &mut db, &mut stats,
        )
        .await;

        assert!(
            stats
                .errors
                .iter()
                .any(|e| e.message.contains("PARTIAL") && e.message.contains("bitbucket")),
            "the truncation must be a recorded run fault: {:?}",
            stats.errors
        );
    }

    #[test]
    fn discovered_repos_union_with_the_configured_pair() {
        let cfg = BitbucketConfig {
            workspace: Some("acme".into()),
            repo_slug: Some("widget".into()),
            ..Default::default()
        };
        let discovered = vec![
            ("acme".to_string(), "widget".to_string()),
            ("acme".to_string(), "gadget".to_string()),
        ];
        assert_eq!(
            resolve_bitbucket_repos(&cfg, &discovered),
            vec![
                ("acme".to_string(), "widget".to_string()),
                ("acme".to_string(), "gadget".to_string()),
            ],
            "the explicit pair leads and is not duplicated by discovery"
        );

        let discovery_only = BitbucketConfig {
            workspaces: vec!["acme".into()],
            ..Default::default()
        };
        assert_eq!(
            resolve_bitbucket_repos(&discovery_only, &discovered),
            discovered,
            "with no explicit pair the discovered set stands alone"
        );
    }
}
