//! Tests for the review prompt builder.
//!
//! Why: extracted from `prompt.rs` to keep that file under the 500-line cap
//! while preserving full test coverage.
//! What: system prompt policy checks, prefix stripping, context-block inclusion,
//! response_schema presence, and structured-output language assertions.
//! Test: included as `#[cfg(test)] mod tests` from `prompt.rs`.

use super::*;

fn sample_meta() -> ReviewPrMeta {
    ReviewPrMeta {
        title: "Add authentication".to_string(),
        author: "alice".to_string(),
        url: "https://github.com/acme/backend/pull/42".to_string(),
    }
}

fn empty_context() -> ReviewContext {
    ReviewContext::default()
}

#[test]
fn system_prompt_contains_policy() {
    let prompt = reviewer_system_prompt();
    assert!(
        prompt.contains("default verdict is APPROVE"),
        "system prompt must state APPROVE-default policy"
    );
    assert!(
        prompt.contains("REQUEST_CHANGES requires ALL THREE"),
        "system prompt must specify the REQUEST_CHANGES gate"
    );
    assert!(
        prompt.contains("BLOCK"),
        "system prompt must describe the BLOCK tier"
    );
    // With forced structured output, the schema is passed as response_schema
    // rather than embedded in the system prompt as a JSON fence.
    assert!(
        prompt.contains("verdict"),
        "system prompt must mention the verdict field"
    );
}

/// Regression test: a `bedrock/`-prefixed reviewer_model must be stripped
/// before being set on `LlmRequest.model`.
///
/// Why: guards against Bug 1 regression — BedrockProvider receives the
/// prefixed id as the Converse model parameter, causing HTTP 400.
/// What: passes `bedrock/<id>` to `build_review_prompt` and asserts
/// `LlmRequest.model` is the bare `<id>`.
/// Test: this test itself; no network calls.
#[test]
fn build_review_prompt_strips_bedrock_prefix() {
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "bedrock/us.anthropic.claude-sonnet-4-6",
    );
    assert_eq!(
        req.model, "us.anthropic.claude-sonnet-4-6",
        "bedrock/ prefix must be stripped from LlmRequest.model"
    );
}

/// Regression test: an `openrouter/`-prefixed model must also be stripped.
///
/// Why: same Bug 1 pattern; OpenRouter API does not accept the routing prefix.
/// What: passes `openrouter/<id>` and asserts the bare id is used.
/// Test: this test itself; no network calls.
#[test]
fn build_review_prompt_strips_openrouter_prefix() {
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "openrouter/openai/gpt-5.4-mini-20260317",
    );
    assert_eq!(
        req.model, "openai/gpt-5.4-mini-20260317",
        "openrouter/ prefix must be stripped from LlmRequest.model"
    );
}

#[test]
fn build_review_prompt_includes_diff() {
    let diff = "+fn hello() { println!(\"hi\"); }\n";
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        diff,
        &empty_context(),
        "openai/gpt-5.4-mini-20260317",
    );
    assert_eq!(req.model, "openai/gpt-5.4-mini-20260317");
    assert_eq!(req.messages.len(), 1);
    let content = &req.messages[0].content;
    assert!(
        content.contains("fn hello"),
        "user message must include the diff"
    );
    assert!(
        content.contains("acme/backend"),
        "user message must include owner/repo"
    );
    assert!(
        content.contains("Add authentication"),
        "user message must include PR title"
    );
    assert!((req.temperature - REVIEWER_TEMPERATURE).abs() < f32::EPSILON);
}

#[test]
fn prompt_includes_context_blocks() {
    use crate::integrations::search_client::SearchResult;

    let context = ReviewContext {
        search_results: vec![SearchResult {
            file: "src/auth.rs".to_string(),
            snippet: Some("pub fn verify() {}".to_string()),
            score: 0.9,
            start_line: Some(10),
            end_line: Some(12),
        }],
        complexity_hotspots: vec![ComplexityHotspot {
            file: "src/auth.rs".to_string(),
            function_name: Some("verify".to_string()),
            cyclomatic: 12,
            cognitive: 8,
        }],
        smells: vec![Smell {
            file: "src/auth.rs".to_string(),
            category: "long_method".to_string(),
            severity: "medium".to_string(),
            line: Some(20),
        }],
    };

    let req = build_review_prompt(
        "acme",
        "repo",
        &sample_meta(),
        "+fn foo() {}",
        &context,
        "openai/gpt-5.4-mini-20260317",
    );
    let content = &req.messages[0].content;
    assert!(
        content.contains("Related code"),
        "user message must include search context section"
    );
    assert!(
        content.contains("pub fn verify"),
        "user message must include search snippet"
    );
    assert!(
        content.contains("Complexity hotspots"),
        "user message must include hotspot section"
    );
    assert!(
        content.contains("Code smells"),
        "user message must include smells section"
    );
}

#[test]
fn prompt_empty_context_omits_sections() {
    let req = build_review_prompt(
        "o",
        "r",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "openai/gpt-5.4-nano-20260317",
    );
    let content = &req.messages[0].content;
    assert!(
        !content.contains("Related code"),
        "empty context must not include search section"
    );
    assert!(
        !content.contains("Complexity hotspots"),
        "empty context must not include hotspot section"
    );
}

/// Verify that `build_review_prompt` includes `response_schema` for structured output.
///
/// Why: if `response_schema` is absent, the provider uses free text and the
/// fail-safe APPROVE problem returns (Haiku always fail-safes; Sonnet sometimes does).
/// What: asserts `LlmRequest.response_schema` is `Some` and the schema name
/// matches the expected constant.
/// Test: no network.
#[test]
fn build_review_prompt_includes_response_schema() {
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "us.anthropic.claude-sonnet-4-6",
    );
    let schema = req
        .response_schema
        .expect("response_schema must be set on every review prompt");
    assert_eq!(
        schema.name, "review_output",
        "schema name must be review_output"
    );
    assert!(schema.schema.is_object(), "schema must be a JSON object");
    let props = &schema.schema["properties"];
    assert!(
        props["verdict"].is_object(),
        "schema must have verdict property"
    );
    assert!(
        props["findings"].is_object(),
        "schema must have findings property"
    );
}

/// Verify the system prompt no longer contains fence-based output instructions.
///
/// Why: with forced structured output, the model must populate the structured
/// response fields, not emit a fenced JSON block.  Fence instructions confuse
/// models that try to literally wrap their output in backticks.
/// What: asserts the system prompt does not contain the old "```json" fence
/// instruction, and does contain the new "structured response" wording.
/// Test: no network.
#[test]
fn system_prompt_uses_structured_output_language() {
    let prompt = reviewer_system_prompt();
    assert!(
        !prompt.contains("```json"),
        "system prompt must not contain the old fenced JSON block instruction"
    );
    assert!(
        prompt.contains("structured response"),
        "system prompt must use structured-response language"
    );
}

#[test]
fn prompt_local_diff_mode_no_pr_metadata() {
    // In --local-diff mode, pr_meta has empty fields.
    let meta = ReviewPrMeta::default();
    let req = build_review_prompt(
        "local",
        "local",
        &meta,
        "+fn local_fn() {}",
        &empty_context(),
        "openai/gpt-5.4-mini-20260317",
    );
    let content = &req.messages[0].content;
    assert!(content.contains("local_fn"));
}
