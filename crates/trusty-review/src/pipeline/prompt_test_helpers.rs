//! Shared test helpers for the review prompt test suite.
//!
//! Why: `sample_meta()`, `empty_context()`, and `stock_voice()` were duplicated
//! across `prompt_tests.rs`, `prompt_tests_apex.rs`, and `prompt_voice_tests.rs`.
//! Duplication meant a change to `ReviewPrMeta` required three edits; it also
//! inflated each file toward the 500-line cap.  A single shared module eliminates
//! the duplication and is the canonical source for these fixtures.
//! What: re-exports the three helper functions; included via `#[path]` from
//! each test file that previously defined its own copies.
//! Test: the helpers themselves have no tests; they are exercised transitively
//! by every test function that calls them.

use crate::{
    pipeline::prompt::{ReviewContext, ReviewPrMeta},
    voice::VoiceConfig,
};

/// Minimal `ReviewPrMeta` fixture for tests that need a populated PR meta.
///
/// Why: avoids per-file boilerplate; keeps test signal focused on behaviour
/// rather than fixture construction.
/// What: returns a `ReviewPrMeta` with stable, recognisable field values.
/// Test: exercised by every test that passes this to `build_review_prompt`.
pub(super) fn sample_meta() -> ReviewPrMeta {
    ReviewPrMeta {
        title: "Add authentication".to_string(),
        body: String::new(),
        author: "alice".to_string(),
        url: "https://github.com/acme/backend/pull/42".to_string(),
    }
}

/// Empty `ReviewContext` with all optional slices defaulted to empty.
///
/// Why: most prompt tests do not care about search/hotspot/smell/apex context;
/// this helper makes the intent explicit and saves per-test boilerplate.
/// What: returns `ReviewContext::default()`.
/// Test: exercised by every test that passes this to `build_review_prompt`.
pub(super) fn empty_context() -> ReviewContext {
    ReviewContext::default()
}

/// Stock-only `VoiceConfig` (no principles, no voice addendum).
///
/// Why: prompt tests that do not exercise the voice layer need a zero-addendum
/// config; using `VoiceConfig::stock_only()` makes the intent explicit.
/// What: returns `VoiceConfig::stock_only()`.
/// Test: exercised by every test that passes this to `build_review_prompt`.
pub(super) fn stock_voice() -> VoiceConfig {
    VoiceConfig::stock_only()
}
