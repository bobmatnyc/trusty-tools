//! Driving a held webhook delivery through the review pipeline (#5192).
//!
//! Why: #5182 gave `trusty-review` a UDS listener that takes durable ownership
//! of a relayed delivery and acknowledges it. The ack is what lets
//! `trusty-console` delete its only copy, so from that moment the delivery is
//! this crate's responsibility — and nothing here discharged it. A
//! `review_requested` event reached a JSON file under
//! `~/…/trusty-review/webhook-inbox/` and stopped: no review, no comment, and a
//! console health signal that read green because the hand-off had succeeded.
//!
//! What: [`ReviewProcessor`] is the [`DeliveryProcessor`] the listener drains
//! into. [`classify_review_event`] is the single copy of the action filter,
//! shared with the legacy HTTP route so the two paths cannot disagree about
//! which deliveries cause a review. [`ConfiguredReviewPipeline`] builds the LLM,
//! verifier, search, analyze and dedup dependencies **lazily**, on the first
//! actionable delivery — a listener that is spawned and never given work must
//! not pay for a Bedrock liveness probe.
//!
//! 🔴 Two decisions carry the correctness of this module.
//!
//! * **`DedupNeed::Required`.** The drain runs with `allow_posting: true`, so
//!   without the durable claim gate a redelivery — which the relay contract
//!   explicitly permits ([`RelayDelivery::attempts`]) — posts a second review
//!   comment on the same PR. A locked or unopenable store is therefore a
//!   retryable failure, never a downgrade to `dedup: None` (#5064).
//! * **The retryable/permanent split.** A body that is not decodable JSON can
//!   never succeed, so it is quarantined at once rather than retried against
//!   bytes that will not change. Everything reachable over the network — the
//!   provider, GitHub, trusty-search — is retryable, so a delivery is kept while
//!   the world is temporarily broken.
//!
//! Outcome polling on `closed` + `merged` (opt-in, default off) is deliberately
//! NOT run here: it schedules a task that sleeps for an hour, which a
//! console-supervised short-lived process cannot honour. Those deliveries are
//! [`Disposition::Ignored`] with the reason recorded. The legacy HTTP route
//! still schedules them; giving the drain a durable equivalent is separate work
//! and is called out in ADR-0034 §5.
//!
//! Test: `webhook_drain_tests.rs`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use base64::Engine as _;
use trusty_common::webhook_relay::{DeliveryProcessor, Disposition, ProcessFailure, RelayDelivery};

use crate::config::{InvocationSurface, ReviewConfig};
use crate::integrations::github::RunMode;
use crate::pipeline::{
    DiffSource, ReviewDeps, ReviewInput, classify_review_request, enforce_verifier_liveness,
    run_review,
};
use crate::store::DedupNeed;

/// GitHub event this crate acts on.
pub const PR_EVENT: &str = "pull_request";

/// The only action that dispatches a review (spec REV-702).
pub const REVIEW_ACTION: &str = "review_requested";

/// The PR one actionable delivery points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewTarget {
    /// Repository owner login.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Pull request number.
    pub pr: u64,
    /// Head commit SHA, empty when the payload omits it.
    pub head_sha: String,
    /// Requested reviewer login, which decides force-live vs force-dry-run
    /// (REV-703).
    pub requested_reviewer: Option<String>,
}

/// What one webhook payload turned out to be.
///
/// Test: `classify_accepts_a_review_request`,
/// `classify_ignores_a_non_pull_request_event`,
/// `classify_ignores_an_action_that_is_not_review_requested`,
/// `classify_reports_a_payload_with_no_pr_number_as_malformed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewEventVerdict {
    /// Review this PR.
    Actionable(ReviewTarget),
    /// Nothing to do, and that is the correct outcome.
    Ignored(String),
    /// The payload cannot be acted on and never will be.
    Malformed(String),
}

/// Decide what a `{event, body}` pair asks for.
///
/// Why: the single copy of the filter. `service::webhook` calls it too, so
/// retiring that route ([#5181]) removes a transport and not a policy.
///
/// [#5181]: https://github.com/bobmatnyc/trusty-tools/issues/5181
///
/// Test: the four `classify_*` cases.
pub fn classify_review_event(event: &str, body: &[u8]) -> ReviewEventVerdict {
    if event != PR_EVENT {
        return ReviewEventVerdict::Ignored(format!("event {event:?} is not {PR_EVENT:?}"));
    }
    let payload: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return ReviewEventVerdict::Malformed(format!("body is not valid JSON: {e}")),
    };
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if action != REVIEW_ACTION {
        return ReviewEventVerdict::Ignored(format!("action {action:?} is not {REVIEW_ACTION:?}"));
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
        return ReviewEventVerdict::Malformed(
            "payload is missing pull_request.number or repository owner/name".to_string(),
        );
    };
    ReviewEventVerdict::Actionable(ReviewTarget {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr,
        head_sha: payload
            .get("pull_request")
            .and_then(|p| p.get("head"))
            .and_then(|h| h.get("sha"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        requested_reviewer: payload
            .get("requested_reviewer")
            .and_then(|r| r.get("login"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

/// The review one actionable delivery causes.
///
/// Why: the seam a test needs. "A delivery whose pipeline fails is kept, not
/// counted and not lost" is unprovable against a processor whose only failure
/// mode is a network call, because whether that call fails depends on the
/// machine the suite runs on.
/// Test: `processor_reports_a_failed_pipeline_as_retryable`.
#[async_trait::async_trait]
pub trait ReviewPipeline: Send + Sync + 'static {
    /// Review `target`, or say why it could not be reviewed.
    async fn review(&self, target: &ReviewTarget) -> Result<()>;
}

/// The real pipeline, with its dependencies built on first use.
///
/// Why lazy: console spawns this process on a delivery and keeps it warm; a
/// spawn that never sees actionable work must not pay for a provider build and
/// a verifier liveness probe. Why `OnceCell` rather than rebuilding per
/// delivery: the dedup store takes an exclusive lock, so a second open inside
/// one process would fail.
pub struct ConfiguredReviewPipeline {
    config: ReviewConfig,
    built: tokio::sync::OnceCell<Built>,
}

/// The config as resolved at build time, plus the deps built against it.
struct Built {
    config: ReviewConfig,
    deps: ReviewDeps,
}

impl ConfiguredReviewPipeline {
    /// Build a pipeline that will resolve its dependencies on first use.
    pub fn new(config: ReviewConfig) -> Self {
        Self {
            config,
            built: tokio::sync::OnceCell::new(),
        }
    }

    /// Resolve the dependencies, once.
    ///
    /// # Errors
    ///
    /// A provider build failure, a failed verifier liveness gate, or a dedup
    /// store that cannot be opened. Each leaves the delivery held and retried.
    async fn built(&self) -> Result<&Built> {
        self.built
            .get_or_try_init(|| build_deps(self.config.clone()))
            .await
    }
}

/// Build everything `run_review` needs, the way `serve` builds it.
///
/// Mirrors `commands::serve::build_app_state` deliberately: the drain runs the
/// same pipeline the HTTP webhook route ran, so it must not run it with weaker
/// dependencies. In particular `DedupNeed::Required` — see the module docs.
async fn build_deps(mut config: ReviewConfig) -> Result<Built> {
    let reviewer_model = config.role_models.reviewer.model.clone();
    let default_provider = config.role_models.reviewer.provider.clone();
    let llm = crate::llm::build_provider(&reviewer_model, &default_provider, &config)
        .await
        .map_err(|e| anyhow::anyhow!("build the reviewer LLM provider: {e}"))?;

    let verifier = crate::llm::build_verifier_required(&config).await?;
    enforce_verifier_liveness(&config, verifier.as_ref())
        .await
        .map_err(|reason| anyhow::anyhow!(reason))?;

    let search = crate::integrations::search_client::HttpSearchClient::from_config(&config)
        .map_err(|e| anyhow::anyhow!("build the search client: {e}"))?;
    config.resolve_index(&search).await;
    let analyze =
        crate::integrations::subprocess_analyze_client::SubprocessAnalyzeClient::from_config(
            &config,
        )
        .map_err(|e| anyhow::anyhow!("build the analyze client: {e}"))?;

    // #5064 / #5192: the drain posts, so the claim gate is mandatory. A store
    // that cannot be opened is an error here and is retried, never a silent
    // `dedup: None` that lets a redelivery comment twice.
    let dedup = crate::store::open_dedup_for(&config.log_dir, DedupNeed::Required)
        .context("open the dedup store for the webhook drain")?;

    Ok(Built {
        config,
        deps: ReviewDeps {
            llm,
            verifier,
            search: Arc::new(search),
            analyze: Some(Arc::new(analyze)),
            dedup,
        },
    })
}

#[async_trait::async_trait]
impl ReviewPipeline for ConfiguredReviewPipeline {
    async fn review(&self, target: &ReviewTarget) -> Result<()> {
        let built = self.built().await?;
        let config = &built.config;
        let trigger = classify_review_request(config, target.requested_reviewer.as_deref());
        let input = ReviewInput {
            diff_source: DiffSource::Github {
                owner: target.owner.clone(),
                repo: target.repo.clone(),
                pr: target.pr,
                // Resolved by `run_review` via `resolve_diff_token` (#1880).
                token: String::new(),
            },
            reviewer_model: config.role_models.reviewer.model.clone(),
            write_log: true,
            print_result: false,
            trigger,
            run_mode: RunMode::Serve,
            allow_posting: true,
            caller_context: crate::pipeline::runner::CallerContext::default(),
            // The hosted webhook bot CAN post to a real PR, so it keeps the
            // strict `Hosted` default and never silently degrades (REV-011).
            surface: InvocationSurface::Hosted,
        };

        let result = run_review(config, input, built.deps.clone()).await;
        if let Some(err) = result.error {
            anyhow::bail!(err);
        }
        tracing::info!(
            pr = target.pr,
            verdict = %result.verdict,
            posted = result.posted,
            findings = result.findings.len(),
            "webhook drain review complete"
        );
        Ok(())
    }
}

/// The drain's view of this crate's pipeline.
///
/// Test: `webhook_drain_tests.rs` — `processor_*`.
#[derive(Clone)]
pub struct ReviewProcessor {
    pipeline: Arc<dyn ReviewPipeline>,
}

impl std::fmt::Debug for ReviewProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewProcessor").finish_non_exhaustive()
    }
}

impl ReviewProcessor {
    /// Build a processor that runs the real pipeline against `config`.
    pub fn new(config: ReviewConfig) -> Self {
        Self::with_pipeline(Arc::new(ConfiguredReviewPipeline::new(config)))
    }

    /// Build a processor over a supplied pipeline.
    pub fn with_pipeline(pipeline: Arc<dyn ReviewPipeline>) -> Self {
        Self { pipeline }
    }
}

#[async_trait::async_trait]
impl DeliveryProcessor for ReviewProcessor {
    /// Map a pipeline outcome onto a drain disposition — which is what decides
    /// whether the delivery is removed, retried, or quarantined.
    ///
    /// Test: `processor_ignores_a_non_review_request`,
    /// `processor_reports_an_undecodable_body_as_permanent`,
    /// `processor_reports_a_failed_pipeline_as_retryable`,
    /// `processor_runs_the_pipeline_for_a_review_request`.
    async fn process(&self, delivery: &RelayDelivery) -> Result<Disposition, ProcessFailure> {
        let body = base64::engine::general_purpose::STANDARD
            .decode(&delivery.body_b64)
            .map_err(|e| {
                ProcessFailure::permanent(format!("relayed body is not valid base64: {e}"))
            })?;

        match classify_review_event(&delivery.event, &body) {
            ReviewEventVerdict::Ignored(reason) => Ok(Disposition::Ignored { reason }),
            ReviewEventVerdict::Malformed(reason) => Err(ProcessFailure::permanent(reason)),
            ReviewEventVerdict::Actionable(target) => match self.pipeline.review(&target).await {
                Ok(()) => Ok(Disposition::Processed),
                Err(e) => Err(ProcessFailure::retryable(format!(
                    "reviewing {}/{}#{} failed: {e:#}",
                    target.owner, target.repo, target.pr
                ))),
            },
        }
    }
}

#[cfg(test)]
#[path = "webhook_drain_tests.rs"]
mod tests;
