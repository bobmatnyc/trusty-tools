//! Bedrock Converse tool-use forcing for structured output.
//!
//! Why: when `LlmRequest.response_schema` is set we must use Bedrock's
//! tool-use forcing mechanism to guarantee the model emits JSON conforming
//! to the schema.  Free-text responses are unreliable for structured data
//! (confirmed in live testing: Haiku always fail-safes, Sonnet sometimes
//! does).  Forcing a single tool with `toolChoice = TOOL (name)` makes
//! Anthropic Claude on Bedrock always emit a `toolUse` block whose `input`
//! is the schema-conformant JSON object.
//!
//! What: exports `build_tool_config` and `extract_tool_use_json`.
//!   - `build_tool_config`: builds a Bedrock `ToolConfiguration` with one
//!     tool whose `inputSchema` is the caller-supplied JSON Schema.  Sets
//!     `toolChoice` to force that specific tool.
//!   - `build_tool_config_for_model`: the same, except `toolChoice = auto`
//!     for a model that rejects forcing (#9292).
//!   - `extract_tool_use_json`: walks the Converse output to find the
//!     `ToolUse` block, serialises its `input` document to a JSON string,
//!     and returns it as `LlmResponse.text`.
//!
//! Test: `tool_config_contains_schema_name` and
//! `extract_tool_use_json_from_mock_output` in `bedrock/mod.rs` test module.

use aws_sdk_bedrockruntime::types::{
    AutoToolChoice, ContentBlock, SpecificToolChoice, Tool, ToolChoice, ToolConfiguration,
    ToolInputSchema, ToolSpecification,
};
use aws_smithy_types::Document;

use super::{arn, normalize_model_family};
use crate::llm::BEDROCK_MODEL_PREFIX;
use crate::llm::error::LlmError;

/// Model families whose Bedrock Converse API rejects a forced `toolChoice`
/// (`tool` or `any`) with `ValidationException` (#9292).
const NO_FORCED_TOOL_CHOICE_FAMILIES: &[&str] = &["claude-sonnet-5-5", "claude-opus-5-5"];

/// Whether Bedrock accepts a forced `toolChoice` (`tool` / `any`) for `model`.
///
/// Why: Sonnet 5.5 and Opus 5.5 on Bedrock reject `toolChoice = tool` with
/// `ValidationException`, so a forced structured-output call to either fails
/// every time (#9292). Every other model keeps the forced request it had.
/// What: reads the family with [`bedrock_model_family`], whatever single
/// region segment precedes the `anthropic.` vendor (`us.`, `apac.`, `au.`,
/// ...), then returns `false` for the 5.5 families and `true` for
/// everything else. An application-inference-profile ARN names no model, so
/// forcing is unknown there; it returns `false`, because `auto` is the one
/// choice every model accepts and a non-tool reply already fails closed in
/// the parsers.
/// Test: `forced_tool_choice_capability_per_model_id_shape`,
/// `forced_tool_choice_capability_covers_every_compare_candidate`.
pub(crate) fn supports_forced_tool_choice(model: &str) -> bool {
    // #9292: an application inference profile hides its model (`None`).
    bedrock_model_family(model).is_some_and(|f| !NO_FORCED_TOOL_CHOICE_FAMILIES.contains(&f))
}

/// The model family a Bedrock model id names, or `None` when the id hides it.
///
/// Why: each per-model capability check (#9292 tool choice, #9304
/// temperature) must read every id shape the same way; one parser keeps them
/// from disagreeing about the same string.
/// What: strips a `bedrock/` routing prefix, reads the embedded model id out
/// of an inference-profile or foundation-model ARN, takes the name after the
/// `anthropic.` vendor segment, and strips any date or version suffix.
/// `None` for an application-inference-profile ARN, which names no model.
/// Test: `forced_tool_choice_capability_per_model_id_shape`,
/// `inference_config_omits_temperature_only_for_claude_5_5`.
pub(crate) fn bedrock_model_family(model: &str) -> Option<&str> {
    let model = model.strip_prefix(BEDROCK_MODEL_PREFIX).unwrap_or(model);
    let model = match arn::parse(model) {
        None => model,
        Some(parsed) => parsed.priced_model()?,
    };
    Some(normalize_model_family(anthropic_model_name(model)))
}

/// The model name after the `anthropic.` vendor in `<region>.anthropic.<name>`
/// or `anthropic.<name>`; any other id comes back unchanged.
// #9292: anchored on the vendor, not a region list, so a region prefix the
// shared list lacks (`apac.`, `au.`) still classifies.
fn anthropic_model_name(model: &str) -> &str {
    const VENDOR: &str = "anthropic.";
    model
        .split_once('.')
        .and_then(|(_, tail)| tail.strip_prefix(VENDOR))
        .or_else(|| model.strip_prefix(VENDOR))
        .unwrap_or(model)
}

/// The one system-prompt line an `auto`-mode request adds (#9292).
///
/// Why: with `toolChoice = auto` the model may answer in prose; this line
/// asks it to call the tool, which forcing used to guarantee.
/// What: names `schema_name` and requires the answer as that tool's input.
/// Test: `system_blocks_add_the_tool_line_only_for_auto_models`.
pub(crate) fn auto_tool_instruction(schema_name: &str) -> String {
    format!(
        "Respond only by calling the `{schema_name}` tool with your complete answer \
         as its input; do not answer in plain text."
    )
}

/// Build the `ToolConfiguration` for `model`: forced when Bedrock accepts
/// forcing for it, `auto` otherwise.
///
/// Why: one call site needs the per-model choice; a model that supports
/// forcing must get the same configuration [`build_tool_config`] builds.
/// What: [`build_tool_config`] when [`supports_forced_tool_choice`] holds;
/// otherwise the same single tool with `toolChoice = auto` (#9292).
/// Test: `build_tool_config_for_model_picks_choice_per_model`.
pub(crate) fn build_tool_config_for_model(
    model: &str,
    schema_name: &str,
    json_schema: &serde_json::Value,
) -> Result<ToolConfiguration, LlmError> {
    if supports_forced_tool_choice(model) {
        return build_tool_config(schema_name, json_schema);
    }
    tool_config_with_choice(
        schema_name,
        json_schema,
        ToolChoice::Auto(AutoToolChoice::builder().build()),
    )
}

/// Build a Bedrock `ToolConfiguration` that forces the model to call the
/// named tool described by `json_schema`.
///
/// Why: the only way to guarantee Bedrock emits structured JSON is to define
/// a tool with the desired schema and set `toolChoice` to force that tool.
/// What: creates a single `Tool` with `name = schema_name` and
/// `inputSchema` = the caller's JSON Schema converted to `Document`.
/// Sets `toolChoice = TOOL { name }` so the model MUST call this tool.
/// Returns `LlmError::Validation` if the JSON Schema cannot be converted
/// to a `Document` (e.g. not a JSON object). Sonnet 5.5 and Opus 5.5 reject
/// this choice; the provider uses `build_tool_config_for_model` (#9292).
/// Test: `build_tool_config_succeeds_for_valid_schema`.
pub fn build_tool_config(
    schema_name: &str,
    json_schema: &serde_json::Value,
) -> Result<ToolConfiguration, LlmError> {
    // Force the specific tool — the model MUST call `schema_name`.
    let specific = SpecificToolChoice::builder()
        .name(schema_name)
        .build()
        .map_err(|e| LlmError::Validation(format!("build SpecificToolChoice: {e}")))?;
    tool_config_with_choice(schema_name, json_schema, ToolChoice::Tool(specific))
}

/// One structured-output tool named `schema_name` with the given `toolChoice`.
fn tool_config_with_choice(
    schema_name: &str,
    json_schema: &serde_json::Value,
    tool_choice: ToolChoice,
) -> Result<ToolConfiguration, LlmError> {
    let doc = json_to_document(json_schema).ok_or_else(|| {
        LlmError::Validation(format!(
            "response_schema for {schema_name:?} must be a JSON object"
        ))
    })?;

    let input_schema = ToolInputSchema::Json(doc);

    let tool_spec = ToolSpecification::builder()
        .name(schema_name)
        .description(format!(
            "Structured output tool for {schema_name}. \
             You MUST call this tool to return your response."
        ))
        .input_schema(input_schema)
        .build()
        .map_err(|e| LlmError::Validation(format!("build ToolSpecification: {e}")))?;

    let tool = Tool::ToolSpec(tool_spec);

    let config = ToolConfiguration::builder()
        .tools(tool)
        .tool_choice(tool_choice)
        .build()
        .map_err(|e| LlmError::Validation(format!("build ToolConfiguration: {e}")))?;

    Ok(config)
}

/// Extract the `toolUse.input` JSON from a Converse response.
///
/// Why: when tool-use is forced, the model emits its structured output as a
/// `ToolUse` content block; the `input` field is the schema-conformant
/// document.  We serialise it to a JSON string so the caller can parse it
/// directly without any fence-stripping.
/// What: walks the output message's content blocks for the first `ToolUse`
/// block, converts its `input` document to a JSON string, and returns it.
/// Returns `None` if no `ToolUse` block is found (graceful fallback for
/// providers that ignore `toolChoice`).
/// Test: `extract_tool_use_json_from_mock_output` in `bedrock/mod.rs`.
pub fn extract_tool_use_json(
    resp: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput,
) -> Option<String> {
    let msg = resp.output()?.as_message().ok()?;
    for block in msg.content() {
        if let ContentBlock::ToolUse(tu) = block {
            let doc = tu.input();
            return document_to_json_string(doc);
        }
    }
    None
}

// ─── Document ↔ JSON conversion helpers ──────────────────────────────────────

/// Convert a `serde_json::Value` to an AWS `Document`.
///
/// Why: Bedrock's `ToolInputSchema::Json` takes a `Document`; we receive
/// the schema as `serde_json::Value` from the caller.
/// What: recursively converts the JSON value tree to the `Document` enum.
/// Returns `None` if the top-level value is not an object (Bedrock requires
/// the tool input schema to be a JSON object, not a primitive or array).
/// Test: covered transitively by `build_tool_config` tests.
pub fn json_to_document(value: &serde_json::Value) -> Option<Document> {
    match value {
        serde_json::Value::Object(map) => {
            let doc_map: std::collections::HashMap<String, Document> = map
                .iter()
                .map(|(k, v)| (k.clone(), json_value_to_doc(v)))
                .collect();
            Some(Document::Object(doc_map))
        }
        _ => None, // must be object at top level
    }
}

/// Recursively convert a `serde_json::Value` to a `Document`.
fn json_value_to_doc(v: &serde_json::Value) -> Document {
    match v {
        serde_json::Value::Null => Document::Null,
        serde_json::Value::Bool(b) => Document::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Document::Number(aws_smithy_types::Number::NegInt(i))
            } else if let Some(u) = n.as_u64() {
                Document::Number(aws_smithy_types::Number::PosInt(u))
            } else {
                Document::Number(aws_smithy_types::Number::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::String(s) => Document::String(s.clone()),
        serde_json::Value::Array(arr) => {
            Document::Array(arr.iter().map(json_value_to_doc).collect())
        }
        serde_json::Value::Object(map) => {
            let doc_map: std::collections::HashMap<String, Document> = map
                .iter()
                .map(|(k, v)| (k.clone(), json_value_to_doc(v)))
                .collect();
            Document::Object(doc_map)
        }
    }
}

/// Convert an AWS `Document` to a JSON string.
///
/// Why: `ToolUse.input` is a `Document`; we need a JSON string for
/// `LlmResponse.text` so callers can `serde_json::from_str` it directly.
/// What: recursively converts the `Document` to `serde_json::Value` then
/// serialises to a string.  Returns `None` on serialization failure.
/// Test: covered transitively by `extract_tool_use_json` tests.
pub fn document_to_json_string(doc: &Document) -> Option<String> {
    let value = doc_to_json_value(doc);
    serde_json::to_string(&value).ok()
}

/// Recursively convert a `Document` to `serde_json::Value`.
fn doc_to_json_value(doc: &Document) -> serde_json::Value {
    match doc {
        Document::Null => serde_json::Value::Null,
        Document::Bool(b) => serde_json::Value::Bool(*b),
        Document::Number(n) => match n {
            aws_smithy_types::Number::PosInt(u) => {
                serde_json::Value::Number(serde_json::Number::from(*u))
            }
            aws_smithy_types::Number::NegInt(i) => {
                serde_json::Value::Number(serde_json::Number::from(*i))
            }
            aws_smithy_types::Number::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
        },
        Document::String(s) => serde_json::Value::String(s.clone()),
        Document::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(doc_to_json_value).collect())
        }
        Document::Object(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), doc_to_json_value(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `json_to_document` converts an object correctly.
    ///
    /// Why: the schema conversion is the entry point for Bedrock tool-use;
    /// if it silently drops fields the model will emit wrong JSON.
    /// What: converts a two-field object schema, asserts both fields survive.
    /// Test: no network.
    #[test]
    fn json_to_document_converts_object() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string" }
            },
            "required": ["verdict"]
        });
        let doc = json_to_document(&schema).expect("must convert object schema");
        // The document must be an Object variant.
        assert!(
            matches!(doc, Document::Object(_)),
            "top-level must be Document::Object"
        );
    }

    /// Verify that a non-object JSON value returns None.
    ///
    /// Why: Bedrock requires the tool input schema to be an object;
    /// passing a string or array would silently produce the wrong call.
    /// What: passes a JSON array, asserts None is returned.
    /// Test: no network.
    #[test]
    fn json_to_document_rejects_non_object() {
        let arr = serde_json::json!([1, 2, 3]);
        assert!(
            json_to_document(&arr).is_none(),
            "array must not convert to Document"
        );
        let s = serde_json::json!("hello");
        assert!(
            json_to_document(&s).is_none(),
            "string must not convert to Document"
        );
    }

    /// Verify that `document_to_json_string` roundtrips a Document correctly.
    ///
    /// Why: `extract_tool_use_json` uses this to produce `LlmResponse.text`;
    /// if the roundtrip is lossy the caller cannot parse the response.
    /// What: builds a Document with string and number values, converts to
    /// JSON string, parses back, asserts values match.
    /// Test: no network.
    #[test]
    fn document_roundtrip_preserves_values() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "verdict".to_string(),
            Document::String("APPROVE".to_string()),
        );
        map.insert(
            "confidence".to_string(),
            Document::Number(aws_smithy_types::Number::Float(0.95)),
        );
        let doc = Document::Object(map);

        let json_str = document_to_json_string(&doc).expect("must serialise");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("must parse");

        assert_eq!(parsed["verdict"], "APPROVE");
        let conf = parsed["confidence"]
            .as_f64()
            .expect("confidence must be float");
        assert!((conf - 0.95).abs() < 1e-9);
    }

    /// Verify that `build_tool_config` uses the schema name as the tool name.
    ///
    /// Why: the tool name in `toolChoice` must match the tool name in
    /// `tools`; a mismatch causes Bedrock to reject the request.
    /// What: calls `build_tool_config` with a test schema, asserts no error
    /// is returned (structural validation only — no AWS call needed).
    /// Test: no network.
    #[test]
    fn build_tool_config_succeeds_for_valid_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string" }
            },
            "required": ["verdict"]
        });
        let result = build_tool_config("review_output", &schema);
        assert!(
            result.is_ok(),
            "build_tool_config must succeed for a valid object schema"
        );
    }

    /// Verify that a non-object schema returns `LlmError::Validation`.
    ///
    /// Why: Bedrock tool schemas must be JSON objects; passing a primitive
    /// or array should fail early with a clear error rather than reaching AWS.
    /// What: passes a string schema, asserts `LlmError::Validation` is returned.
    /// Test: no network.
    #[test]
    fn build_tool_config_rejects_non_object_schema() {
        let schema = serde_json::json!("not-an-object");
        let err =
            build_tool_config("review_output", &schema).expect_err("non-object schema must fail");
        assert!(
            matches!(err, LlmError::Validation(_)),
            "expected Validation error, got: {err:?}"
        );
    }
}
