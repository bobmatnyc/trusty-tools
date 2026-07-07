//! Bounded corrective-retry loop for tool-call extraction (#1023).
//!
//! Why: A single extraction attempt (`ToolCallExtractor::extract`) can fail
//! for reasons a model can often fix if told exactly what was wrong — broken
//! JSON, a missing required field, an unknown tool name. Silently giving up
//! (or worse, silently substituting `{}` as `agent_loop::parse_args` did
//! pre-#1023) discards a recoverable turn. This loop re-attempts extraction
//! after sending the model a precise corrective message, bounded by
//! `max_attempts` so a persistently broken model cannot loop forever.
//! What: [`extract_with_repair`] drives the loop; [`build_corrective_message`]
//! renders a [`ToolCallExtractError`] into the text sent back to the model.
//! The loop is generic over how a "retry" is performed (`retry: F`) — callers
//! typically close over an `Arc<dyn LlmClientTrait>` and the running
//! transcript, but tests can supply a canned sequence of responses with no
//! network dependency at all.
//! Test: `repair::tests::*` — covers "malformed → one repair → success" and
//! "unrepairable → structured error (no panic)", the two repair-loop
//! acceptance-criterion scenarios from #1023.

use std::future::Future;

use super::error::ToolCallExtractError;
use super::{ExtractedToolCall, ToolCallExtractor};
use crate::llm::ChatResponse;

/// Default cap on corrective round-trips before giving up.
///
/// Why: A concrete default lets call sites opt in to the bounded loop without
/// having to pick a number themselves; 2 retries (3 total attempts) matches
/// this crate's general "bounded but generous" philosophy (compare
/// `AgentLoopConfig::default`'s turn cap) while keeping a persistently broken
/// model from burning the whole run's turn budget on one tool call.
/// Test: `repair::tests::default_is_two`.
pub const DEFAULT_MAX_REPAIR_ATTEMPTS: u32 = 2;

/// Render one extraction failure into a corrective message for the model.
///
/// Why: A repair round-trip only helps if the model is told EXACTLY what was
/// wrong and how to fix it; a generic "that didn't work" message wastes the
/// retry.
/// What: Matches every non-terminal [`ToolCallExtractError`] variant and
/// produces an actionable instruction naming the tool and the precise
/// violation(s). `Unrepairable` has no sensible corrective text (it is the
/// loop's own terminal output, never fed back in) — it renders a generic
/// fallback that should never actually be reached in practice.
/// Test: `repair::tests::corrective_message_names_missing_field`,
/// `repair::tests::corrective_message_for_malformed_json`.
pub fn build_corrective_message(error: &ToolCallExtractError) -> String {
    match error {
        ToolCallExtractError::NoCallFound { tried } => format!(
            "Your previous response did not contain a recognisable tool call. \
             Please emit exactly one tool call using either the native function-call \
             mechanism, a ```json fenced block shaped like {{\"name\": \"<tool>\", \"arguments\": {{...}}}}, \
             or a <tool_call>{{...}}</tool_call> tag. (Already checked and found nothing: {tried:?}.)"
        ),
        ToolCallExtractError::MalformedArguments { name, source } => format!(
            "Your call to '{name}' had arguments that are not valid JSON ({source}). \
             Please re-emit the call to '{name}' with syntactically valid JSON arguments."
        ),
        ToolCallExtractError::UnknownTool { name } => format!(
            "'{name}' is not a recognised tool in this conversation. \
             Please re-emit your call using one of the tools already provided to you."
        ),
        ToolCallExtractError::SchemaInvalid { name, violations } => {
            let details = violations
                .iter()
                .map(|v| format!("- {v}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Your call to '{name}' had invalid arguments:\n{details}\n\
                 Please re-emit the call to '{name}' with corrected arguments that satisfy every point above."
            )
        }
        ToolCallExtractError::Retry(source) => format!(
            "A transient error occurred while requesting a repair ({source}). Please try again."
        ),
        ToolCallExtractError::Unrepairable { .. } => {
            "the previous tool call could not be repaired".to_string()
        }
    }
}

/// Drive the bounded extract → (on failure) corrective-retry loop.
///
/// Why: Centralises the "try, and if it fails, tell the model precisely what
/// to fix and try again, up to N times" control flow so every caller (today:
/// none directly — `agent_loop` uses the same-turn tool-result-feedback path
/// for its native-argument repairs; future: any caller that wants an
/// explicit, immediate corrective round-trip rather than waiting for the next
/// natural turn) gets identical, tested bounding behaviour.
/// What: Calls `extractor.extract(&response, model_slug)`. On success,
/// returns the calls immediately. On failure, if `attempts` has already
/// reached `max_attempts`, returns `Unrepairable { attempts, last_error }` —
/// a structured error, never a panic. Otherwise builds a corrective message
/// via [`build_corrective_message`], invokes `retry(corrective)` to obtain
/// the next `ChatResponse`, increments `attempts`, and loops.
/// Test: `repair::tests::malformed_then_one_repair_succeeds`,
/// `repair::tests::unrepairable_after_max_attempts_returns_structured_error`,
/// `repair::tests::succeeds_immediately_without_calling_retry`.
pub async fn extract_with_repair<F, Fut>(
    extractor: &ToolCallExtractor<'_>,
    mut response: ChatResponse,
    model_slug: &str,
    max_attempts: u32,
    mut retry: F,
) -> Result<Vec<ExtractedToolCall>, ToolCallExtractError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<ChatResponse, ToolCallExtractError>>,
{
    let mut attempts = 0u32;
    loop {
        match extractor.extract(&response, model_slug) {
            Ok(calls) => return Ok(calls),
            Err(err) => {
                if attempts >= max_attempts {
                    return Err(ToolCallExtractError::Unrepairable {
                        attempts,
                        last_error: Box::new(err),
                    });
                }
                let corrective = build_corrective_message(&err);
                attempts += 1;
                response = retry(corrective).await?;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    fn bash_schema_entry() -> serde_json::Value {
        json!({
            "type": "function",
            "function": {
                "name": "bash",
                "parameters": {
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        })
    }

    fn extractor_with_bash() -> ToolCallExtractor<'static> {
        ToolCallExtractor::new(|name| match name {
            "bash" => Some(bash_schema_entry()),
            _ => None,
        })
    }

    fn response_with_native_call(arguments: &str) -> ChatResponse {
        let fixture = format!(
            r#"{{
              "id": "gen-1",
              "choices": [{{
                "message": {{
                  "role": "assistant",
                  "content": null,
                  "tool_calls": [{{
                    "id": "call_1",
                    "type": "function",
                    "function": {{"name": "bash", "arguments": {arguments:?}}}
                  }}]
                }},
                "finish_reason": "tool_calls"
              }}],
              "usage": {{"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
            }}"#
        );
        serde_json::from_str(&fixture).expect("fixture deserialises")
    }

    /// The repair loop's default cap is 2 (3 total attempts).
    ///
    /// Why: Guards the documented default against accidental drift.
    /// What: Trivial constant assertion.
    /// Test: this test.
    #[test]
    fn default_is_two() {
        assert_eq!(DEFAULT_MAX_REPAIR_ATTEMPTS, 2);
    }

    /// A corrective message for a missing required field names the field.
    ///
    /// Why: The whole point of the message is to be actionable.
    /// What: Build a `SchemaInvalid` error and check the rendered text.
    /// Test: this test.
    #[test]
    fn corrective_message_names_missing_field() {
        let err = ToolCallExtractError::SchemaInvalid {
            name: "bash".into(),
            violations: vec![super::super::SchemaViolation {
                path: "$".into(),
                message: "missing required property 'command'".into(),
            }],
        };
        let msg = build_corrective_message(&err);
        assert!(msg.contains("bash"));
        assert!(msg.contains("command"));
    }

    /// A corrective message for malformed JSON names the tool and the parse error.
    ///
    /// Why: Distinct failure mode from schema mismatch; message must differ.
    /// What: Build a `MalformedArguments` error from a real parse failure.
    /// Test: this test.
    #[test]
    fn corrective_message_for_malformed_json() {
        let parse_err = serde_json::from_str::<serde_json::Value>("{not json").unwrap_err();
        let err = ToolCallExtractError::MalformedArguments {
            name: "bash".into(),
            source: parse_err,
        };
        let msg = build_corrective_message(&err);
        assert!(msg.contains("bash"));
        assert!(msg.contains("valid JSON"));
    }

    /// Extraction that succeeds on the first attempt never calls `retry`.
    ///
    /// Why: The loop must not waste a round-trip when nothing is wrong.
    /// What: A valid native call; `retry` panics if invoked.
    /// Test: this test.
    #[tokio::test]
    async fn succeeds_immediately_without_calling_retry() {
        let extractor = extractor_with_bash();
        let response = response_with_native_call(r#"{"command": "ls"}"#);
        let result = extract_with_repair(
            &extractor,
            response,
            "anthropic/claude-sonnet-4-5",
            DEFAULT_MAX_REPAIR_ATTEMPTS,
            |_corrective| async { panic!("retry must not be called on first-attempt success") },
        )
        .await
        .expect("should succeed without repair");
        assert_eq!(result[0].name, "bash");
    }

    /// A malformed first attempt, corrected on the first retry, succeeds.
    ///
    /// Why: This is the #1023 acceptance-criterion scenario "malformed → one
    /// repair → success".
    /// What: First response has invalid args (missing `command`); the retry
    /// closure returns a corrected response with valid args; assert success
    /// with `attempts` reflected via a call counter of exactly 1.
    /// Test: this test.
    #[tokio::test]
    async fn malformed_then_one_repair_succeeds() {
        let extractor = extractor_with_bash();
        let first = response_with_native_call("{}"); // missing required `command`
        let call_count = AtomicU32::new(0);

        let result = extract_with_repair(
            &extractor,
            first,
            "anthropic/claude-sonnet-4-5",
            DEFAULT_MAX_REPAIR_ATTEMPTS,
            |corrective| {
                call_count.fetch_add(1, Ordering::SeqCst);
                assert!(
                    corrective.contains("command"),
                    "corrective message: {corrective}"
                );
                async { Ok(response_with_native_call(r#"{"command": "ls"}"#)) }
            },
        )
        .await
        .expect("should succeed after one repair");

        assert_eq!(result[0].name, "bash");
        assert_eq!(result[0].arguments, json!({"command": "ls"}));
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "exactly one repair round-trip"
        );
    }

    /// A persistently invalid response exhausts the attempt budget and
    /// returns a structured `Unrepairable` error — never a panic.
    ///
    /// Why: This is the #1023 acceptance-criterion scenario "unrepairable →
    /// structured error (no panic)".
    /// What: `retry` always returns the same invalid response; assert the
    /// loop terminates with `Unrepairable { attempts: max_attempts, .. }`
    /// after exactly `max_attempts` retry calls (not more, not fewer).
    /// Test: this test.
    #[tokio::test]
    async fn unrepairable_after_max_attempts_returns_structured_error() {
        let extractor = extractor_with_bash();
        let call_count = AtomicU32::new(0);
        let max_attempts = 2;

        let err = extract_with_repair(
            &extractor,
            response_with_native_call("{}"),
            "anthropic/claude-sonnet-4-5",
            max_attempts,
            |_corrective| {
                call_count.fetch_add(1, Ordering::SeqCst);
                let resp = response_with_native_call("{}");
                async move { Ok(resp) }
            },
        )
        .await
        .expect_err("should be unrepairable");

        match err {
            ToolCallExtractError::Unrepairable {
                attempts,
                last_error,
            } => {
                assert_eq!(attempts, max_attempts);
                assert!(matches!(
                    *last_error,
                    ToolCallExtractError::SchemaInvalid { .. }
                ));
            }
            other => panic!("expected Unrepairable, got {other:?}"),
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            max_attempts,
            "retry must be called exactly max_attempts times"
        );
    }

    /// A `Retry` failure (the retry closure's own chat call failing) propagates
    /// immediately rather than being retried again.
    ///
    /// Why: A transport/API failure during the repair round-trip itself is not
    /// something another repair attempt can fix; it must surface, not loop.
    /// What: `retry` returns `Err(ToolCallExtractError::Retry(..))`; assert it
    /// propagates unchanged via `?`.
    /// Test: this test.
    #[tokio::test]
    async fn retry_failure_propagates_immediately() {
        let extractor = extractor_with_bash();
        let first = response_with_native_call("{}");

        let err = extract_with_repair(
            &extractor,
            first,
            "anthropic/claude-sonnet-4-5",
            DEFAULT_MAX_REPAIR_ATTEMPTS,
            |_corrective| async {
                Err(ToolCallExtractError::UnknownTool {
                    name: "irrelevant".into(),
                })
            },
        )
        .await
        .expect_err("should propagate the retry error");

        assert!(matches!(err, ToolCallExtractError::UnknownTool { .. }));
    }
}
