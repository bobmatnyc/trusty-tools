//! Tests for the review prompt builder.
//!
//! Why: extracted from `prompt.rs` to keep that file under the 500-line cap
//! while preserving full test coverage.
//! What: system prompt policy checks, prefix stripping, context-block inclusion,
//! response_schema presence, and structured-output language assertions.
//! Test: included as `#[cfg(test)] mod tests` from `prompt.rs`.

use super::*;

// Shared fixture helpers (sample_meta, empty_context, stock_voice).
// Extracted to avoid duplication across prompt_tests.rs / prompt_tests_apex.rs /
// prompt_voice_tests.rs (#760 cleanup finding #4).
#[path = "prompt_test_helpers.rs"]
mod helpers;
use helpers::{empty_context, sample_meta, stock_voice};

#[test]
fn system_prompt_contains_policy() {
    let prompt = reviewer_system_prompt();
    assert!(
        prompt.contains("default verdict is APPROVE"),
        "system prompt must state APPROVE-default policy"
    );
    assert!(
        prompt.contains("REQUEST_CHANGES requires strong evidence on AT LEAST TWO"),
        "system prompt must specify the recalibrated (#1876) REQUEST_CHANGES gate — \
         evidence on 2-of-3 dimensions, not a conjunctive AND of all three"
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

/// Verify the system prompt contains the severity anchors added in grading calibration.
///
/// Why: Fix 5 — explicit severity anchors guide the model to escalate correctly.
/// Without them the model tends to under-rate Critical/High findings as Medium/Low.
/// What: asserts key severity anchor phrases are present in the system prompt.
/// Test: no network.
#[test]
fn system_prompt_contains_severity_anchors() {
    let prompt = reviewer_system_prompt();
    assert!(
        prompt.contains("critical"),
        "system prompt must define the 'critical' severity anchor"
    );
    assert!(
        prompt.contains("severity=critical"),
        "system prompt must instruct model to assign severity=critical for BLOCK issues"
    );
    assert!(
        prompt.contains("Compile-break rule"),
        "system prompt must contain the compile-break BLOCK rule"
    );
    assert!(
        prompt.contains("under-rate"),
        "system prompt must warn against under-rating blocking issues"
    );
}

/// Verify the compile-break BLOCK rule is present in the system prompt.
///
/// Why: Fix 3 — the model must know that removing a symbol while leaving
/// call-sites is a compile-time regression warranting BLOCK.
/// What: asserts the specific compile-break rule text is in the prompt.
/// Test: no network.
#[test]
fn system_prompt_contains_compile_break_rule() {
    let prompt = reviewer_system_prompt();
    assert!(
        prompt.contains("REMOVES a symbol"),
        "system prompt must describe the removed-symbol compile-break pattern"
    );
    assert!(
        prompt.contains("compile-time regression"),
        "system prompt must name it a compile-time regression"
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
        "",
        "bedrock/us.anthropic.claude-sonnet-4-6",
        &stock_voice(),
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
        "",
        "openrouter/openai/gpt-5.4-mini-20260317",
        &stock_voice(),
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
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
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

/// Caller-supplied PR context (#1618) appears in the reviewer user message under
/// the agreed section headings when provided.
///
/// Why: the reviewer must SEE the PR description, author rationale, and referenced
/// code so it can judge the diff against the author's intent — especially on the
/// local-diff path where there is no GitHub fetch.
/// What: builds a context with all three caller fields set, asserts each heading
/// and its body text appear in `messages[0].content`.
#[test]
fn prompt_includes_caller_context() {
    let context = ReviewContext {
        pr_description: Some("Adds a retry guard around the emitter.".to_string()),
        pr_discussion: Some(
            "Author: checked the data source; no values exceed the cap.".to_string(),
        ),
        referenced_code: Some("pub const CAP: u32 = 100;".to_string()),
        ..Default::default()
    };
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &context,
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    );
    let content = &req.messages[0].content;
    assert!(
        content.contains("## PR Description"),
        "reviewer message must include the PR Description heading"
    );
    assert!(
        content.contains("Adds a retry guard around the emitter."),
        "reviewer message must include the PR description body"
    );
    assert!(
        content.contains("## PR Discussion / Author Rationale"),
        "reviewer message must include the PR Discussion heading"
    );
    assert!(
        content.contains("checked the data source"),
        "reviewer message must include the author rationale body"
    );
    assert!(
        content.contains("## Referenced Code"),
        "reviewer message must include the Referenced Code heading"
    );
    assert!(
        content.contains("pub const CAP: u32 = 100;"),
        "reviewer message must include the referenced code body"
    );
}

/// Absent caller context (#1618) produces NO empty sections — back-compat.
///
/// Why: callers that pass no extra context (the existing behaviour) must get a
/// reviewer message with none of the new headings, so nothing regresses.
/// What: builds an empty context, asserts none of the three headings appear and
/// a whitespace-only field is treated as absent.
#[test]
fn prompt_omits_absent_caller_context() {
    // All None.
    let mut context = empty_context();
    // A whitespace-only field must be treated as absent (no empty heading).
    context.pr_discussion = Some("   \n  ".to_string());
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &context,
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    );
    let content = &req.messages[0].content;
    assert!(
        !content.contains("## PR Description"),
        "absent PR description must not render a heading"
    );
    assert!(
        !content.contains("## PR Discussion / Author Rationale"),
        "whitespace-only discussion must not render a heading"
    );
    assert!(
        !content.contains("## Referenced Code"),
        "absent referenced code must not render a heading"
    );
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
        apex_results: vec![],
        coverage_contrib: None,
        ..Default::default()
    };

    let req = build_review_prompt(
        "acme",
        "repo",
        &sample_meta(),
        "+fn foo() {}",
        &context,
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
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

/// Verify external context markdown is embedded into the user message.
///
/// Why: the context orchestrator renders `## Related <source>` markdown that the
/// prompt builder must append verbatim before the structured-output instruction
/// (Phase 6, #550); a regression here would silently drop JIRA/Confluence/GH
/// enrichment from the reviewer prompt.
/// What: passes a non-empty `external_context` block and asserts it appears in
/// the user message ahead of the closing instruction.
/// Test: this test; no network.
#[test]
fn prompt_includes_external_context() {
    let external = "## Related JIRA tickets\n\n- **PROJ-1 — Add auth** — In Progress\n";
    let req = build_review_prompt(
        "acme",
        "backend",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        external,
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    );
    let content = &req.messages[0].content;
    assert!(
        content.contains("## Related JIRA tickets"),
        "external context heading must be embedded"
    );
    assert!(
        content.contains("PROJ-1 — Add auth"),
        "external context bullet must be embedded"
    );
    // The closing instruction must come AFTER the external context.
    let ext_pos = content.find("Related JIRA tickets").unwrap();
    let instr_pos = content.find("populate the structured response").unwrap();
    assert!(
        ext_pos < instr_pos,
        "external context must precede the closing instruction"
    );
}

/// Verify an empty external context block adds nothing.
///
/// Why: out of the box (no Atlassian/GitHub creds) the orchestrator returns an
/// empty string; the prompt must not emit a stray blank section.
/// What: passes an empty `external_context` and asserts no `## Related ` heading
/// for an external source appears.
/// Test: this test; no network.
#[test]
fn prompt_empty_external_context_adds_nothing() {
    let req = build_review_prompt(
        "o",
        "r",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "   \n",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    );
    let content = &req.messages[0].content;
    assert!(!content.contains("Related JIRA"));
    assert!(!content.contains("Related Confluence"));
    assert!(!content.contains("Related GitHub issues"));
}

#[test]
fn prompt_empty_context_omits_sections() {
    let req = build_review_prompt(
        "o",
        "r",
        &sample_meta(),
        "+fn x() {}",
        &empty_context(),
        "",
        "openai/gpt-5.4-nano-20260317",
        &stock_voice(),
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
        "",
        "us.anthropic.claude-sonnet-4-6",
        &stock_voice(),
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
        "",
        "openai/gpt-5.4-mini-20260317",
        &stock_voice(),
    );
    let content = &req.messages[0].content;
    assert!(content.contains("local_fn"));
}

/// Verify the schema enum contains exactly the five board grades with UNKNOWN.
///
/// Why: if UNKNOWN is missing from the schema the model cannot emit it and
/// will fall back to guessing; if N/A is present the board calibration breaks
/// because N/A is not a board grade.
/// What: inspects the `verdict.enum` array in `review_response_schema` and
/// asserts all five board grades are present and N/A is absent.
/// Test: no network.
#[test]
fn review_output_schema_enum_matches_board_grades() {
    let schema = review_response_schema();
    let verdict_enum = &schema.schema["properties"]["verdict"]["enum"];
    let values: Vec<&str> = verdict_enum
        .as_array()
        .expect("verdict enum must be an array")
        .iter()
        .map(|v| v.as_str().expect("enum value must be a string"))
        .collect();

    assert!(values.contains(&"APPROVE"), "schema must have APPROVE");
    assert!(values.contains(&"APPROVE*"), "schema must have APPROVE*");
    assert!(
        values.contains(&"REQUEST_CHANGES"),
        "schema must have REQUEST_CHANGES"
    );
    assert!(values.contains(&"BLOCK"), "schema must have BLOCK");
    assert!(
        values.contains(&"UNKNOWN"),
        "schema must have UNKNOWN (not N/A)"
    );
    assert!(
        !values.contains(&"N/A"),
        "schema must NOT have N/A (not a board grade)"
    );
    assert_eq!(values.len(), 5, "schema must have exactly 5 board grades");
}

// The recursive strict-mode assertion lives in `llm::schema_tests`
// (`assert_object_nodes_strict`) and is re-exported `pub(crate)` from
// `llm::schema`. We reuse it here instead of duplicating the walk so the
// invariant is defined in exactly one place.
use crate::llm::schema::assert_object_nodes_strict as assert_strict;

/// The review response schema must be OpenAI strict-mode compliant top-to-bottom.
///
/// Why: OpenRouter forwards the schema with `strict: true` for `openai/*`
/// models; if ANY object node (top-level OR the nested `findings.items`) omits
/// `additionalProperties:false` or fails to list every property in `required`,
/// OpenAI rejects the request and EVERY OpenAI review fails.  This locks the
/// recursive invariant against regression.
/// What: builds `review_response_schema()` and walks it with `assert_strict`,
/// then spot-checks the previously-broken `findings.items` node directly.
/// Test: no network — pure schema inspection.
#[test]
fn review_schema_is_openai_strict_compliant() {
    let schema = review_response_schema();
    assert_strict(&schema.schema);

    // Spot-check the exact node that was non-compliant before the fix.
    let items = &schema.schema["properties"]["findings"]["items"];
    assert_eq!(
        items["additionalProperties"],
        serde_json::json!(false),
        "findings.items must set additionalProperties:false"
    );
    let required: std::collections::BTreeSet<&str> = items["required"]
        .as_array()
        .expect("findings.items.required array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    // `category` (#1359) MUST be in `required` here: OpenAI strict mode rejects
    // any schema whose object node omits a declared property from `required`.
    // This does NOT make `category` mandatory for non-strict providers — those
    // funnel through the serde path where `LlmFinding.category` is
    // `#[serde(default)]` → `Correctness`, so an omitted `category` is defaulted,
    // never rejected (see `parse_finding_without_category_defaults_correctness`).
    assert_eq!(
        required,
        [
            "body",
            "category",
            "confidence",
            "consequence",
            "file",
            "line",
            "severity",
            "suggested_replacement",
            "title"
        ]
        .into_iter()
        .collect(),
        "findings.items must require every property under strict mode"
    );
}

/// Verify the system prompt describes UNKNOWN.
///
/// Why: the model must know what UNKNOWN means and when to use it; if it is
/// absent from the prompt the model may invent usage semantics.
/// What: asserts the system prompt contains "UNKNOWN" and does not contain "N/A"
/// as a verdict grade.
/// Test: no network.
#[test]
fn system_prompt_describes_unknown_grade() {
    let prompt = reviewer_system_prompt();
    assert!(
        prompt.contains("UNKNOWN"),
        "system prompt must describe the UNKNOWN grade"
    );
    // N/A is no longer a board grade — it must not appear as a verdict option.
    assert!(
        !prompt.contains("N/A"),
        "system prompt must not list N/A as a verdict option"
    );
}

// ── max_tokens model routing (#1241) ──────────────────────────────────────────

#[test]
fn max_tokens_gemini_is_raised() {
    // #1241: Gemini's verbose JSON needs a higher ceiling (8192) so it isn't
    // truncated → UNKNOWN.  Matches the bare slug and prefixed routing forms.
    assert_eq!(max_tokens_for_model("google/gemini-2.5-pro"), 8192);
    assert_eq!(
        max_tokens_for_model("openrouter/google/gemini-2.5-flash"),
        8192
    );
    assert_eq!(
        max_tokens_for_model("GOOGLE/GEMINI-2.5-PRO"),
        8192,
        "match must be case-insensitive"
    );
}

#[test]
fn max_tokens_default_for_non_gemini() {
    // Non-Gemini models keep the leaner 4096 default.
    assert_eq!(max_tokens_for_model("openai/gpt-5.4-mini-20260317"), 4096);
    assert_eq!(
        max_tokens_for_model("bedrock/us.anthropic.claude-sonnet-4-6"),
        4096
    );
}

// ── source_citation instruction (#1419) ──────────────────────────────────────

/// Verify both system prompt variants instruct the LLM to populate
/// `source_citation` when a context snippet carries an explicit source
/// label (#1419).
///
/// Why: the spec-grounding mechanism is inert unless the LLM is told *when*
/// and *how* to populate `source_citation`.  If the instruction is absent,
/// the field will always be `None` regardless of the schema.
/// What: asserts the key instruction phrase is present in both the stock
/// prompt (coverage gating off) and the coverage-gating variant.
/// Test: no network.
#[test]
fn system_prompt_contains_source_citation_instruction() {
    for (label, prompt) in [
        ("stock", reviewer_system_prompt_with_coverage(false)),
        (
            "coverage-gating",
            reviewer_system_prompt_with_coverage(true),
        ),
    ] {
        assert!(
            prompt.contains("source_citation"),
            "{label} system prompt must instruct the LLM to populate source_citation (#1419)"
        );
        assert!(
            prompt.contains("prescribed intent"),
            "{label} system prompt must explain the purpose of source_citation (spec grounding)"
        );
    }
}

// ── APEX prompt tests ─────────────────────────────────────────────────────────
// Extracted to prompt_tests_apex.rs to keep this file under the 500-line cap (#610).
#[path = "prompt_tests_apex.rs"]
mod apex_tests;

// ── Rust false-positive guardrail tests (#1422) ───────────────────────────────

/// Verify both system-prompt variants contain the Rust false-positive guardrail.
///
/// Why: the guardrail section was added to reduce known false positives on Rust
/// `move` closures and `tokio::select!` branches (#1422).  Asserting its presence
/// ensures prompt changes don't accidentally drop it.
/// What: checks both SYSTEM_PROMPT_STOCK and SYSTEM_PROMPT_COVERAGE_GATING for
/// the key guardrail phrases.
/// Test: no network; operates on the constant strings directly.
#[test]
fn both_system_prompts_contain_rust_fp_guardrail() {
    use crate::pipeline::prompt_templates::{SYSTEM_PROMPT_COVERAGE_GATING, SYSTEM_PROMPT_STOCK};

    for (label, prompt) in [
        ("stock", SYSTEM_PROMPT_STOCK),
        ("coverage-gating", SYSTEM_PROMPT_COVERAGE_GATING),
    ] {
        assert!(
            prompt.contains("Known false-positive patterns"),
            "{label} system prompt must contain the 'Known false-positive patterns' section"
        );
        assert!(
            prompt.contains("`move` closures"),
            "{label} system prompt must include the move-closure guardrail"
        );
        assert!(
            prompt.contains("tokio::select!"),
            "{label} system prompt must include the tokio::select! guardrail"
        );
        assert!(
            prompt.contains("DO NOT flag"),
            "{label} system prompt must say DO NOT flag for the guardrail"
        );
    }
}
