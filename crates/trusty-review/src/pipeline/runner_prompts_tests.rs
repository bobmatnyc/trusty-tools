//! The reviewer prompts reach the pipeline from content, or the review never
//! starts (#9015).
//!
//! Why: the prompts are runtime content. A review whose prompts cannot load
//! must stop before it fetches the PR, claims a dedup slot or calls a model;
//! a fail-open arm that warned and reviewed anyway would post a review built
//! on no system prompt.
//! What: drives the GitHub path through the fake PR source of
//! `optional_context_off`, with this thread's prompt source pointed at an
//! empty cache, and counts every call the run made.
//! Test: `run_review_without_prompts_fails_closed_before_any_fetch_or_model_call`,
//! `run_review_uses_supplied_prompts_without_loading`,
//! `run_review_without_the_configured_voice_fails_closed_before_any_fetch`.

use std::sync::atomic::Ordering;

use super::optional_context_off::{
    BODY, FakePrSource, billing_diff, hermetic_config, legacy_caller, observe, observe_with,
};
use super::*;
use crate::pipeline::prompt_templates::test_support::{OverrideGuard, prompts};

/// No content installed: the run ends `Skipped`/UNKNOWN with the remedy in
/// `error`, before the PR fetch (and so before any claim), the search query
/// or a reviewer call.
#[tokio::test]
async fn run_review_without_prompts_fails_closed_before_any_fetch_or_model_call() {
    let cache = tempfile::tempdir().expect("cache");
    let _guard = OverrideGuard::not_installed(cache.path());

    let seen = observe(
        FakePrSource::new(BODY, &billing_diff()),
        legacy_caller(),
        ReviewOptions::default(),
    )
    .await;

    let result = &seen.outcome.result;
    let error = result.error.as_deref().expect("the abort explains itself");
    assert!(error.starts_with("not reviewed: "), "{error}");
    assert!(error.contains("tm content update"), "{error}");
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.status, ReviewStatus::Skipped);
    assert!(
        result.infra_unavailable,
        "the MCP envelope must turn isError on"
    );
    assert!(result.dry_run && !result.posted);
    assert_eq!(
        seen.source.meta_calls.load(Ordering::SeqCst),
        0,
        "no PR fetch"
    );
    assert_eq!(
        seen.source.diff_calls.load(Ordering::SeqCst),
        0,
        "no diff fetch"
    );
    assert!(
        seen.search.queries.lock().expect("lock").is_empty(),
        "no search"
    );
    assert!(seen.reviewer.requests().is_empty(), "no reviewer call");
    assert!(seen.verifier.requests().is_empty(), "no verifier call");
}

/// Prompts the caller supplied are used as they are: the review runs with no
/// content installed, and its system prompt is the supplied stock text.
#[tokio::test]
async fn run_review_uses_supplied_prompts_without_loading() {
    let cache = tempfile::tempdir().expect("cache");
    let _guard = OverrideGuard::not_installed(cache.path());

    let options = ReviewOptions::default().with_prompts(Arc::new(prompts().clone()));
    let seen = observe(
        FakePrSource::new(BODY, &billing_diff()),
        legacy_caller(),
        options,
    )
    .await;

    let requests = seen.reviewer.requests();
    assert_eq!(requests.len(), 1, "{:?}", seen.outcome.result.error);
    assert!(requests[0].0.starts_with(prompts().stock()));
}

/// A configured voice the content ships but lacks (a release older than
/// #9015): the run ends unreviewed naming the file, before the PR fetch, the
/// search query or a reviewer call — never a review without the voice.
#[tokio::test]
async fn run_review_without_the_configured_voice_fails_closed_before_any_fetch() {
    use crate::runtime_content::{content_path, test_support::installed_without};
    let voice = content_path("voices/duetto/voice.toml");
    let _guard = OverrideGuard::content(installed_without(&voice));
    let mut config = hermetic_config();
    config.voice_package = Some("duetto".to_string());

    let source = FakePrSource::new(BODY, &billing_diff());
    let seen = observe_with(&config, source, legacy_caller(), ReviewOptions::default()).await;

    let result = &seen.outcome.result;
    let error = result.error.as_deref().expect("the abort explains itself");
    assert!(error.starts_with("not reviewed: "), "{error}");
    assert!(error.contains(&voice), "{error}");
    assert!(error.contains("tm content update"), "{error}");
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.status, ReviewStatus::Skipped);
    assert!(result.infra_unavailable);
    assert_eq!(
        seen.source.meta_calls.load(Ordering::SeqCst),
        0,
        "no PR fetch"
    );
    assert!(
        seen.search.queries.lock().expect("lock").is_empty(),
        "no search"
    );
    assert!(seen.reviewer.requests().is_empty(), "no reviewer call");
}
