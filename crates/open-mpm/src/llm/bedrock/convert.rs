//! Bedrock <-> JSON conversion + Converse-response extraction helpers.
//!
//! Why: The Converse API speaks Smithy's `Document` type and typed
//! `ConverseOutput` blocks, while the agent harness works in `serde_json::Value`
//! and the `(content, tool_calls, usage)` shape. Centralizing the translation
//! keeps `client.rs` focused on request orchestration.
//! What: `json_to_document` / `document_to_json` bridge the two value models;
//! `build_tool_config` translates OpenAI-format tools to Bedrock's
//! `ToolConfiguration`; the `extract_*` / `parse_usage` helpers pull the pieces
//! the chat loop needs out of a `ConverseOutput`.
//! Test: See module tests at the bottom.

use anyhow::{Context, Result};
use aws_sdk_bedrockruntime::operation::converse::ConverseOutput;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, Tool, ToolConfiguration, ToolInputSchema, ToolSpecification,
};
use aws_smithy_types::{Document, Number};
use serde_json::Value;
use std::collections::HashMap;

use crate::perf::TokenUsage;

/// Tool-use blocks exposed to the chat loop as a normalized record.
#[derive(Debug, Clone)]
pub struct BedrockToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Convert a `serde_json::Value` to an `aws_smithy_types::Document`.
///
/// Why: Bedrock tool input schemas and tool-call inputs use Smithy's
/// protocol-agnostic `Document` type; our tool registry speaks JSON. A
/// faithful conversion preserves nested objects/arrays without lossy string
/// coercion.
/// What: Recursive walk; numbers map to `Number::PosInt`/`NegInt`/`Float`
/// based on the JSON number shape.
/// Test: `serde_json_to_document_roundtrip`.
pub(super) fn json_to_document(v: &Value) -> Document {
    match v {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Document::Number(Number::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Document::Number(Number::NegInt(i))
            } else if let Some(f) = n.as_f64() {
                Document::Number(Number::Float(f))
            } else {
                Document::Null
            }
        }
        Value::String(s) => Document::String(s.clone()),
        Value::Array(a) => Document::Array(a.iter().map(json_to_document).collect()),
        Value::Object(o) => {
            let map: HashMap<String, Document> = o
                .iter()
                .map(|(k, v)| (k.clone(), json_to_document(v)))
                .collect();
            Document::Object(map)
        }
    }
}

/// Convert an `aws_smithy_types::Document` back to a `serde_json::Value`.
///
/// Why: The model returns tool-call inputs as `Document`; the agent harness
/// dispatches tools using `serde_json::Value`.
/// What: Inverse of `json_to_document`.
/// Test: `serde_json_to_document_roundtrip`.
fn document_to_json(d: &Document) -> Value {
    match d {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::Number(n) => match n {
            Number::PosInt(u) => Value::Number((*u).into()),
            Number::NegInt(i) => Value::Number((*i).into()),
            Number::Float(f) => serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        },
        Document::String(s) => Value::String(s.clone()),
        Document::Array(a) => Value::Array(a.iter().map(document_to_json).collect()),
        Document::Object(o) => {
            let mut map = serde_json::Map::new();
            for (k, v) in o {
                map.insert(k.clone(), document_to_json(v));
            }
            Value::Object(map)
        }
    }
}

/// Build a Bedrock `ToolConfiguration` from OpenAI-format tool definitions.
///
/// Why: Our `ToolRegistry` exposes tools in OpenAI function-calling JSON
/// shape — `{"type":"function","function":{"name","description","parameters"}}`.
/// Bedrock wants `Vec<Tool::ToolSpec(ToolSpecification)>` with the schema as
/// a Smithy `Document`. Translating once here keeps every agent's tools
/// usable on Bedrock without per-tool changes.
/// What: For each tool, extracts `function.name`, `function.description`,
/// and `function.parameters` (defaulting to `{"type":"object"}` when absent),
/// converts the parameters JSON to a `Document`, and wraps everything in a
/// `ToolSpecification`.
/// Test: `build_tool_config_translates_openai_schema`.
pub(super) fn build_tool_config(tools: &[Value]) -> Result<Option<ToolConfiguration>> {
    if tools.is_empty() {
        return Ok(None);
    }
    let mut tool_specs: Vec<Tool> = Vec::with_capacity(tools.len());
    for tool_def in tools {
        // Support both wrapped {function:{...}} and flat {name,description,parameters}.
        let func = tool_def.get("function").unwrap_or(tool_def);
        let name = func
            .get("name")
            .and_then(|v| v.as_str())
            .context("tool definition missing function.name")?
            .to_string();
        let description = func
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = func
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));
        let schema_doc = json_to_document(&params);
        let spec = ToolSpecification::builder()
            .name(name)
            .description(description)
            .input_schema(ToolInputSchema::Json(schema_doc))
            .build()
            .context("failed to build Bedrock ToolSpecification")?;
        tool_specs.push(Tool::ToolSpec(spec));
    }
    let cfg = ToolConfiguration::builder()
        .set_tools(Some(tool_specs))
        .build()
        .context("failed to build Bedrock ToolConfiguration")?;
    Ok(Some(cfg))
}

/// Pull all `Text` blocks out of a Converse response and join with newlines.
pub(super) fn extract_text_from_output(resp: &ConverseOutput) -> Option<String> {
    let msg = resp.output().and_then(|o| o.as_message().ok())?;
    let mut out = String::new();
    for block in msg.content() {
        if let ContentBlock::Text(t) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Pull `ToolUse` blocks out of a Converse response.
pub(super) fn extract_tool_uses(resp: &ConverseOutput) -> Vec<BedrockToolUse> {
    let Some(msg) = resp.output().and_then(|o| o.as_message().ok()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for block in msg.content() {
        if let ContentBlock::ToolUse(tu) = block {
            out.push(BedrockToolUse {
                id: tu.tool_use_id().to_string(),
                name: tu.name().to_string(),
                input: document_to_json(tu.input()),
            });
        }
    }
    out
}

/// Pull a `TokenUsage` out of a Converse response.
pub(super) fn parse_usage(resp: &ConverseOutput) -> TokenUsage {
    if let Some(u) = resp.usage() {
        let p = u.input_tokens().max(0) as u32;
        let c = u.output_tokens().max(0) as u32;
        return TokenUsage::new(p, c, 0, 0);
    }
    TokenUsage::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serde_json_to_document_roundtrip() {
        let original = json!({
            "name": "alice",
            "age": 30,
            "balance": -1.5,
            "active": true,
            "tags": ["a", "b"],
            "meta": null,
        });
        let doc = json_to_document(&original);
        let back = document_to_json(&doc);
        assert_eq!(back["name"], "alice");
        assert_eq!(back["age"], 30);
        assert_eq!(back["active"], true);
        assert_eq!(back["tags"][1], "b");
        assert!(back["meta"].is_null());
    }

    #[test]
    fn build_tool_config_translates_openai_schema() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "search the web",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    },
                    "required": ["query"]
                }
            }
        })];
        let cfg = build_tool_config(&tools).unwrap().unwrap();
        let specs = cfg.tools();
        assert_eq!(specs.len(), 1);
        if let Tool::ToolSpec(spec) = &specs[0] {
            assert_eq!(spec.name(), "search");
            assert_eq!(spec.description(), Some("search the web"));
        } else {
            panic!("expected ToolSpec variant");
        }
    }

    #[test]
    fn build_tool_config_empty_returns_none() {
        let cfg = build_tool_config(&[]).unwrap();
        assert!(cfg.is_none());
    }

    #[test]
    fn build_tool_config_supports_flat_schema() {
        // Tools provided without the {function:{...}} wrapper should still parse.
        let tools = vec![json!({
            "name": "echo",
            "description": "echoes input",
            "parameters": {"type": "object"}
        })];
        let cfg = build_tool_config(&tools).unwrap().unwrap();
        assert_eq!(cfg.tools().len(), 1);
    }
}
