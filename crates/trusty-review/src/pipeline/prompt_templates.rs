//! System-prompt templates for the reviewer role.
//!
//! Why: the two prompt constants (stock and coverage-gating variant) are each
//! ~140 lines of prose; keeping them as inline literals bloated this file to
//! ~95% prose. Per the repo `include_str!` convention (see
//! `trusty-mpm/src/core/bundle.rs`) the text now lives verbatim under
//! `assets/prompts/` and is embedded at compile time, keeping the source lean
//! while the rendered bytes stay byte-for-byte identical to the old literals.
//!
//! What: two `pub const` strings exported to `prompt.rs`, each `include_str!`
//! of its `.md` asset. The stock constant is the pre-#1014 text (unchanged
//! behaviour when coverage gating is off). The coverage-gating variant replaces
//! the "do not block on coverage" advisory with an informational note that the
//! runner will apply a deterministic floor.
//!
//! Test: `system_prompt_coverage_gating_on`, `system_prompt_coverage_gating_off`
//! in `prompt_tests.rs`; `prompt_assets_are_byte_identical` below pins the
//! embedded bytes so the extraction can never silently drift.

/// Stock reviewer system prompt (no coverage-gating language).
///
/// Why: the default reviewer prompt used when coverage gating is off; kept
/// byte-identical to the pre-#1014 text so review behaviour is unchanged.
/// What: embeds `assets/prompts/system_prompt_stock.md` verbatim at compile
/// time via `include_str!`.
/// Test: `prompt_assets_are_byte_identical`, `system_prompt_coverage_gating_off`.
pub const SYSTEM_PROMPT_STOCK: &str = include_str!("../../assets/prompts/system_prompt_stock.md");

/// Coverage-gating reviewer system prompt variant.
///
/// Why: used when a deterministic coverage floor is applied by the runner
/// (#1014); the "do not block on coverage" advisory is replaced with an
/// informational note. Kept byte-identical to the original inline literal.
/// What: embeds `assets/prompts/system_prompt_coverage_gating.md` verbatim at
/// compile time via `include_str!`.
/// Test: `prompt_assets_are_byte_identical`, `system_prompt_coverage_gating_on`.
pub const SYSTEM_PROMPT_COVERAGE_GATING: &str =
    include_str!("../../assets/prompts/system_prompt_coverage_gating.md");

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the embedded prompt bytes so the `include_str!` extraction can never
    /// silently drift from the intended text.
    ///
    /// Why: the prompts feed the reviewer LLM and are part of the crate's
    /// observable contract; a stray edit to an asset must be caught here.
    /// What: asserts the exact opening line, exact closing text, and the frozen
    /// byte lengths of both embedded prompts.
    /// Test: this is the test.
    #[test]
    fn prompt_assets_are_byte_identical() {
        // Both prompts open with the same role line.
        let opening = "You are a senior software engineer performing a pull-request code review.";
        assert!(SYSTEM_PROMPT_STOCK.starts_with(opening));
        assert!(SYSTEM_PROMPT_COVERAGE_GATING.starts_with(opening));

        // Both prompts end with the exact same closing sentence, no trailing
        // newline (the original literals closed `"#;` on the same line).
        let closing = "`findings` may be an empty array if there are no issues.";
        assert!(SYSTEM_PROMPT_STOCK.ends_with(closing));
        assert!(SYSTEM_PROMPT_COVERAGE_GATING.ends_with(closing));
        assert!(!SYSTEM_PROMPT_STOCK.ends_with('\n'));
        assert!(!SYSTEM_PROMPT_COVERAGE_GATING.ends_with('\n'));

        // Frozen byte lengths (updated in #PR84: both prompts gained the
        // citable-findings gate + author-gated rule + the `code_provable` field
        // docs; then the daemon-native inline citation grammar — Gap #1 of the
        // #PR84 follow-up; then the `code_provable` worked examples —
        // adversarial-review item 4).
        assert_eq!(SYSTEM_PROMPT_STOCK.len(), 14104);
        assert_eq!(SYSTEM_PROMPT_COVERAGE_GATING.len(), 14420);

        // The coverage-gating variant is distinguished by its coverage-floor note.
        assert!(SYSTEM_PROMPT_COVERAGE_GATING.contains("deterministic\ncoverage floor"));
        assert!(!SYSTEM_PROMPT_STOCK.contains("deterministic\ncoverage floor"));
    }
}
