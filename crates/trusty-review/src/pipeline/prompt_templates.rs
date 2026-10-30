//! Reviewer system prompts, read at runtime from instructional content
//! (ADR-0064, #9015).
//!
//! Why: the two reviewer prompts (stock and coverage-gating) are product
//! prompts, an ADR-0064 content class. A published crate cannot embed a file
//! outside its own directory, and a prompt fix must not need a crate release.
//! So the text lives under `content/instructions/trusty-review/prompts/` and
//! is read through `crate::runtime_content`: the sha256-pinned bundle `tm
//! content update` installed, or a checkout the operator names (#9015).
//! What: [`ReviewerPrompts::from_content`] reads both prompts from a resolved
//! source; [`ReviewerPrompts::load`] does it for this process. No content, an
//! absent prompt, or an empty one is a [`PromptContentError`] whose text names
//! the remedy; nothing here answers with a default or partial prompt.
//! Test: `prompt_templates_tests.rs` (one test per error arm), and the binary
//! refusal in `tests/stale_content_e2e.rs`.
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use std::sync::Arc;

use trusty_common::content::ResolvedContent;

// #9015: the content resolution and its error moved to `crate::runtime_content`,
// which the templates and the voice read through too; re-exported here so the
// prompt paths stay where callers found them.
pub use crate::runtime_content::{PromptContentError, REMEDY, describe_source};
use crate::runtime_content::{content_path, require_text, resolve_for_process};
use crate::voice::principles::principles_addendum;

/// Bundle directory of trusty-review's prompts, under the `instructions`
/// class (ADR-0064 amendment 2026-10-01, item 3).
pub const PROMPTS_DIR: &str = "instructions/trusty-review/prompts";

/// File name of the stock reviewer prompt under [`PROMPTS_DIR`].
pub const STOCK_FILE: &str = "system_prompt_stock.md";

/// File name of the coverage-gating reviewer prompt under [`PROMPTS_DIR`].
pub const COVERAGE_GATING_FILE: &str = "system_prompt_coverage_gating.md";

/// The bundle path of prompt file `file` (`system_prompt_stock.md`).
pub fn prompt_path(file: &str) -> String {
    content_path(&format!("prompts/{file}"))
}

/// The two reviewer system prompts, read from one content source, and that
/// source for the principles addendum.
///
/// Why: replaces the two `include_str!` constants (#9015) with the same text,
/// read from content; a caller loads once and builds any number of prompts.
/// The principles addendum is read from the same source, but only when the
/// config enables it (#9991), so the source is kept rather than the text.
/// What: built only by [`ReviewerPrompts::from_content`], which never returns
/// an empty prompt. Equal when both prompts match and the source is the same
/// `Arc`.
/// Test: `the_repository_prompts_load_byte_identical`,
/// `the_repository_principles_load_byte_identical`.
#[derive(Debug, Clone)]
pub struct ReviewerPrompts {
    stock: String,
    coverage_gating: String,
    // #9991: the principles addendum is read from here, on demand.
    content: Arc<ResolvedContent>,
}

impl PartialEq for ReviewerPrompts {
    fn eq(&self, other: &Self) -> bool {
        self.stock == other.stock
            && self.coverage_gating == other.coverage_gating
            && Arc::ptr_eq(&self.content, &other.content)
    }
}

impl Eq for ReviewerPrompts {}

impl ReviewerPrompts {
    /// Reads both prompts from `content`.
    ///
    /// # Errors
    /// [`PromptContentError::Missing`] naming the first absent prompt,
    /// [`PromptContentError::Invalid`] for an empty one, and the resolver's own
    /// error otherwise.
    /// Test: `a_release_without_the_prompts_is_missing_naming_the_path`,
    /// `an_empty_prompt_is_invalid`.
    pub fn from_content(content: &Arc<ResolvedContent>) -> Result<Self, PromptContentError> {
        Ok(Self {
            stock: require_prompt(content, STOCK_FILE)?,
            coverage_gating: require_prompt(content, COVERAGE_GATING_FILE)?,
            content: Arc::clone(content),
        })
    }

    /// [`ReviewerPrompts::from_content`] over the content resolved for this
    /// process: the checkout `TRUSTY_REVIEW_CONTENT_CHECKOUT` names, else the
    /// installed bundle; never the cwd (#9015). Never fetches.
    ///
    /// Test: `no_content_is_not_installed_naming_the_remedy`; end to end in
    /// `tests/stale_content_e2e.rs`.
    pub fn load() -> Result<Self, PromptContentError> {
        let content = resolve_for_process()?;
        Self::from_content(&content)
    }

    /// The stock prompt: no coverage-gating language.
    pub fn stock(&self) -> &str {
        &self.stock
    }

    /// The coverage-gating variant (#1014): the "do not block on coverage"
    /// advisory is replaced with a note that the runner applies a floor.
    pub fn coverage_gating(&self) -> &str {
        &self.coverage_gating
    }

    /// The principles addendum, read now from the source these prompts came
    /// from (#9991).
    ///
    /// Why: the caller reads it only when `voice_principles` is on, so a
    /// release without the file still serves a config that turned it off.
    /// What: [`principles_addendum`] over the kept source.
    /// Test: `the_repository_principles_load_byte_identical`,
    /// `a_release_without_the_principles_is_missing_naming_the_path`,
    /// `an_empty_principles_file_is_invalid`.
    ///
    /// # Errors
    /// [`PromptContentError::Missing`] for an absent file,
    /// [`PromptContentError::Invalid`] for an empty one.
    pub fn principles(&self) -> Result<String, PromptContentError> {
        principles_addendum(&self.content)
    }

    /// [`Self::coverage_gating`] when `coverage_gating_enabled`, else
    /// [`Self::stock`].
    pub fn for_coverage(&self, coverage_gating_enabled: bool) -> &str {
        if coverage_gating_enabled {
            &self.coverage_gating
        } else {
            &self.stock
        }
    }
}

/// Reads prompt `file`, which must exist and hold text.
fn require_prompt(content: &ResolvedContent, file: &str) -> Result<String, PromptContentError> {
    require_text(content, &prompt_path(file))
}

#[cfg(test)]
#[path = "prompt_templates_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "prompt_templates_tests.rs"]
mod tests;
