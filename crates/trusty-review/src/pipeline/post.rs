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
//! `format_review_footer` renders the compact metadata footer appended to
//! `review_body` before posting or returning — ensuring the footer is identical
//! in the GitHub comment and the dry-run/MCP response (closes #728).
//!
//! Test: `effective_dry_run` math lives in `trigger`; the post/log branch
//! selection is covered by `decide_action_*` here; `format_review_footer` is
//! covered by `footer_format_known_tuple` and `footer_thousands_separator`; the
//! live happy path is covered by `#[ignore]` integration tests (needs a live PR).

use std::sync::Arc;

use tracing::{error, info, warn};

use crate::{
    config::ReviewConfig,
    integrations::github::{AuthStrategy, GithubClient, RunMode, post_pr_review},
    models::{Finding, ReviewResult},
    pipeline::{
        output::{print_review_result, write_review_log},
        trigger::{TriggerDecision, effective_dry_run},
    },
    store::DedupStore,
};

// ─── Metadata footer (closes #728) ───────────────────────────────────────────

/// Render the compact metadata footer appended to every completed review body.
///
/// Why: the GitHub PR comment and the MCP `review_pr` structured response both
/// expose `review_body`, but neither previously recorded which model produced the
/// review or how much it cost — making reviews hard to audit at a glance.
/// Appending the footer here (before the post/log branch) ensures a single source
/// of truth: the same footer text appears in the live GitHub comment AND in the
/// dry-run / MCP response, so callers see exactly what was (or would be) posted
/// (closes #728, #732).
/// What: formats one line `[Grade: <g> · ]🤖 Reviewed by Trusty-Review (\`<model>\`) · tokens ↑<in> ↓<out> · est. $<cost>`
/// where the grade prefix is included when `grade` is `Some`, token counts use
/// thousands separators, and cost is rounded to 3 decimal places (e.g. `$0.066`).
/// An empty model string is rendered as `(unknown)` so the line is always well-formed.
/// Test: `footer_format_known_tuple` (exact-string regression for the sample
/// tuple from #728), `footer_format_with_grade` (grade-prefixed form from #732),
/// `footer_thousands_separator` (boundary at 1 000).
pub fn format_review_footer(
    grade: Option<&str>,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
) -> String {
    let model_display = if model.is_empty() {
        "(unknown)".to_string()
    } else {
        model.to_string()
    };
    // Format token counts with locale-style thousands separators (groups of 3).
    let in_fmt = format_with_thousands(input_tokens);
    let out_fmt = format_with_thousands(output_tokens);
    // Round cost to 3 decimal places; strip trailing zeros after the 3rd digit.
    let cost_fmt = format_cost(cost_usd);
    let grade_prefix = match grade {
        Some(g) if !g.is_empty() => format!("Grade: {g} · "),
        _ => String::new(),
    };
    format!(
        "\n---\n{grade_prefix}🤖 Reviewed by Trusty-Review (`{model_display}`) · tokens ↑{in_fmt} ↓{out_fmt} · est. ${cost_fmt}"
    )
}

/// Format a `u32` integer with comma thousands separators.
///
/// Why: token counts in the footer must be human-readable (e.g. `13,499`);
/// Rust's standard library does not provide locale-aware formatting.
/// What: splits the decimal representation into groups of three from the right,
/// joining them with commas.
/// Test: `footer_thousands_separator`.
fn format_with_thousands(n: u32) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

/// Format a cost in USD to a compact, sensibly-rounded string.
///
/// Why: `f64` default Display produces too many or too few digits (e.g.
/// `0.06626699999` or `0.1`); the footer needs a fixed-width, readable form.
/// What: renders at 3 decimal places, then strips trailing zeros so `$0.100`
/// becomes `$0.1` but `$0.066` stays `$0.066`.  A zero cost renders as `$0`.
/// Test: covered transitively by `footer_format_known_tuple`.
fn format_cost(cost_usd: f64) -> String {
    if cost_usd == 0.0 {
        return "0".to_string();
    }
    // 3 decimal places covers sub-cent precision without excessive noise.
    let raw = format!("{cost_usd:.3}");
    // Strip trailing zeros after the decimal point, but keep at least one digit.
    let trimmed = raw.trim_end_matches('0');
    // If we stripped all fractional digits, keep the decimal point + one zero.
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_string()
    }
}

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

/// Count the findings nothing rendered a judgment on (#4459).
///
/// Why: both canonical exits — `finalize_review` here and `abort_dry` in
/// `runner_helpers` — owe the caller the same number, and two copies of the
/// predicate would drift the moment a `VerifyOutcome` variant is added.
/// What: counts findings whose recorded outcome answers true to
/// `VerifyOutcome::is_unverified`. A finding with no outcome at all is NOT
/// counted — it was never a verification candidate, the same state as
/// `Skipped`.
/// Test: `unverified_count_matches_the_unverified_findings`.
pub(crate) fn count_unverified(findings: &[Finding]) -> usize {
    findings
        .iter()
        .filter(|f| f.verified.as_ref().is_some_and(|v| v.is_unverified()))
        .count()
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
    /// Head commit SHA (dedup key). Empty when the PR-metadata fetch failed —
    /// `finalize_review` then refuses to post, because an unkeyed claim cannot
    /// be completed and a re-run would duplicate the comment (#6062).
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
/// What: computes the action via `decide_action`, then downgrades `Post` to
/// `LogOnly` when the context carries no head SHA — the dedup claim is keyed on
/// it, so posting unkeyed leaves the run with no defence against a duplicate on
/// re-run (#6062). On `Post`, resolves a token
/// through the auth abstraction and posts a PR review comment (setting
/// `posted=true`, `dry_run=false` and marking the dedup claim complete on
/// success); on `LogOnly` (or any post failure) writes the dry-run log.  Prints
/// the result to STDOUT when `print_result` is set.  Never returns an error —
/// failures degrade to a logged dry-run.  Also syncs `findings_count` to
/// `findings.len()` (#1877) so every completed review (unified or map-reduce)
/// carries the authoritative count regardless of exit path.
/// Test: `decide_action_*` cover the branch; the live post is `#[ignore]`;
/// `findings_count_matches_len_on_completed_review`,
/// `finalize_review_does_not_post_without_a_head_sha`.
pub async fn finalize_review(
    mut result: ReviewResult,
    config: &ReviewConfig,
    trigger: TriggerDecision,
    allow_posting: bool,
    write_log: bool,
    print_result: bool,
    post_ctx: PostContext<'_>,
) -> ReviewResult {
    // #1877: keep the authoritative findings_count in sync at this canonical
    // completed-review exit point (covers both the unified and map-reduce
    // paths, which both funnel through `finalize_run` → here).
    result.findings_count = result.findings.len();
    // #4459: same rationale one field over — report how many findings nothing
    // verified, so a review whose verifier was unreachable does not read clean.
    result.unverified_count = count_unverified(&result.findings);

    // #4044: the narrative summary was written BEFORE the verification round on
    // both paths, so it can still cite a finding the verifier refuted as a merge
    // blocker.  Prepend the correction here — the one exit both paths funnel
    // through — so the qualifier reaches the posted comment and the returned
    // result alike.  No-op when nothing was refuted.
    result.review_body = crate::pipeline::verification_notice::prepend_verification_notice(
        &result.review_body,
        &result.findings,
    );

    // Append the metadata footer to review_body BEFORE the post/log branch so
    // the footer is identical in the live GitHub comment (which reads
    // result.review_body via build_review_comment_body) and in the returned
    // ReviewResult (which the MCP wrapper serialises as structured output).
    // This is the single source of truth required by #728 (token/cost) and
    // #732 (grade prefix). build_review_comment_body in posting.rs must NOT
    // generate its own footer — it should render result.review_body which
    // already carries this footer line.
    let footer = format_review_footer(
        result.grade.as_deref(),
        &result.model,
        result.input_tokens,
        result.output_tokens,
        result.cost_estimate_usd,
    );
    result.review_body.push_str(&footer);

    let is_github = !post_ctx.owner.is_empty() && post_ctx.owner != "local";
    let mut action = decide_action(config.dry_run, trigger, allow_posting, is_github);

    // #6062: empty head_sha is fail-closed — no claim, no post
    if action == FinalizeAction::Post && post_ctx.head_sha.is_empty() {
        error!(
            owner = post_ctx.owner,
            repo = post_ctx.repo,
            pr = post_ctx.pr,
            "no head SHA to key the dedup claim — logging instead of posting (#6062)"
        );
        if result.error.is_none() {
            result.error = Some(
                "not posted: no head SHA, so the dedup claim cannot be completed and a \
                 re-run could post a duplicate comment (#6062)"
                    .to_string(),
            );
        }
        action = FinalizeAction::LogOnly;
    }

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
                        && let Err(e) = store
                            .complete(
                                post_ctx.owner,
                                post_ctx.repo,
                                post_ctx.pr,
                                post_ctx.head_sha,
                            )
                            .await
                    {
                        // #5064: the comment is already posted, so there is
                        // nothing left to fail closed on. The InProgress claim
                        // still suppresses redelivery until it ages out, but an
                        // operator needs to see this, so it is an error.
                        error!("dedup complete() failed after a live post: {e}");
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
    let client = GithubClient::new()?;
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
    use crate::models::{Effort, VerifyOutcome};

    /// #4459: the count reports every finding nothing rendered a judgment on,
    /// and nothing else — a judged finding and one that was never a candidate
    /// both stay out of it.
    #[test]
    fn unverified_count_matches_the_unverified_findings() {
        let mk = |outcome: Option<VerifyOutcome>| {
            let mut f = Finding::new("src/a.rs", "logic", "a bug", "fix it", 0.9, Effort::Medium);
            f.verified = outcome;
            f
        };
        let findings = vec![
            mk(Some(VerifyOutcome::Confirmed)),
            mk(Some(VerifyOutcome::Refuted)),
            mk(Some(VerifyOutcome::Skipped)),
            mk(None),
            mk(Some(VerifyOutcome::ErrorRefuted {
                error_class: "AccessDenied".to_string(),
            })),
            mk(Some(VerifyOutcome::TruncationRefuted)),
            mk(Some(VerifyOutcome::Unverifiable {
                reason: "unreachable after 3 attempt(s)".to_string(),
            })),
        ];
        assert_eq!(count_unverified(&findings), 3);
        assert_eq!(count_unverified(&[]), 0);
    }

    // ── #6062: an empty head SHA is fail-closed ───────────────────────────────

    /// REGRESSION (#6062): `finalize_review` never posts a review it cannot key
    /// a dedup claim to.
    ///
    /// Why: `decide_action` reads only `is_github`, `allow_posting`, `trigger`
    /// and `dry_run`, so a GitHub run whose PR-metadata fetch failed reached
    /// `FinalizeAction::Post` with an empty `head_sha`; the `complete()` call is
    /// itself gated on `!head_sha.is_empty()`, so the comment posted with the
    /// claim never taken and never completed.
    /// What: a GitHub `PostContext` with an empty `head_sha`, `allow_posting`
    /// true and `dry_run` false — the exact shape that selects `Post`. The
    /// result must stay unposted and say why.
    /// Test: this test. Fails pre-fix: `post_live` ran and the error read
    /// `post failed: …` rather than naming the missing head SHA.
    #[tokio::test]
    async fn finalize_review_does_not_post_without_a_head_sha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = ReviewConfig::load(None);
        config.dry_run = false;
        config.log_dir = dir.path().to_path_buf();

        let result = ReviewResult::new("acme", "backend", 42, "t", "u");
        let out = finalize_review(
            result,
            &config,
            TriggerDecision::None,
            true,
            false,
            false,
            PostContext {
                owner: "acme",
                repo: "backend",
                pr: 42,
                head_sha: "",
                run_mode: RunMode::Serve,
                dedup: None,
            },
        )
        .await;

        assert!(!out.posted, "an unkeyed review must never post");
        assert!(out.dry_run, "the fail-closed path stays dry-run");
        let error = out.error.expect("the downgrade must explain itself");
        assert!(
            error.contains("head SHA"),
            "the error must name the missing dedup key: {error}"
        );
    }

    // ── Footer rendering ──────────────────────────────────────────────────────

    /// Exact-string regression for the no-grade form from issue #728.
    ///
    /// Why: the footer format is a user-facing string that must not drift
    /// silently; an exact assertion catches any change to separators, arrows,
    /// or emoji.
    /// What: asserts the full footer line for grade=None,
    ///   model=us.anthropic.claude-sonnet-4-6, in=13499, out=1718, cost=0.066267.
    /// Test: this test itself (no network, no FS).
    #[test]
    fn footer_format_known_tuple() {
        let footer = format_review_footer(
            None,
            "us.anthropic.claude-sonnet-4-6",
            13499,
            1718,
            0.066_267,
        );
        assert_eq!(
            footer,
            "\n---\n🤖 Reviewed by Trusty-Review (`us.anthropic.claude-sonnet-4-6`) · tokens ↑13,499 ↓1,718 · est. $0.066"
        );
    }

    /// Exact-string regression for the grade-prefixed form from issue #732.
    ///
    /// Why: the consolidated footer must prepend the grade when present, using
    /// the same thousands separators and cost rounding as the #728 no-grade form.
    /// Any format drift is caught immediately by this exact assertion.
    /// What: asserts the full footer line for grade=B+,
    ///   model=us.anthropic.claude-sonnet-4-6, in=13499, out=1718, cost=0.066267
    ///   → `Grade: B+ · 🤖 Reviewed by Trusty-Review (\`us.anthropic.claude-sonnet-4-6\`) · tokens ↑13,499 ↓1,718 · est. $0.066`
    /// Test: this test itself (no network, no FS).
    #[test]
    fn footer_format_with_grade() {
        let footer = format_review_footer(
            Some("B+"),
            "us.anthropic.claude-sonnet-4-6",
            13499,
            1718,
            0.066_267,
        );
        assert_eq!(
            footer,
            "\n---\nGrade: B+ · 🤖 Reviewed by Trusty-Review (`us.anthropic.claude-sonnet-4-6`) · tokens ↑13,499 ↓1,718 · est. $0.066"
        );
    }

    /// Verify thousands-separator boundary at exactly 1 000.
    ///
    /// Why: the formatter uses modular arithmetic; 1 000 is the smallest value
    /// that triggers a separator and is easy to verify manually.
    /// What: asserts `1000` → `"1,000"` and `999` → `"999"`.
    /// Test: this test itself.
    #[test]
    fn footer_thousands_separator() {
        assert_eq!(format_with_thousands(1000), "1,000");
        assert_eq!(format_with_thousands(999), "999");
        assert_eq!(format_with_thousands(1_000_000), "1,000,000");
        assert_eq!(format_with_thousands(0), "0");
    }

    /// Empty model slug renders as `(unknown)`.
    ///
    /// Why: early-abort paths (fail-safe APPROVE) may return before the LLM
    /// model field is filled in; the footer must still be well-formed.
    /// What: asserts the footer contains `(unknown)` when model is empty.
    /// Test: this test itself.
    #[test]
    fn footer_empty_model_renders_unknown() {
        let footer = format_review_footer(None, "", 10, 5, 0.001);
        assert!(
            footer.contains("`(unknown)`"),
            "empty model must render as (unknown): {footer}"
        );
    }

    /// Zero cost renders as `$0` (not `$0.000`).
    ///
    /// Why: some providers (e.g. local models, tests) report zero cost; the
    /// trailing-zero strip must produce a clean `$0`.
    /// What: asserts cost=0.0 → `$0`.
    /// Test: this test itself.
    #[test]
    fn footer_zero_cost() {
        let footer = format_review_footer(None, "my-model", 1, 1, 0.0);
        assert!(
            footer.contains("$0"),
            "zero cost must render as $0: {footer}"
        );
        assert!(
            !footer.contains("$0."),
            "zero cost must not have decimal: {footer}"
        );
    }

    // ── Finalisation-action branch selection ──────────────────────────────────

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
