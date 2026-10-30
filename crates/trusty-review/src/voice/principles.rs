//! Universal best-practices principles layer (#756), read at runtime from
//! instructional content (ADR-0064, #9991).
//!
//! Why: research into authoritative code-review guidance (Google Engineering
//! Practices, Bacchelli & Bird ICSE 2013, Sadowski et al. ICSE-SEIP 2018,
//! SmartBear/Cisco study, Conventional Comments) distilled into a reusable
//! injectable prompt component.  This layer captures WHAT the field considers
//! effective code review — distinct from HOW the Duetto team reviews (the voice
//! layer).  It is composed BETWEEN the stock base prompt and the voice layer:
//! stock → principles → voice.  It is a product prompt, so a fix to it must not
//! need a crate release: the text lives in
//! `content/instructions/trusty-review/prompts/principles_addendum.md`.
//!
//! What: [`principles_addendum`] reads that file from a resolved content source
//! through `crate::runtime_content::require_text`. An absent file is
//! `PromptContentError::Missing` and an empty one `Invalid`, each naming the
//! path and the remedy. There is no compiled-in default.
//!
//! Sources (cited in research/best-practices/code-review-best-practices-2026-06-04.md):
//!   Google Eng Practices — Reviewer Guide (https://google.github.io/eng-practices/review/reviewer/)
//!   Conventional Comments (https://conventionalcomments.org/)
//!   Bacchelli & Bird, ICSE 2013 (https://dl.acm.org/doi/10.5555/2486788.2486882)
//!   Sadowski et al., ICSE-SEIP 2018 (https://dl.acm.org/doi/10.1145/3183519.3183525)
//!   SmartBear/Cisco study (review size, latency, defect density)
//!   Automated Code Review In Practice, arXiv 2412.18531
//!
//! Test: `principles_addendum_is_non_empty`, `principles_contains_key_concepts`
//! in voice/tests.rs; `the_repository_principles_load_byte_identical` in
//! pipeline/prompt_templates_tests.rs.

use trusty_common::content::ResolvedContent;

use crate::runtime_content::{PromptContentError, content_path, require_text};

/// File name of the principles addendum under
/// `instructions/trusty-review/prompts/`.
pub const PRINCIPLES_FILE: &str = "principles_addendum.md";

/// The bundle path of the principles addendum
/// (`instructions/trusty-review/prompts/principles_addendum.md`).
pub fn principles_path() -> String {
    content_path(&format!("prompts/{PRINCIPLES_FILE}"))
}

/// Reads the universal best-practices principles addendum from `content`.
///
/// Why: the principles text is a product prompt (ADR-0064), so it is runtime
/// content rather than a compiled constant, and a stale or broken release must
/// stop the review instead of sending a prompt without the layer (#9991).
/// What: [`require_text`] over [`principles_path`]; the text is returned
/// verbatim, with no default and no fallback.
/// Test: `the_repository_principles_load_byte_identical`,
/// `a_release_without_the_principles_is_missing_naming_the_path`,
/// `an_empty_principles_file_is_invalid`.
///
/// # Errors
/// [`PromptContentError::Missing`] for an absent file,
/// [`PromptContentError::Invalid`] for an empty one, and the resolver's own
/// error otherwise.
pub fn principles_addendum(content: &ResolvedContent) -> Result<String, PromptContentError> {
    // #9991: read from content; the compiled-in `PRINCIPLES_ADDENDUM` is gone.
    require_text(content, &principles_path())
}
