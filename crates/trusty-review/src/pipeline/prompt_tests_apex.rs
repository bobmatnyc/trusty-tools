//! APEX-specific tests for the review prompt builder (Phase 6, REV-420).
//!
//! Why: extracted from prompt_tests.rs to keep that file under the 500-line cap
//! (#610); these APEX tests are self-contained and cohesive.
//! What: verifies that `build_review_prompt` correctly embeds the APEX section
//! when apex_results are present, and omits it when empty.
//! Test: included via `#[path = "prompt_tests_apex.rs"]` from prompt_tests.rs.

use super::*;

// Shared fixture helpers re-used from the parent test module's helpers submodule.
// `super::helpers` is the `mod helpers` declared in `prompt_tests.rs`, which is
// the parent of this `#[path]`-included module.
use super::helpers::{empty_context, sample_meta, stock_voice};

// ── APEX prompt tests (Phase 6 PR-B, REV-420) ───────────────────────────────

/// ReviewContext with apex_results => heading, file, snippet, citation hint, ordering.
///
/// Why/What: guards the APEX block from silent omission.
/// Test: no network.
#[test]
fn prompt_includes_apex_context_when_present() {
    use crate::integrations::apex_context::ApexContextResult;
    let ctx = ReviewContext {
        apex_results: vec![ApexContextResult {
            file: "apex/auth-spec.md".to_string(),
            snippet: "Token expiry must be checked.".to_string(),
            score: 0.88,
            start_line: Some(42),
        }],
        ..Default::default()
    };
    let content = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &ctx,
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    )
    .messages[0]
        .content
        .clone();
    assert!(content.contains("Related APEX product specs"));
    assert!(content.contains("apex/auth-spec.md"));
    assert!(content.contains("Token expiry must be checked"));
    assert!(content.contains("[apex:"));
    let apex_pos = content.find("Related APEX product specs").unwrap();
    let instr_pos = content.find("populate the structured response").unwrap();
    assert!(
        apex_pos < instr_pos,
        "APEX section must precede closing instruction"
    );
}

/// Empty apex_results => no APEX section.
///
/// Why/What: default config (APEX disabled) must not emit a stray heading.
/// Test: no network.
#[test]
fn prompt_no_apex_section_when_empty() {
    let content = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    )
    .messages[0]
        .content
        .clone();
    assert!(!content.contains("Related APEX product specs"));
    assert!(!content.contains("[apex:"));
}
