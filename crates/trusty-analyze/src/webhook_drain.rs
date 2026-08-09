//! Driving a held webhook delivery through the analysis pipeline (#5192).
//!
//! Why: #5182 gave `trusty-analyze` a UDS listener that takes durable ownership
//! of a relayed delivery and acknowledges it. Acknowledging is what lets
//! `trusty-console` delete its only copy, so from that moment the delivery is
//! this crate's responsibility — and until this module existed, nothing here
//! discharged it. A `pull_request` event reached a JSON file under
//! `~/…/trusty-analyze/webhook-inbox/` and stopped: no diff fetched, no review
//! run, no comment posted, and a console health signal that read green because
//! the hand-off had succeeded.
//!
//! What: [`AnalyzeProcessor`] is the [`DeliveryProcessor`] the listener drains
//! into. It runs exactly what the legacy HTTP route runs — the same filter, the
//! same [`run_pr_analysis`] — because two copies of "which actions are
//! actionable" is how the UDS path and the HTTP path start disagreeing about
//! what a webhook does. The HTTP handler in `service::handlers::review` calls
//! into here too, so retiring it ([#5181]) deletes a route and not a pipeline.
//!
//! 🔴 The retryable/permanent split is the load-bearing decision. A payload
//! with no `pull_request.number` can never succeed, so retrying it holds a slot
//! and a red health signal forever; a GitHub 503 or an unset `GITHUB_TOKEN`
//! can, so deleting or quarantining it on the first failure throws away a
//! review that would have run. Everything reachable over the network is
//! retryable and everything about the payload's shape is not.
//!
//! [#5181]: https://github.com/bobmatnyc/trusty-tools/issues/5181
//!
//! Test: `webhook_drain_tests.rs`.

use anyhow::Result;
use base64::Engine as _;
use trusty_common::webhook_relay::{DeliveryProcessor, Disposition, ProcessFailure, RelayDelivery};

use crate::core::TrustySearchClient;

/// GitHub event this crate acts on. Anything else is deliberately ignored.
pub const PR_EVENT: &str = "pull_request";

/// Actions that warrant a fresh analysis.
pub const ACTIONABLE_ACTIONS: [&str; 3] = ["opened", "synchronize", "reopened"];

/// The PR one actionable delivery points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrTarget {
    /// Repository owner login.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Pull request number.
    pub pr: u64,
    /// Head commit SHA, or `"unknown"` when the payload omits it.
    pub head_sha: String,
}

/// What one webhook payload turned out to be.
///
/// Why: three outcomes, and conflating any two of them is a defect. `Ignored`
/// is success (the delivery is done with), `Malformed` is permanent (retrying
/// cannot help), and only `Actionable` costs a GitHub round trip.
/// Test: `classify_accepts_an_actionable_pull_request`,
/// `classify_ignores_a_non_pull_request_event`,
/// `classify_ignores_an_unactionable_action`,
/// `classify_reports_a_payload_with_no_pr_number_as_malformed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrEventVerdict {
    /// Analyse this PR.
    Actionable(PrTarget),
    /// Nothing to do, and that is the correct outcome.
    Ignored(String),
    /// The payload cannot be acted on and never will be.
    Malformed(String),
}

/// Decide what a `{event, body}` pair asks for.
///
/// Why: the single copy of the filter, shared by the UDS drain and the legacy
/// HTTP route so the two cannot drift on which actions trigger an analysis.
/// Test: the four `classify_*` cases.
pub fn classify_pr_event(event: &str, body: &[u8]) -> PrEventVerdict {
    if event != PR_EVENT {
        return PrEventVerdict::Ignored(format!("event {event:?} is not {PR_EVENT:?}"));
    }
    let payload: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return PrEventVerdict::Malformed(format!("body is not valid JSON: {e}")),
    };
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if !ACTIONABLE_ACTIONS.contains(&action) {
        return PrEventVerdict::Ignored(format!("action {action:?} needs no analysis"));
    }

    let pr = payload
        .get("pull_request")
        .and_then(|p| p.get("number"))
        .and_then(serde_json::Value::as_u64);
    let owner = payload
        .get("repository")
        .and_then(|r| r.get("owner"))
        .and_then(|o| o.get("login"))
        .and_then(serde_json::Value::as_str);
    let repo = payload
        .get("repository")
        .and_then(|r| r.get("name"))
        .and_then(serde_json::Value::as_str);

    let (Some(pr), Some(owner), Some(repo)) = (pr, owner, repo) else {
        return PrEventVerdict::Malformed(
            "payload is missing pull_request.number or repository owner/name".to_string(),
        );
    };
    PrEventVerdict::Actionable(PrTarget {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr,
        head_sha: payload
            .get("pull_request")
            .and_then(|p| p.get("head"))
            .and_then(|h| h.get("sha"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

/// Fetch the PR diff, review it against trusty-search, and post the comment.
///
/// Why: the work a webhook exists to cause. Extracted from the HTTP handler's
/// spawned task so the UDS drain runs the identical pipeline rather than a
/// second implementation of it.
/// What: requires `GITHUB_TOKEN`; uses `repo` as the trusty-search index id
/// (the conventional 1:1 mapping). 30 s request / 5 s connect timeouts, matching
/// every other GitHub call in this crate — an untimed call here would leak the
/// drain task for the process's lifetime.
///
/// # Errors
///
/// A missing token, an unreachable GitHub or trusty-search, a malformed diff.
/// Every one of them is transient from the drain's point of view: the delivery
/// stays held and is retried.
///
/// Test: not unit-tested — it is entirely network I/O. The decision that
/// reaches it is `classify_pr_event`, which is.
pub async fn run_pr_analysis(search: &TrustySearchClient, target: &PrTarget) -> Result<()> {
    let PrTarget {
        owner,
        repo,
        pr,
        head_sha,
    } = target;
    let token = std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN)
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; cannot analyse webhook PR"))?;
    tracing::info!("processing webhook PR {owner}/{repo}#{pr} (head {head_sha})");
    let client = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest ClientBuilder is infallible with valid config");
    let diff = crate::core::fetch_pr_diff(&client, owner, repo, *pr, &token).await?;
    let report = crate::core::analyze_diff_with_client(&diff, search, repo).await?;
    let markdown = crate::core::format_review_as_markdown(&report);
    crate::core::post_pr_comment(&client, owner, repo, *pr, &markdown, &token).await?;
    tracing::info!("posted webhook review comment to {owner}/{repo}#{pr}");
    Ok(())
}

/// The analysis one actionable delivery causes.
///
/// Why: a seam, and specifically the seam a test needs. The drain's central
/// promise is that a delivery whose pipeline FAILS is kept, not counted and not
/// lost — and that is unprovable against a processor whose only failure mode is
/// an unreachable network, because "unreachable" on a developer's machine
/// depends on whether `GITHUB_TOKEN` happens to be set. Injecting the pipeline
/// makes the failure the test's choice rather than the environment's.
/// What: one method, the same one [`run_pr_analysis`] implements.
/// Test: `processor_reports_a_failed_pipeline_as_retryable`.
#[async_trait::async_trait]
pub trait PrPipeline: Send + Sync + 'static {
    /// Analyse `target`, or say why it could not be analysed.
    async fn analyse(&self, target: &PrTarget) -> Result<()>;
}

/// The real pipeline: GitHub in, trusty-search across, PR comment out.
#[derive(Clone)]
pub struct GithubPrPipeline {
    search: TrustySearchClient,
}

#[async_trait::async_trait]
impl PrPipeline for GithubPrPipeline {
    async fn analyse(&self, target: &PrTarget) -> Result<()> {
        run_pr_analysis(&self.search, target).await
    }
}

/// The drain's view of this crate's pipeline.
///
/// Why: what turns a held delivery into an analysis. Holding the search client
/// rather than building one per delivery keeps the connection pool the CLI
/// already configured.
/// What: decode the relayed body, classify it, and run the pipeline when it is
/// actionable.
/// Test: `webhook_drain_tests.rs` — `processor_*`.
#[derive(Clone)]
pub struct AnalyzeProcessor {
    pipeline: std::sync::Arc<dyn PrPipeline>,
}

impl std::fmt::Debug for AnalyzeProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalyzeProcessor").finish_non_exhaustive()
    }
}

impl AnalyzeProcessor {
    /// Build a processor that runs the real pipeline over `search`.
    pub fn new(search: TrustySearchClient) -> Self {
        Self::with_pipeline(std::sync::Arc::new(GithubPrPipeline { search }))
    }

    /// Build a processor over a supplied pipeline.
    pub fn with_pipeline(pipeline: std::sync::Arc<dyn PrPipeline>) -> Self {
        Self { pipeline }
    }
}

#[async_trait::async_trait]
impl DeliveryProcessor for AnalyzeProcessor {
    /// 🔴 The mapping from pipeline outcome to drain disposition, which is what
    /// decides whether a delivery is removed, retried, or quarantined.
    ///
    /// A body that is not valid base64 or not a decodable payload is
    /// [`ProcessFailure::permanent`]: the bytes are fixed, so no retry changes
    /// the answer, and the drain quarantines them where an operator can look at
    /// them. Everything else — token, network, search, GitHub — is
    /// [`ProcessFailure::retryable`], because the delivery is fine and the
    /// world is temporarily not.
    ///
    /// Test: `processor_ignores_a_non_pull_request_delivery`,
    /// `processor_reports_an_undecodable_body_as_permanent`,
    /// `processor_reports_a_failed_pipeline_as_retryable`,
    /// `processor_runs_the_pipeline_for_an_actionable_delivery`.
    async fn process(&self, delivery: &RelayDelivery) -> Result<Disposition, ProcessFailure> {
        let body = base64::engine::general_purpose::STANDARD
            .decode(&delivery.body_b64)
            .map_err(|e| {
                ProcessFailure::permanent(format!("relayed body is not valid base64: {e}"))
            })?;

        match classify_pr_event(&delivery.event, &body) {
            PrEventVerdict::Ignored(reason) => Ok(Disposition::Ignored { reason }),
            PrEventVerdict::Malformed(reason) => Err(ProcessFailure::permanent(reason)),
            PrEventVerdict::Actionable(target) => match self.pipeline.analyse(&target).await {
                Ok(()) => Ok(Disposition::Processed),
                Err(e) => Err(ProcessFailure::retryable(format!(
                    "analysing {}/{}#{} failed: {e:#}",
                    target.owner, target.repo, target.pr
                ))),
            },
        }
    }
}

#[cfg(test)]
#[path = "webhook_drain_tests.rs"]
mod tests;
