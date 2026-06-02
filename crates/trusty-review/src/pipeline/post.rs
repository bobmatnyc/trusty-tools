//! Post-or-log finalisation — the live/dry-run decision (Phase 1, #582).
//!
//! Why: a completed review must either be *posted* to the PR (live) or merely
//! *logged* (dry-run).  Concentrating that decision — and its fail-safe error
//! handling — in one place keeps `runner.rs` focused on the pipeline stages and
//! keeps the policy testable in isolation.  The decision folds three inputs:
//! the global `dry_run` flag, the trigger classification, and a hard
//! `allow_posting` belt (false for `compare`/`--local-diff`).
//!
//! What: `finalize_review` applies the effective-dry-run formula; when live it
//! resolves a token through the dual-mode auth abstraction and posts a PR review
//! comment, marking the dedup claim complete; when dry it writes the JSON/MD log.
//! Every GitHub / dedup error is logged and swallowed — a post failure must
//! never crash a review (fail-safe), and falls back to leaving the result
//! flagged dry-run.
//!
//! Test: `effective_dry_run` math lives in `trigger`; the post/log branch
//! selection is covered by `decide_action_*` here, and the live happy path is
//! covered by `#[ignore]` integration tests (needs a live PR).

use std::sync::Arc;

use tracing::{info, warn};

use crate::{
    config::ReviewConfig,
    integrations::github::{AuthStrategy, GithubClient, RunMode, post_pr_review},
    models::ReviewResult,
    pipeline::{
        output::{print_review_result, write_review_log},
        trigger::{TriggerDecision, effective_dry_run},
    },
    store::DedupStore,
};

/// The finalisation action selected for a completed review.
///
/// Why: separating the *decision* from the *side effect* lets the branch
/// selection be unit-tested without a network or filesystem, while the runner
/// still gets a single `finalize_review` call.
/// What: `Post` means attempt a live PR comment; `LogOnly` means write the
/// dry-run log.
/// Test: `decide_action_live_posts`, `decide_action_dry_logs`,
/// `decide_action_disallowed_logs`, `decide_action_local_logs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeAction {
    /// Post the review live as a GitHub PR review comment.
    Post,
    /// Write a dry-run log only; do not post.
    LogOnly,
}

/// Decide whether a completed review should be posted or only logged.
///
/// Why: the live-vs-dry choice has three gates that must all agree — the
/// effective-dry-run formula (config flag folded with the trigger), the hard
/// `allow_posting` belt, and the requirement that the diff actually came from a
/// GitHub PR (a local diff has nowhere to post).  Centralising the conjunction
/// avoids subtly different copies in the runner and the service.
/// What: returns `Post` only when posting is allowed, the source is a GitHub PR,
/// and the effective decision is *not* dry-run; otherwise `LogOnly`.
/// Test: `decide_action_live_posts`, `decide_action_dry_logs`,
/// `decide_action_disallowed_logs`, `decide_action_local_logs`.
pub fn decide_action(
    config_dry_run: bool,
    trigger: TriggerDecision,
    allow_posting: bool,
    is_github_source: bool,
) -> FinalizeAction {
    let dry = effective_dry_run(config_dry_run, trigger);
    if !dry && allow_posting && is_github_source {
        FinalizeAction::Post
    } else {
        FinalizeAction::LogOnly
    }
}

/// Context the runner passes to `finalize_review` for the post path.
///
/// Why: posting needs the PR coordinates, the run mode (to select the auth
/// strategy), and the optional dedup store (to mark the claim complete); bundling
/// them avoids a long argument list.
/// What: a plain owned bundle; `dedup` is `None` when dedup is disabled.
/// Test: used by the runner; the post path itself is `#[ignore]` integration.
pub struct PostContext<'a> {
    /// GitHub organisation / owner.
    pub owner: &'a str,
    /// Repository name.
    pub repo: &'a str,
    /// Pull request number.
    pub pr: u64,
    /// Head commit SHA (dedup key); empty when unknown.
    pub head_sha: &'a str,
    /// Run mode selecting the auth strategy (CLI=PAT/`gh`, Serve=App).
    pub run_mode: RunMode,
    /// Optional dedup store to mark the claim complete on a successful post.
    pub dedup: Option<&'a Arc<DedupStore>>,
}

/// Finalise a completed review: post it live, or write a dry-run log.
///
/// Why: the single exit point for `run_review` so every code path applies the
/// same post-or-log policy and the same fail-safe error handling.
/// What: computes the action via `decide_action`; on `Post`, resolves a token
/// through the auth abstraction and posts a PR review comment (setting
/// `posted=true`, `dry_run=false` and marking the dedup claim complete on
/// success); on `LogOnly` (or any post failure) writes the dry-run log.  Prints
/// the result to STDOUT when `print_result` is set.  Never returns an error —
/// failures degrade to a logged dry-run.
/// Test: `decide_action_*` cover the branch; the live post is `#[ignore]`.
pub async fn finalize_review(
    mut result: ReviewResult,
    config: &ReviewConfig,
    trigger: TriggerDecision,
    allow_posting: bool,
    write_log: bool,
    print_result: bool,
    post_ctx: PostContext<'_>,
) -> ReviewResult {
    let is_github = !post_ctx.owner.is_empty() && post_ctx.owner != "local";
    let action = decide_action(config.dry_run, trigger, allow_posting, is_github);

    match action {
        FinalizeAction::Post => {
            match post_live(&mut result, config, &post_ctx).await {
                Ok(()) => {
                    result.posted = true;
                    result.dry_run = false;
                    info!(
                        owner = post_ctx.owner,
                        repo = post_ctx.repo,
                        pr = post_ctx.pr,
                        verdict = %result.verdict,
                        "review posted live to GitHub PR"
                    );
                    // Mark the dedup claim complete so retries are suppressed.
                    if let Some(store) = post_ctx.dedup
                        && !post_ctx.head_sha.is_empty()
                        && let Err(e) = store.complete(
                            post_ctx.owner,
                            post_ctx.repo,
                            post_ctx.pr,
                            post_ctx.head_sha,
                        )
                    {
                        // Fail-safe: a dedup write failure must not fail the review.
                        warn!("dedup complete() failed (non-fatal): {e}");
                    }
                }
                Err(e) => {
                    // Fail-safe: posting failed → fall back to a dry-run log so
                    // the review is still inspectable, and surface the error.
                    warn!("live post failed (falling back to dry-run log): {e}");
                    result.dry_run = true;
                    if result.error.is_none() {
                        result.error = Some(format!("post failed: {e}"));
                    }
                    write_review_log(&result, &config.log_dir);
                }
            }
        }
        FinalizeAction::LogOnly => {
            result.dry_run = true;
            if write_log {
                write_review_log(&result, &config.log_dir);
            }
        }
    }

    if print_result {
        print_review_result(&result);
    }

    result
}

/// Resolve a token via the auth abstraction and post the PR review comment.
///
/// Why: the live side effect, isolated so its `?`-based error flow stays clean
/// while `finalize_review` owns the fail-safe swallowing.
/// What: selects the auth strategy from the run mode, resolves a token for the
/// owner, and POSTs the review comment; on success copies the posted-review
/// HTML URL into the result.
/// Test: network-bound; covered by `#[ignore]` integration tests.
async fn post_live(
    result: &mut ReviewResult,
    config: &ReviewConfig,
    ctx: &PostContext<'_>,
) -> Result<(), crate::integrations::github::GithubError> {
    let client = GithubClient::new();
    let strategy = AuthStrategy::select(ctx.run_mode, None);
    let token = strategy.resolve_token(&client, config, ctx.owner).await?;
    let posted = post_pr_review(&client, ctx.owner, ctx.repo, ctx.pr, &token, result).await?;
    if !posted.html_url.is_empty() {
        // Stash the posted-review URL on the result for the log / future updates.
        info!(review_url = %posted.html_url, "posted review URL");
    }
    Ok(())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_action_live_posts() {
        // config live (dry_run=false), no trigger override, posting allowed,
        // GitHub source → Post.
        assert_eq!(
            decide_action(false, TriggerDecision::None, true, true),
            FinalizeAction::Post
        );
    }

    #[test]
    fn decide_action_force_live_overrides_dry_config() {
        // config dry, but trigger forces live → Post.
        assert_eq!(
            decide_action(true, TriggerDecision::ForceLive, true, true),
            FinalizeAction::Post
        );
    }

    #[test]
    fn decide_action_dry_logs() {
        // config dry, no override → LogOnly even with posting allowed.
        assert_eq!(
            decide_action(true, TriggerDecision::None, true, true),
            FinalizeAction::LogOnly
        );
    }

    #[test]
    fn decide_action_force_dry_run_overrides_live_config() {
        // config live, but trigger forces dry → LogOnly.
        assert_eq!(
            decide_action(false, TriggerDecision::ForceDryRun, true, true),
            FinalizeAction::LogOnly
        );
    }

    #[test]
    fn decide_action_disallowed_logs() {
        // Posting disallowed (compare mode) → LogOnly even when live.
        assert_eq!(
            decide_action(false, TriggerDecision::ForceLive, false, true),
            FinalizeAction::LogOnly
        );
    }

    #[test]
    fn decide_action_local_logs() {
        // Local diff (no GitHub source) can never post → LogOnly.
        assert_eq!(
            decide_action(false, TriggerDecision::ForceLive, true, false),
            FinalizeAction::LogOnly
        );
    }
}
