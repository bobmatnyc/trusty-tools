//! Tests for the crate-private extra-sections prompt seam (#9197).
//!
//! Why: Architect ruling Q1(a) puts issue blocks into the prompt through
//! `build_review_prompt_with_sections`; an empty section must leave the
//! prompt exactly as the public builder makes it, and a non-empty one must
//! sit between the caller's sections and the retrieved context.
//! What: compares the two builders' requests and the section's position.
//! Test: included as `#[cfg(test)] mod sections_tests` from `prompt.rs`.

use super::*;
use crate::integrations::search_client::SearchResult;

// Inline fixtures: `prompt_test_helpers.rs` is already loaded by `prompt_tests.rs`.
fn sample_meta() -> ReviewPrMeta {
    ReviewPrMeta {
        title: "Add authentication".to_string(),
        body: String::new(),
        author: "alice".to_string(),
        url: "https://github.com/acme/backend/pull/42".to_string(),
    }
}

fn stock_voice() -> VoiceConfig {
    VoiceConfig::stock_only()
}

/// A context with caller text and one search hit, so both neighbours exist.
fn context() -> ReviewContext {
    ReviewContext {
        search_results: vec![SearchResult {
            file: "src/auth.rs".to_string(),
            snippet: Some("pub fn authenticate() {}".to_string()),
            score: 0.9,
            start_line: None,
            end_line: None,
        }],
        referenced_code: Some("fn helper() {}".to_string()),
        ..ReviewContext::default()
    }
}

fn user(extra: &str) -> String {
    let req = build_review_prompt_with_sections(
        "acme",
        "backend",
        &sample_meta(),
        "+fn a() {}\n",
        &context(),
        "",
        "m",
        &stock_voice(),
        false,
        extra,
    );
    req.messages[0].content.clone()
}

/// #9197: an empty or blank section renders nothing; the request equals the
/// public builder's, byte for byte.
#[test]
fn an_empty_extra_section_renders_nothing() {
    let public = build_review_prompt_with_coverage(
        "acme",
        "backend",
        &sample_meta(),
        "+fn a() {}\n",
        &context(),
        "",
        "m",
        &stock_voice(),
        false,
    );
    assert_eq!(user(""), public.messages[0].content);
    assert_eq!(user(" \n"), public.messages[0].content);
}

/// #9197: a section lands after `## Referenced Code` and before
/// `## Related code`, followed by one blank line.
#[test]
fn an_extra_section_sits_between_caller_text_and_retrieved_context() {
    let msg = user("## Linked issues\n\nISSUE_TEXT_9197");
    let referenced = msg.find("## Referenced Code").expect("referenced code");
    let issues = msg
        .find("## Linked issues\n\nISSUE_TEXT_9197\n\n## Related code")
        .expect("the section, one blank line, then the retrieved context");
    assert!(referenced < issues);
}
