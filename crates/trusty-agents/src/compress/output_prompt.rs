//! Caveman-style output compression prompt fragment.
//!
//! Why: LLMs burn output tokens on linguistic "glue" (filler phrases,
//!      preamble, postamble, hedge words) that carries zero information.
//!      Injecting a caveman-style constraint into sub-agent system prompts
//!      reduces output tokens by 22–87% (65% avg on multi-turn sessions)
//!      with no accuracy loss.
//!      Reference: https://github.com/JuliusBrussee/caveman (MIT)
//!
//! What: Generates a system-prompt fragment instructing the model to use
//!      a specified compression level. Three modes: Lite / Full / Ultra.
//!      Fragment is appended to sub-agent system prompts when
//!      `[compress] output_style` is set in agent TOML.
//!
//! Test: See `#[cfg(test)]` module at bottom of file.

use serde::Deserialize;

/// Output-style compression level for sub-agent responses.
///
/// Why: Different agents tolerate different levels of terseness. A QA agent
/// reporting test results can go Ultra; an explanation-heavy ticketing
/// agent should stay Lite or Full.
/// What: Four modes — None disables, Lite keeps grammar, Full drops articles
/// and fillers, Ultra goes telegraphic.
/// Test: `parses_from_toml_string`, `default_is_full`, `none_returns_none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputStyle {
    /// Disabled — no caveman instruction injected.
    None,
    /// Drop filler phrases / preamble / postamble but keep full grammar.
    Lite,
    /// Default — fragments, drop articles ("a"/"the"), execute before explaining.
    #[default]
    Full,
    /// Telegraphic — maximum density, drop hedge words and verbose synonyms.
    Ultra,
}

const LITE_PROMPT: &str = "\
## Output Style: Lite

Apply these output-compression rules to every response:

1. No filler phrases (\"I'd be happy to\", \"Sure, let me\", \"Great question!\").
2. Execute before explaining — show the work, then describe it if needed.
3. No meta-commentary about what you are about to do.
4. No preamble restating the task.
5. No postamble summarizing what you just did unless explicitly requested.
6. No tool-use announcements (\"I'll now call the X tool\").
7. Explain only when strictly necessary for correctness.
8. Let code speak for itself — no narration of obvious behavior.
9. Treat errors as things to fix, not narrate.

Keep full sentence grammar; just strip the linguistic glue.";

const FULL_PROMPT: &str = "\
## Output Style: Full

Apply these output-compression rules to every response:

1. No filler phrases (\"I'd be happy to\", \"Sure, let me\", \"Great question!\").
2. Execute before explaining — show the work, then describe it if needed.
3. No meta-commentary about what you are about to do.
4. No preamble restating the task.
5. No postamble summarizing what you just did unless explicitly requested.
6. No tool-use announcements (\"I'll now call the X tool\").
7. Explain only when strictly necessary for correctness.
8. Let code speak for itself — no narration of obvious behavior.
9. Treat errors as things to fix, not narrate.
10. Drop articles (\"a\", \"the\") and hedge words (\"maybe\", \"perhaps\", \"sort of\")
    where the meaning stays clear.

Prefer sentence fragments to full sentences when fragments carry the signal.";

const ULTRA_PROMPT: &str = "\
## Output Style: Ultra

Apply these output-compression rules to every response. Maximum density.

1. No filler phrases (\"I'd be happy to\", \"Sure, let me\", \"Great question!\").
2. Execute before explaining — show the work, then describe it if needed.
3. No meta-commentary about what you are about to do.
4. No preamble restating the task.
5. No postamble summarizing what you just did unless explicitly requested.
6. No tool-use announcements (\"I'll now call the X tool\").
7. Explain only when strictly necessary for correctness.
8. Let code speak for itself — no narration of obvious behavior.
9. Treat errors as things to fix, not narrate.
10. Drop articles (\"a\", \"the\"), hedge words (\"maybe\", \"perhaps\"), and verbose
    synonyms. Use telegraphic style: noun-verb-object only.

Examples:
- BAD:  \"I have updated the file and the tests are now passing.\"
- GOOD: \"File updated. Tests pass.\"
- BAD:  \"It looks like there might be a small issue with the import.\"
- GOOD: \"Import broken.\"";

/// Return the caveman-style prompt fragment for the requested style.
///
/// Why: Centralizes the prompt text so callers (prompt_builder) can append
/// it as a system-prompt layer without owning the rule text.
/// What: Returns `Some(&'static str)` for Lite/Full/Ultra; returns `None`
/// for `OutputStyle::None` so the caller can skip appending entirely.
/// Test: `none_returns_none`, `lite_contains_no_filler_rule`,
/// `full_contains_articles_rule`, `ultra_contains_telegraphic_example`.
pub fn output_compression_prompt(style: OutputStyle) -> Option<&'static str> {
    match style {
        OutputStyle::None => None,
        OutputStyle::Lite => Some(LITE_PROMPT),
        OutputStyle::Full => Some(FULL_PROMPT),
        OutputStyle::Ultra => Some(ULTRA_PROMPT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_full() {
        assert_eq!(OutputStyle::default(), OutputStyle::Full);
    }

    #[test]
    fn none_returns_none() {
        assert!(output_compression_prompt(OutputStyle::None).is_none());
    }

    #[test]
    fn lite_contains_no_filler_rule() {
        let p = output_compression_prompt(OutputStyle::Lite).unwrap();
        assert!(p.contains("No filler"));
        assert!(p.contains("Execute before explaining"));
        assert!(p.contains("Lite"));
    }

    #[test]
    fn full_contains_articles_rule() {
        let p = output_compression_prompt(OutputStyle::Full).unwrap();
        assert!(p.contains("Drop articles"));
        assert!(p.contains("Full"));
    }

    #[test]
    fn ultra_contains_telegraphic_example() {
        let p = output_compression_prompt(OutputStyle::Ultra).unwrap();
        assert!(p.contains("telegraphic"));
        assert!(p.contains("Ultra"));
        // Has the BAD/GOOD example block.
        assert!(p.contains("BAD"));
        assert!(p.contains("GOOD"));
    }

    #[test]
    fn all_modes_contain_core_caveman_rules() {
        // Rules 1–9 appear in every non-None style.
        for style in [OutputStyle::Lite, OutputStyle::Full, OutputStyle::Ultra] {
            let p = output_compression_prompt(style).unwrap();
            assert!(p.contains("No filler"), "style {style:?} missing rule 1");
            assert!(
                p.contains("Execute before"),
                "style {style:?} missing rule 2"
            );
            assert!(
                p.contains("meta-commentary"),
                "style {style:?} missing rule 3"
            );
            assert!(p.contains("preamble"), "style {style:?} missing rule 4");
            assert!(p.contains("postamble"), "style {style:?} missing rule 5");
            assert!(
                p.contains("tool-use announcements"),
                "style {style:?} missing rule 6"
            );
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct WrapCfg {
        output_style: OutputStyle,
    }

    #[test]
    fn parses_from_toml_string() {
        let cfg: WrapCfg = toml::from_str("output_style = \"ultra\"").unwrap();
        assert_eq!(cfg.output_style, OutputStyle::Ultra);
        let cfg: WrapCfg = toml::from_str("output_style = \"none\"").unwrap();
        assert_eq!(cfg.output_style, OutputStyle::None);
        let cfg: WrapCfg = toml::from_str("output_style = \"lite\"").unwrap();
        assert_eq!(cfg.output_style, OutputStyle::Lite);
        let cfg: WrapCfg = toml::from_str("output_style = \"full\"").unwrap();
        assert_eq!(cfg.output_style, OutputStyle::Full);
    }
}
