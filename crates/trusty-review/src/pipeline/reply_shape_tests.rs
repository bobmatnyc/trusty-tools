//! Tests for the parse-failure reply shape (#9310).
//!
//! Test: included as `#[cfg(test)] mod tests` from `reply_shape.rs`.

use super::*;

/// An `LlmResponse` with `text`, a stop reason, and an output-token count.
fn response(text: &str, finish_reason: Option<&str>, output_tokens: u32) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "test-model".to_string(),
        input_tokens: 10,
        output_tokens,
        latency_ms: 1,
        cost_usd: 0.0,
        finish_reason: finish_reason.map(str::to_string),
    }
}

/// The shape names the stop reason and token count, and bounds both snippets.
///
/// Why: the harness row must say how the reply ended and how it starts and
/// finishes, without carrying a 4 KB reply.
/// What: a 500-character reply with distinct start and end markers.
/// Test: this test.
#[test]
fn describe_reply_carries_stop_tokens_and_bounded_snippets() {
    let text = format!("START {}END", "word ".repeat(100));
    let shape = describe_reply(&response(&text, Some("end_turn"), 812));
    assert!(
        shape.starts_with("reply shape: stop=end_turn output_tokens=812 text_chars="),
        "{shape}"
    );
    assert!(shape.contains(r#"head="START word"#), "{shape}");
    assert!(shape.contains(r#"word END""#), "{shape}");
    assert!(!shape.contains('\n'), "the shape is one line: {shape}");
    // Two 80-character snippets, the quotes, and the fixed prefix.
    assert!(shape.len() < 300, "the shape is bounded: {}", shape.len());
}

/// A credential-shaped token never reaches the shape (#9310).
///
/// Why: the shape goes to a log and a result field; a key the model echoed
/// must not ride along.
/// What: an AWS-style key id at the head and an `sk-` key at the tail.
/// Test: this test.
#[test]
fn describe_reply_masks_credential_shaped_tokens() {
    let aws = "AKIAIOSFODNN7EXAMPLE";
    let sk = "sk-ant-api03-abcdefghij0123456789";
    let text = format!("{aws} leaked here. {} and {sk}", "filler ".repeat(40));
    let shape = describe_reply(&response(&text, None, 5));
    assert!(!shape.contains(aws), "{shape}");
    assert!(!shape.contains(sk), "{shape}");
    assert!(shape.contains("[masked 20 chars]"), "{shape}");
    assert!(
        shape.contains("stop=none"),
        "a missing reason reads none: {shape}"
    );
}

/// A short reply is all head, and plain identifiers are kept.
///
/// What: a one-line reply with a long snake_case identifier.
/// Test: this test.
#[test]
fn describe_reply_short_text_is_all_head() {
    let shape = describe_reply(&response(
        "  try_parse_direct_json_happy_path APPROVE\n",
        None,
        3,
    ));
    assert!(
        shape.ends_with(r#"head="try_parse_direct_json_happy_path APPROVE" tail="""#),
        "{shape}"
    );
}
