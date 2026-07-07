//! Cross-provider tool-call extraction, validation, and bounded repair (#1023).
//!
//! Why: `ChatResponse::first_tool_calls` (see `llm::response`) only recovers a
//! tool call when the provider populated the native
//! `choices[0].message.tool_calls` wire field. Several model families routed
//! through OpenRouter (Qwen, DeepSeek, Gemma, and any future non-native
//! family — see `provider::traits::Provider::supports_native_tools`) instead
//! emit their intended call as text inside `content`, using one of a few
//! conventions. Before #1023 there was no fallback: `agent_loop::parse_args`
//! silently degraded any unparseable native `arguments` string to `{}`,
//! discarding the model's intent and dispatching the tool with no arguments
//! at all. This module recovers the call across all four representations,
//! validates its arguments against the tool's real schema (the same
//! validation for every provider — `schema::validate_args`), and drives a
//! bounded corrective-retry loop (`repair::extract_with_repair`) when the
//! first attempt fails.
//! What: Re-exports [`ExtractedToolCall`], [`ExtractionStrategy`],
//! [`ToolCallExtractError`], [`ToolCallExtractor`], and the `repair` module's
//! [`repair::extract_with_repair`] / [`repair::DEFAULT_MAX_REPAIR_ATTEMPTS`].
//! [`ToolCallExtractor::extract`] is the single-attempt entry point: try
//! native first, then the fallback strategies in [`strategy_order_for`]'s
//! per-model order, then validate whatever was found against the schema
//! looked up for that tool's name.
//!
//! ## Per-model strategy matrix
//!
//! | Model family (slug substring) | Native tools? | Fallback order (when native absent) |
//! |---|---|---|
//! | `claude-`, `gpt-`, `gemini-` (native families, see `provider::openrouter`) | yes | n/a — native `tool_calls` is authoritative |
//! | `qwen`, `deepseek` | no | `<tool_call>` tag → fenced ```json → tolerant scan |
//! | `gemma`, everything else | no | fenced ```json → `<tool_call>` tag → tolerant scan |
//!
//! Rationale: Qwen's and DeepSeek's published chat templates natively teach
//! the `<tool_call>{...}</tool_call>` convention, so it is tried first for
//! those slugs. Gemma has no standard function-calling convention; a fenced
//! JSON block is the most common prompt-guided pattern across
//! OpenRouter-hosted community fine-tunes, so it leads for the catch-all
//! branch. The tolerant balanced-JSON scan is always last — it is the most
//! expensive and least precise, only used when both structured conventions
//! fail.
//! Test: `tool_call_extractor::tests::*`, `strategies::tests::*`,
//! `schema::tests::*`, `repair::tests::*`.

mod error;
mod repair;
mod schema;
mod strategies;

pub use error::ToolCallExtractError;
pub use repair::{DEFAULT_MAX_REPAIR_ATTEMPTS, extract_with_repair};
pub use schema::SchemaViolation;

use serde_json::Value;

use crate::llm::ChatResponse;

/// Synthetic call ID assigned to a tool call recovered via a text fallback
/// strategy, since none of those conventions carry a wire-level call ID.
const FALLBACK_CALL_ID: &str = "fallback-call-0";

/// Schema-lookup callback type: tool name -> its raw OpenAI-function schema.
///
/// Why: `clippy::type_complexity` flags the inline `Box<dyn Fn(...) -> ...>`
/// form; naming it here also documents the expected shape at the point
/// `ToolCallExtractor` is defined.
/// What: See [`ToolCallExtractor::new`] for the expected schema shape.
type SchemaLookup<'a> = Box<dyn Fn(&str) -> Option<Value> + Send + Sync + 'a>;

/// Which extraction convention recovered a tool call.
///
/// Why: Callers (repair-message building, logging, the per-model ordering
/// table) need to know which strategy succeeded or was attempted.
/// What: `Native` is the OpenAI-wire `tool_calls` field; the other three are
/// the text-based fallbacks in `strategies`.
/// Test: `tests::extract_native_success`, `tests::extract_fenced_json_success`,
/// `tests::extract_angle_bracket_success`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionStrategy {
    /// Recovered from `choices[0].message.tool_calls`.
    Native,
    /// Recovered from a ```` ```json ``` ```` fenced code block.
    FencedJson,
    /// Recovered from an `<tool_call>{...}</tool_call>` tag.
    AngleBracket,
    /// Recovered via the tolerant balanced-`{}`-scan last resort.
    BalancedJsonScan,
}

/// A tool call recovered by [`ToolCallExtractor::extract`], normalised across
/// every extraction strategy and already schema-validated.
///
/// Why: Downstream dispatch (`tools::ToolRegistry::dispatch_gated`) wants a
/// single shape regardless of which strategy produced the call.
/// What: `id` is the wire call ID for `Native`, or [`FALLBACK_CALL_ID`] for
/// any text-based strategy (documented there — those conventions carry no
/// ID). `arguments` is already a parsed, schema-valid `Value`.
/// Test: `tests::extract_native_success` and friends assert every field.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedToolCall {
    /// Call ID to echo back in the subsequent `tool` result message.
    pub id: String,
    /// The tool's registered name.
    pub name: String,
    /// Parsed, schema-validated argument object.
    pub arguments: Value,
    /// Which strategy recovered this call.
    pub strategy: ExtractionStrategy,
}

/// Per-model fallback strategy order, tried when no native tool call is present.
///
/// Why: Centralising the per-family order here (rather than scattering
/// `if slug.contains(...)` checks through `extract`) keeps the matrix
/// documented in exactly one place — see the module-level table above.
/// What: Returns the three text-based strategies in priority order for
/// `model_slug`. Matching is case-insensitive and substring-based, mirroring
/// `provider::openrouter::slug_supports_native_tools`.
/// Test: `tests::strategy_order_prefers_angle_bracket_for_qwen_and_deepseek`,
/// `tests::strategy_order_prefers_fenced_json_for_others`.
pub fn strategy_order_for(model_slug: &str) -> [ExtractionStrategy; 3] {
    let lower = model_slug.to_ascii_lowercase();
    if lower.contains("qwen") || lower.contains("deepseek") {
        [
            ExtractionStrategy::AngleBracket,
            ExtractionStrategy::FencedJson,
            ExtractionStrategy::BalancedJsonScan,
        ]
    } else {
        [
            ExtractionStrategy::FencedJson,
            ExtractionStrategy::AngleBracket,
            ExtractionStrategy::BalancedJsonScan,
        ]
    }
}

/// Recovers and validates tool calls across every supported provider convention.
///
/// Why: Bundles the schema-lookup callback so a single instance can be reused
/// across every attempt of a [`repair::extract_with_repair`] loop without
/// re-threading the tool registry through each call.
/// What: Holds a boxed closure from tool name to that tool's raw
/// OpenAI-function schema `Value` (see `tools::traits::ToolExecutor::schema`;
/// callers typically pass a closure over `ToolRegistry::schemas()` results
/// keyed by name). [`Self::extract`] is the single-attempt entry point.
/// Test: `tests::*`.
pub struct ToolCallExtractor<'a> {
    schema_lookup: SchemaLookup<'a>,
}

impl<'a> ToolCallExtractor<'a> {
    /// Build an extractor bound to a schema-lookup callback.
    ///
    /// Why: Decouples this module from `tools::ToolRegistry` directly, so it
    /// can be unit-tested with a plain closure/`HashMap` lookup.
    /// What: Stores `schema_lookup`; each tool's schema is expected to be the
    /// full `{"type":"function","function":{"name","parameters",...}}`
    /// envelope — [`Self::extract`] reads `.function.parameters` out of it.
    /// Test: `tests::*` all construct via this constructor.
    pub fn new(schema_lookup: impl Fn(&str) -> Option<Value> + Send + Sync + 'a) -> Self {
        Self {
            schema_lookup: Box::new(schema_lookup),
        }
    }

    /// Attempt one extraction across native + fallback strategies, then validate.
    ///
    /// Why: This is the "single attempt" the bounded repair loop iterates:
    /// try native first (authoritative when present), else each fallback
    /// strategy for `model_slug` in order, then validate whatever candidate
    /// was found against its tool's schema.
    /// What: If `response.first_tool_calls()` is non-empty, parses and
    /// validates each as [`ExtractionStrategy::Native`], returning the first
    /// failure encountered (or all calls on full success). Otherwise scans
    /// `response.first_text()` via [`strategy_order_for`]'s fallback order,
    /// stopping at the first strategy that recovers a candidate — even if
    /// that candidate then fails validation, later strategies are NOT tried
    /// (the model committed to a shape; the repair loop, not a different
    /// parse strategy, is the correct next step).
    /// Test: `tests::extract_native_success`, `tests::extract_fenced_json_success`,
    /// `tests::extract_angle_bracket_success`, `tests::extract_no_call_found`,
    /// `tests::extract_native_malformed_arguments`,
    /// `tests::extract_schema_invalid_arguments`, `tests::extract_unknown_tool`.
    pub fn extract(
        &self,
        response: &ChatResponse,
        model_slug: &str,
    ) -> Result<Vec<ExtractedToolCall>, ToolCallExtractError> {
        let native = response.first_tool_calls();
        if !native.is_empty() {
            let mut out = Vec::with_capacity(native.len());
            for call in native {
                let args =
                    self.parse_and_validate(&call.function.name, &call.function.arguments)?;
                out.push(ExtractedToolCall {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: args,
                    strategy: ExtractionStrategy::Native,
                });
            }
            return Ok(out);
        }

        let text = response.first_text().unwrap_or_default();
        let order = strategy_order_for(model_slug);
        for strategy in order {
            let candidate = match strategy {
                ExtractionStrategy::FencedJson => strategies::extract_fenced_json(&text),
                ExtractionStrategy::AngleBracket => strategies::extract_angle_bracket(&text),
                ExtractionStrategy::BalancedJsonScan => {
                    strategies::extract_balanced_json_scan(&text)
                }
                ExtractionStrategy::Native => {
                    unreachable!("strategy_order_for never returns Native")
                }
            };
            if let Some((name, args)) = candidate {
                let validated = self.validate(&name, args, strategy)?;
                return Ok(vec![ExtractedToolCall {
                    id: FALLBACK_CALL_ID.to_string(),
                    ..validated
                }]);
            }
        }

        Err(ToolCallExtractError::NoCallFound {
            tried: order.to_vec(),
        })
    }

    /// Parse a raw JSON arguments string and validate it against `name`'s schema.
    ///
    /// Why: This is the exact per-call operation `Self::extract`'s native
    /// branch needs, factored out so a caller that already has a single known
    /// `(name, raw_arguments)` pair — notably `agent_loop::dispatch_all`,
    /// which dispatches native `ToolCall`s one at a time — can validate
    /// without constructing a `ChatResponse`. This is the #1023 replacement
    /// for the pre-#1023 `agent_loop::parse_args`, which silently degraded
    /// any unparseable `arguments` string to `{}`; a genuine JSON syntax
    /// error now surfaces as a structured, actionable
    /// [`ToolCallExtractError::MalformedArguments`] instead.
    /// What: An empty or all-whitespace `raw_arguments` is treated as `{}`
    /// (many providers send an empty string for a zero-argument call — this
    /// preserves that legacy leniency) and still runs through schema
    /// validation, so a tool with required arguments correctly reports
    /// `SchemaInvalid` rather than silently succeeding. A non-empty string
    /// that fails to parse reports `MalformedArguments`. Returns the
    /// validated `arguments` value on success.
    /// Test: `tests::parse_and_validate_success`,
    /// `tests::parse_and_validate_empty_string_becomes_empty_object`,
    /// `tests::parse_and_validate_malformed_json`.
    pub fn parse_and_validate(
        &self,
        name: &str,
        raw_arguments: &str,
    ) -> Result<Value, ToolCallExtractError> {
        let trimmed = raw_arguments.trim();
        let args: Value = if trimmed.is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(trimmed).map_err(|source| {
                ToolCallExtractError::MalformedArguments {
                    name: name.to_string(),
                    source,
                }
            })?
        };
        self.validate(name, args, ExtractionStrategy::Native)
            .map(|c| c.arguments)
    }

    /// Validate one candidate `(name, args)` pair against its tool's schema.
    ///
    /// Why: Shared by both the native and fallback branches of [`Self::extract`]
    /// so every strategy funnels through the exact same validation (#1023:
    /// "all providers share same validation plumbing").
    /// What: Looks up `name` via `schema_lookup`; `None` → `UnknownTool`.
    /// Reads `schema["function"]["parameters"]` (defaulting to an empty
    /// object schema `{}` when absent, which validates everything) and runs
    /// `schema::validate_args`. Returns a placeholder-ID `ExtractedToolCall`
    /// on success — callers overwrite `id` with the real value.
    /// Test: `tests::extract_unknown_tool`, `tests::extract_schema_invalid_arguments`.
    fn validate(
        &self,
        name: &str,
        args: Value,
        strategy: ExtractionStrategy,
    ) -> Result<ExtractedToolCall, ToolCallExtractError> {
        let schema =
            (self.schema_lookup)(name).ok_or_else(|| ToolCallExtractError::UnknownTool {
                name: name.to_string(),
            })?;
        let parameters = schema
            .get("function")
            .and_then(|f| f.get("parameters"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));

        schema::validate_args(&parameters, &args).map_err(|violations| {
            ToolCallExtractError::SchemaInvalid {
                name: name.to_string(),
                violations,
            }
        })?;

        Ok(ExtractedToolCall {
            id: String::new(),
            name: name.to_string(),
            arguments: args,
            strategy,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash_schema_entry() -> Value {
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

    fn response_with_native_call(name: &str, arguments: &str) -> ChatResponse {
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
                    "function": {{"name": "{name}", "arguments": {arguments:?}}}
                  }}]
                }},
                "finish_reason": "tool_calls"
              }}],
              "usage": {{"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
            }}"#
        );
        serde_json::from_str(&fixture).expect("fixture deserialises")
    }

    fn response_with_text(text: &str) -> ChatResponse {
        let fixture = json!({
            "id": "gen-2",
            "choices": [{
                "message": { "role": "assistant", "content": text, "tool_calls": [] },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        serde_json::from_str(&fixture.to_string()).expect("fixture deserialises")
    }

    /// Native `tool_calls` with valid arguments extracts and validates cleanly.
    ///
    /// Why: The primary, cheapest path — most requests never need a fallback.
    /// What: A response with a valid native call; assert strategy + fields.
    /// Test: this test.
    #[test]
    fn extract_native_success() {
        let resp = response_with_native_call("bash", r#"{"command": "ls"}"#);
        let extractor = extractor_with_bash();
        let calls = extractor
            .extract(&resp, "anthropic/claude-sonnet-4-5")
            .expect("extract");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments, json!({"command": "ls"}));
        assert_eq!(calls[0].strategy, ExtractionStrategy::Native);
    }

    /// A fenced ```json block is recovered when no native call is present.
    ///
    /// Why: Required acceptance-criterion scenario for #1023.
    /// What: A text-only response containing a fenced tool call.
    /// Test: this test.
    #[test]
    fn extract_fenced_json_success() {
        let text = "```json\n{\"name\": \"bash\", \"arguments\": {\"command\": \"pwd\"}}\n```";
        let resp = response_with_text(text);
        let extractor = extractor_with_bash();
        let calls = extractor
            .extract(&resp, "google/gemma-2-27b-it")
            .expect("extract");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].strategy, ExtractionStrategy::FencedJson);
        assert_eq!(calls[0].id, FALLBACK_CALL_ID);
    }

    /// An `<tool_call>` tag is recovered when no native call is present.
    ///
    /// Why: Required acceptance-criterion scenario for #1023.
    /// What: A text-only response using the tag convention, for a Qwen slug
    /// (which prioritises this strategy per `strategy_order_for`).
    /// Test: this test.
    #[test]
    fn extract_angle_bracket_success() {
        let text = "<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"echo hi\"}}</tool_call>";
        let resp = response_with_text(text);
        let extractor = extractor_with_bash();
        let calls = extractor
            .extract(&resp, "qwen/qwen-2.5-coder-32b-instruct")
            .expect("extract");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].strategy, ExtractionStrategy::AngleBracket);
    }

    /// `parse_and_validate` succeeds for well-formed, schema-valid arguments.
    ///
    /// Why: Guards the direct per-call entry point used by
    /// `agent_loop::dispatch_all` (the pre-#1023 `parse_args` replacement).
    /// What: Valid JSON for `bash`; assert the parsed value.
    /// Test: this test.
    #[test]
    fn parse_and_validate_success() {
        let extractor = extractor_with_bash();
        let args = extractor
            .parse_and_validate("bash", r#"{"command": "ls"}"#)
            .expect("valid");
        assert_eq!(args, json!({"command": "ls"}));
    }

    /// An empty (or whitespace-only) arguments string is treated as `{}`.
    ///
    /// Why: Some providers send an empty string for a zero-argument call;
    /// preserving this leniency avoids regressing existing behaviour. It
    /// must still be validated — a required-field violation surfaces rather
    /// than silently succeeding.
    /// What: Empty string against `bash` (which requires `command`) reports
    /// `SchemaInvalid`, not a parse error.
    /// Test: this test.
    #[test]
    fn parse_and_validate_empty_string_becomes_empty_object() {
        let extractor = extractor_with_bash();
        let err = extractor
            .parse_and_validate("bash", "")
            .expect_err("missing required field");
        assert!(matches!(err, ToolCallExtractError::SchemaInvalid { .. }));
    }

    /// A genuinely malformed (non-empty) arguments string reports `MalformedArguments`.
    ///
    /// Why: This is the exact #1023 regression target — previously silently
    /// degraded to `{}` and dispatched anyway.
    /// What: `"not json"` against `bash`.
    /// Test: this test.
    #[test]
    fn parse_and_validate_malformed_json() {
        let extractor = extractor_with_bash();
        let err = extractor
            .parse_and_validate("bash", "not json")
            .expect_err("malformed");
        assert!(
            matches!(err, ToolCallExtractError::MalformedArguments { name, .. } if name == "bash")
        );
    }

    /// No recognisable call anywhere yields a structured `NoCallFound`, not a panic.
    ///
    /// Why: The model may just answer in prose; extraction must fail cleanly.
    /// What: Plain prose text, no native calls.
    /// Test: this test.
    #[test]
    fn extract_no_call_found() {
        let resp = response_with_text("Sure, here's the answer: 42.");
        let extractor = extractor_with_bash();
        let err = extractor
            .extract(&resp, "google/gemma-2-27b-it")
            .expect_err("should fail");
        assert!(matches!(err, ToolCallExtractError::NoCallFound { .. }));
    }

    /// A native call with unparseable JSON arguments reports `MalformedArguments`.
    ///
    /// Why: This is the exact case `agent_loop::parse_args` used to silently
    /// degrade to `{}`; #1023 requires it surface as a structured error instead.
    /// What: Native call whose `arguments` string is invalid JSON.
    /// Test: this test.
    #[test]
    fn extract_native_malformed_arguments() {
        let resp = response_with_native_call("bash", "{not valid json");
        let extractor = extractor_with_bash();
        let err = extractor
            .extract(&resp, "anthropic/claude-sonnet-4-5")
            .expect_err("should fail");
        assert!(
            matches!(err, ToolCallExtractError::MalformedArguments { name, .. } if name == "bash")
        );
    }

    /// A native call to an unregistered tool name reports `UnknownTool`.
    ///
    /// Why: A hallucinated tool name has no schema to validate against.
    /// What: Call a tool absent from the extractor's schema lookup.
    /// Test: this test.
    #[test]
    fn extract_unknown_tool() {
        let resp = response_with_native_call("frobnicate", "{}");
        let extractor = extractor_with_bash();
        let err = extractor
            .extract(&resp, "anthropic/claude-sonnet-4-5")
            .expect_err("should fail");
        assert!(matches!(err, ToolCallExtractError::UnknownTool { name } if name == "frobnicate"));
    }

    /// A native call with valid JSON but schema-invalid arguments reports `SchemaInvalid`.
    ///
    /// Why: Valid JSON that doesn't satisfy the tool's schema (missing
    /// required field) is a distinct, repairable failure mode.
    /// What: Call `bash` with no `command` field.
    /// Test: this test.
    #[test]
    fn extract_schema_invalid_arguments() {
        let resp = response_with_native_call("bash", "{}");
        let extractor = extractor_with_bash();
        let err = extractor
            .extract(&resp, "anthropic/claude-sonnet-4-5")
            .expect_err("should fail");
        assert!(matches!(err, ToolCallExtractError::SchemaInvalid { name, .. } if name == "bash"));
    }

    /// Qwen and DeepSeek slugs prioritise the angle-bracket strategy.
    ///
    /// Why: Guards the documented per-model matrix stays correct as the table
    /// evolves.
    /// What: Assert the first element of the order for representative slugs.
    /// Test: this test.
    #[test]
    fn strategy_order_prefers_angle_bracket_for_qwen_and_deepseek() {
        for slug in ["qwen/qwen-2.5-coder-32b-instruct", "deepseek/deepseek-chat"] {
            assert_eq!(
                strategy_order_for(slug)[0],
                ExtractionStrategy::AngleBracket
            );
        }
    }

    /// Gemma and any other non-native family prioritise fenced-JSON.
    ///
    /// Why: Guards the catch-all branch of the matrix.
    /// What: Assert the first element for Gemma and an arbitrary other slug.
    /// Test: this test.
    #[test]
    fn strategy_order_prefers_fenced_json_for_others() {
        for slug in ["google/gemma-2-27b-it", "mistralai/mixtral-8x7b"] {
            assert_eq!(strategy_order_for(slug)[0], ExtractionStrategy::FencedJson);
        }
    }
}
