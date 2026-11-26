//! Hosted repo skills through the whole pipeline (#11899 step 2, DOC-93 RS-38).
//!
//! Why: the unit tests prove the hosted reader's rules; this proves the
//! runner, given the webhook drain's options, reads the base config and skill
//! through the Contents API seam at the PR base SHA, puts the skill in the
//! reviewer's system prompt, records it and names it in the review body.
//! What: `optional_context_off`'s fake PR source reporting the base SHA, and
//! the `hosted_tests` fake Contents API reader as the `doc_fetcher` seam.
//! Test: `runner_hosted_review_loads_base_skill_into_the_system_prompt`.

use std::sync::Arc;

use super::optional_context_off::{BODY, FakePrSource, billing_diff, legacy_caller, observe};
use crate::integrations::context::contents_at_ref::DocFetcher;
use crate::pipeline::repo_skills::hosted_tests::{BASE, Fake};

/// Acceptance (a), (b), (d) on the hosted path: the base-SHA skill reaches
/// the reviewer's system prompt and the review record and body.
#[tokio::test]
async fn runner_hosted_review_loads_base_skill_into_the_system_prompt() {
    let fake = Arc::new(
        Fake::default()
            .config(&["billing-checks"])
            .skill("billing-checks", "BASE-RULE: flag unchecked arithmetic."),
    );
    let mut source = FakePrSource::new(BODY, &billing_diff());
    source.head.base_sha = BASE.to_string();
    let mut options = crate::webhook_drain::hosted_review_options();
    options.doc_fetcher = Some(fake.clone() as Arc<dyn DocFetcher>);
    let seen = observe(source, legacy_caller(), options).await;
    let requests = seen.reviewer.requests();
    let system = &requests.first().expect("a reviewer call").0;
    assert!(system.contains("BASE-RULE"), "{system}");
    assert!(fake.calls().iter().all(|(_, sha)| sha == BASE));
    let result = &seen.outcome.result;
    let record = result.repo_skills.as_ref().expect("record");
    assert_eq!((record.base_sha.as_str(), record.fork), (BASE, false));
    assert_eq!(record.loaded.len(), 1);
    assert_eq!(record.loaded[0].name, "billing-checks");
    assert!(
        result
            .review_body
            .contains("- loaded `billing-checks` (sha256 "),
        "{}",
        result.review_body
    );
}
