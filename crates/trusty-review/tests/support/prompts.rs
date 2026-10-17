//! The checkout's reviewer prompts, supplied to an in-process review (#9015).
//!
//! Why: an in-process review cannot rely on an installed bundle (EVO has none,
//! and syncs no `.git`, #9619), so the model-eval driver hands the pipeline
//! the checkout's prompts through `ReviewOptions::with_prompts`.
//! What: [`checkout_prompts`] reads them through the resolver from the staged
//! bundle `support/content.rs` builds.
//! Test: `perfect_recording_catches_every_defect` (tests/model_eval.rs).

use trusty_review::pipeline::ReviewerPrompts;

/// The checkout's prompts, read through the resolver from a staged bundle.
pub fn checkout_prompts() -> ReviewerPrompts {
    ReviewerPrompts::from_content(&crate::content::staged_content())
        .expect("the checkout's prompts load")
}
