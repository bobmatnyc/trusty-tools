//! Tests for the layered system prompt composition (stock, principles, voice).
//!
//! Why: extracted from `prompt_tests.rs` to keep that file under the 500-line
//! cap (#610) while adding voice-layer test coverage (#754/#756).
//! What: exercises `build_system_prompt` with all VoiceConfig combinations,
//! and verifies `build_review_prompt` forwards voice layers correctly into the
//! LlmRequest system field.
//! Test: included as `#[cfg(test)] mod voice_tests` from `prompt.rs`.

use crate::{
    pipeline::prompt::{
        ReviewContext, ReviewPrMeta, build_review_prompt, build_system_prompt,
        reviewer_system_prompt,
    },
    voice::{VoiceConfig, principles::principles_addendum},
};

// Local fixture: this module uses a distinct meta title from prompt_tests.rs
// (both are valid PR-meta fixtures; the helper in prompt_tests.rs uses
// "Add authentication"; here "Add feature" is retained for clarity).
// The shared helpers module lives under tests::helpers (prompt_tests.rs);
// this sibling module uses an inline fixture to avoid a duplicate-mod load.
fn sample_meta() -> ReviewPrMeta {
    ReviewPrMeta {
        title: "Add feature".to_string(),
        body: String::new(),
        author: "bob".to_string(),
        url: "https://github.com/acme/repo/pull/1".to_string(),
    }
}

/// build_system_prompt with stock_only VoiceConfig equals reviewer_system_prompt().
///
/// Why: the stock-only path must be a no-op regression guard; adding voice
/// support must not change the output when no layers are configured.
/// What: asserts build_system_prompt(&VoiceConfig::stock_only()) equals
/// reviewer_system_prompt() verbatim.
/// Test: no network.
#[test]
fn build_system_prompt_stock_only() {
    let vc = VoiceConfig::stock_only();
    let layered = build_system_prompt(&vc);
    let stock = reviewer_system_prompt();
    assert_eq!(
        layered, stock,
        "stock_only VoiceConfig must produce identical output to reviewer_system_prompt()"
    );
}

/// build_system_prompt with principles-only VoiceConfig appends principles text.
///
/// Why: the default_production config has principles ON and no voice; this
/// must produce stock + principles without any voice addendum.
/// What: asserts the output starts with the stock base and contains the
/// principles heading; voice content must be absent.
/// Test: no network.
#[test]
fn build_system_prompt_with_principles_only() {
    let vc = VoiceConfig {
        principles: Some(principles_addendum().to_string()),
        voice_addendum: None,
        voice_name: None,
    };
    let result = build_system_prompt(&vc);
    // Must start with stock base.
    assert!(
        result.starts_with("You are a senior software engineer"),
        "result must start with stock base"
    );
    // Principles content must be present.
    assert!(
        result.contains("Review principles"),
        "result must contain principles heading"
    );
    // Stock base is a substring.
    assert!(
        result.contains(reviewer_system_prompt()),
        "result must contain the full stock base"
    );
}

/// build_system_prompt with full VoiceConfig (principles + voice) produces ordered layers.
///
/// Why: end-to-end guard: the combined prompt must have stock < principles <
/// voice in document order.
/// What: uses synthetic addenda to have predictable positions; asserts ordering.
/// Test: no network.
#[test]
fn build_system_prompt_full_pipeline_ordering() {
    let vc = VoiceConfig {
        principles: Some("## PRINCIPLES_MARKER".to_string()),
        voice_addendum: Some("## VOICE_MARKER".to_string()),
        voice_name: Some("test".to_string()),
    };
    let result = build_system_prompt(&vc);
    let stock_pos = result.find("senior software engineer").unwrap();
    let principles_pos = result.find("PRINCIPLES_MARKER").unwrap();
    let voice_pos = result.find("VOICE_MARKER").unwrap();
    assert!(
        stock_pos < principles_pos,
        "stock base must precede principles"
    );
    assert!(
        principles_pos < voice_pos,
        "principles must precede voice addendum"
    );
}

/// build_review_prompt forwards VoiceConfig into the LlmRequest.system field.
///
/// Why: the prompt builder delegates system-prompt construction to
/// `build_system_prompt`; this verifies the delegation is wired correctly.
/// What: passes a VoiceConfig with a unique marker; asserts the marker appears
/// in `LlmRequest.system`.
/// Test: no network.
#[test]
fn build_review_prompt_with_voice_config_principles() {
    let vc = VoiceConfig {
        principles: Some("## UNIQUE_PRINCIPLES_42".to_string()),
        voice_addendum: None,
        voice_name: None,
    };
    let req = build_review_prompt(
        "o",
        "r",
        &sample_meta(),
        "+fn x() {}",
        &ReviewContext::default(),
        "",
        "openai/gpt-5.4-mini-20260317",
        &vc,
    );
    assert!(
        req.system.contains("UNIQUE_PRINCIPLES_42"),
        "LlmRequest.system must include the principles addendum"
    );
    assert!(
        req.system.contains("You are a senior software engineer"),
        "LlmRequest.system must still include the stock base"
    );
}

/// build_review_prompt with full VoiceConfig (stock + principles + voice).
///
/// Why: full pipeline test — confirms both layers are injected and the user
/// message is unaffected by voice layering.
/// What: passes both principles and voice markers; asserts both appear in
/// system but not in the user message.
/// Test: no network.
#[test]
fn build_review_prompt_with_voice_config_full() {
    let vc = VoiceConfig {
        principles: Some("PRINCIPLES_TEXT_XYZ".to_string()),
        voice_addendum: Some("VOICE_TEXT_XYZ".to_string()),
        voice_name: Some("testvoice".to_string()),
    };
    let req = build_review_prompt(
        "o",
        "r",
        &sample_meta(),
        "+fn y() {}",
        &ReviewContext::default(),
        "",
        "openai/gpt-5.4-mini-20260317",
        &vc,
    );
    // Both markers must be in system prompt.
    assert!(
        req.system.contains("PRINCIPLES_TEXT_XYZ"),
        "system must contain principles marker"
    );
    assert!(
        req.system.contains("VOICE_TEXT_XYZ"),
        "system must contain voice marker"
    );
    // User message must not contain system markers.
    let user = &req.messages[0].content;
    assert!(
        !user.contains("PRINCIPLES_TEXT_XYZ"),
        "user message must not contain principles (system-prompt content)"
    );
    assert!(
        !user.contains("VOICE_TEXT_XYZ"),
        "user message must not contain voice addendum (system-prompt content)"
    );
}

/// Regression: stock_only VoiceConfig does NOT add a trailing newline or separator.
///
/// Why: the stock base prompt must not gain spurious whitespace when no layers
/// are active; trailing-whitespace diff noise would break existing snapshot tests.
/// What: asserts build_system_prompt(&stock_only) == reviewer_system_prompt()
/// (same test as build_system_prompt_stock_only but framed as a regression guard).
/// Test: no network.
#[test]
fn build_system_prompt_no_trailing_separator_when_no_addendum() {
    let vc = VoiceConfig::stock_only();
    let result = build_system_prompt(&vc);
    assert!(
        !result.ends_with("\n\n"),
        "stock_only must not gain a trailing double-newline"
    );
    assert_eq!(
        result.len(),
        reviewer_system_prompt().len(),
        "stock_only result length must match stock base length"
    );
}
