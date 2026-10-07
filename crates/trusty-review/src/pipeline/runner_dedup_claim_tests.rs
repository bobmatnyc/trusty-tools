//! A review that does not post leaves no in-progress dedup claim (#9348).
//!
//! Why: CLI `run` claimed `owner/repo/pr/head_sha` on every GitHub run,
//! `--dry-run` included. A dry run never completed or released that claim,
//! and neither did a failed live post, so a live review of the same head
//! failed with `InProgressElsewhere` until the 7200s stale TTL expired.
//! What: drives the real pipeline down the GitHub path through the fake
//! [`PrSource`](crate::pipeline::optional_context::PrSource) from
//! `runner_optional_context_off_tests.rs`, with a real tempdir `DedupStore`,
//! and checks the store afterwards with a second `claim`.
//! Test: `dry_run_leaves_no_in_progress_claim`,
//! `dry_run_never_claims_over_another_holder`,
//! `dry_run_abort_keeps_a_completed_record`, `failed_post_releases_its_claim`,
//! `live_run_on_a_completed_head_is_still_skipped`.

use super::optional_context_off::{BODY, FakePrSource, HEAD_SHA, billing_diff, hermetic_config};
use super::*;
use crate::pipeline::optional_context::OptionalContextRequest;
use crate::pipeline::post::{PostContext, finalize_review};
use crate::store::{ClaimOutcome, DedupStore};

const OWNER: &str = "acme";
const REPO: &str = "billing";
const PR: u64 = 7;

/// A store on a fresh file; the directory guard keeps the file alive.
fn fresh_store() -> (Arc<DedupStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = DedupStore::open(&dir.path().join("dedup.redb")).expect("open");
    (Arc::new(store), dir)
}

/// The input CLI `run` builds for `acme/billing#7`: posting allowed, with
/// `trigger` deciding live or dry.
fn run_input(trigger: TriggerDecision) -> ReviewInput {
    ReviewInput {
        diff_source: DiffSource::Github {
            owner: OWNER.to_string(),
            repo: REPO.to_string(),
            pr: PR,
            token: "fixture-token".to_string(),
        },
        reviewer_model: "stub/reviewer".to_string(),
        write_log: false,
        print_result: false,
        trigger,
        run_mode: RunMode::Cli,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::Interactive,
    }
}

/// Run one review of the fixture PR against `store`.
async fn review(
    config: &ReviewConfig,
    trigger: TriggerDecision,
    llm: FakeLlm,
    store: &Arc<DedupStore>,
) -> ReviewResult {
    let mut deps = ready_deps(Arc::new(llm), None);
    deps.dedup = Some(Arc::clone(store));
    let mut options = ReviewOptions::new(OptionalContextRequest::default());
    options.pr_source = Some(Arc::new(FakePrSource::new(BODY, &billing_diff())));
    run_review_with(config, run_input(trigger), deps, options)
        .await
        .result
}

/// What a later claim of the fixture head sees.
async fn next_claim(store: &Arc<DedupStore>) -> ClaimOutcome {
    store.claim(OWNER, REPO, PR, HEAD_SHA).await.expect("claim")
}

/// REGRESSION (#9348), arm (a): a dry run — forced by the trigger, or by the
/// config flag the webhook worker defers to — must leave no `InProgress`.
/// What: a dry run of the fixture head, then a second claim of that head.
/// Test: this test. Fails pre-fix: the second claim is `InProgressElsewhere`.
#[tokio::test]
async fn dry_run_leaves_no_in_progress_claim() {
    for (trigger, config_dry_run) in [
        (TriggerDecision::ForceDryRun, false),
        (TriggerDecision::None, true),
    ] {
        let (store, _dir) = fresh_store();
        let mut config = hermetic_config();
        config.dry_run = config_dry_run;

        let out = review(&config, trigger, FakeLlm::approves(), &store).await;
        assert!(out.dry_run && !out.posted, "{trigger:?}: a dry run");

        assert_eq!(
            next_claim(&store).await,
            ClaimOutcome::Claimed,
            "{trigger:?}: a dry run must not strand an InProgress claim"
        );
    }
}

/// REGRESSION (#9348), arm (d): a dry run never claims, so a live review
/// holding the head neither blocks it nor loses its claim to it.
/// What: another holder claims the head; a dry run reviews it; the holder's
/// claim is still in progress afterwards.
/// Test: this test. Fails pre-fix: the dry run hit the holder's claim and
/// reported "not reviewed" with `Verdict::Unknown`.
#[tokio::test]
async fn dry_run_never_claims_over_another_holder() {
    let (store, _dir) = fresh_store();
    assert_eq!(next_claim(&store).await, ClaimOutcome::Claimed);

    let out = review(
        &hermetic_config(),
        TriggerDecision::ForceDryRun,
        FakeLlm::approves(),
        &store,
    )
    .await;

    assert_eq!(
        out.verdict,
        Verdict::Approve,
        "the dry run must review, not stop at the dedup claim: {:?}",
        out.error
    );
    assert_eq!(
        next_claim(&store).await,
        ClaimOutcome::InProgressElsewhere,
        "the live holder's claim must survive the dry run"
    );
}

/// Guard (#9348): a dry run that aborts must not release a record it never
/// claimed — here the `Completed` record a live post wrote.
/// What: the head is completed; a dry run of it aborts on an LLM error; a
/// later claim is still `Skipped`.
/// Test: this test. Fails if `abort_dry` releases for a run that cannot post.
#[tokio::test]
async fn dry_run_abort_keeps_a_completed_record() {
    let (store, _dir) = fresh_store();
    assert_eq!(next_claim(&store).await, ClaimOutcome::Claimed);
    store
        .complete(OWNER, REPO, PR, HEAD_SHA)
        .await
        .expect("complete");

    let out = review(
        &hermetic_config(),
        TriggerDecision::ForceDryRun,
        FakeLlm::errors("provider down"),
        &store,
    )
    .await;
    assert!(out.dry_run && !out.posted);

    assert_eq!(
        next_claim(&store).await,
        ClaimOutcome::Skipped,
        "a completed review must still suppress a live re-run"
    );
}

/// REGRESSION (#9348), arm (b): a live post that fails before its POST is
/// sent releases the claim, so a retry can run.
/// What: the head is claimed as `claim_slot` would; `finalize_review` posts
/// live in serve mode with no App credentials, so token resolution fails
/// before any request; a retry's claim then succeeds. No network is used.
/// Test: this test. Fails pre-fix: the retry's claim is `InProgressElsewhere`.
#[tokio::test]
async fn failed_post_releases_its_claim() {
    let (store, dir) = fresh_store();
    assert_eq!(next_claim(&store).await, ClaimOutcome::Claimed);
    let mut config = hermetic_config();
    config.dry_run = false;
    config.log_dir = dir.path().to_path_buf();

    let mut result = ReviewResult::new(OWNER, REPO, PR, "t", "u");
    result.head_sha = HEAD_SHA.to_string();
    let out = finalize_review(
        result,
        &config,
        TriggerDecision::ForceLive,
        true,
        false,
        false,
        PostContext {
            owner: OWNER,
            repo: REPO,
            pr: PR,
            head_sha: HEAD_SHA,
            run_mode: RunMode::Serve,
            dedup: Some(&store),
        },
    )
    .await;

    assert!(!out.posted && out.dry_run, "the post failed");
    let error = out.error.expect("a failed post explains itself");
    assert!(error.starts_with("post failed"), "{error}");
    assert_eq!(
        next_claim(&store).await,
        ClaimOutcome::Claimed,
        "a post that never left must not block the retry"
    );
}

/// Control (#9348), arm (c): a completed live post still suppresses a live
/// re-run of the same head, and a dry run in between does not undo it.
/// What: the head carries the `Completed` record the successful-post arm of
/// `finalize_review` writes; a dry run and then a live run review it.
/// Test: this test. Passes before and after the fix.
#[tokio::test]
async fn live_run_on_a_completed_head_is_still_skipped() {
    let (store, _dir) = fresh_store();
    assert_eq!(next_claim(&store).await, ClaimOutcome::Claimed);
    store
        .complete(OWNER, REPO, PR, HEAD_SHA)
        .await
        .expect("complete");
    let config = hermetic_config();

    let dry = review(
        &config,
        TriggerDecision::ForceDryRun,
        FakeLlm::approves(),
        &store,
    )
    .await;
    assert!(!dry.posted, "a dry run never posts");

    let live = review(
        &config,
        TriggerDecision::ForceLive,
        FakeLlm::approves(),
        &store,
    )
    .await;
    assert!(!live.posted, "the completed head must not be posted again");
    assert_eq!(
        live.error.as_deref(),
        Some("skipped: duplicate of a completed review")
    );
}
