//! The `ToolExecutor` adapter over one discovered MCP tool (#5428).

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;
use trusty_common::stdio_mcp_client::{McpTool, StdioMcpClient};

use crate::mcp::composed_name;
use crate::mcp::tool::{
    MAX_INPUT_SCHEMA_BYTES, McpBridgeTool, build_schema, is_mcp_error, render_result,
};
use crate::tools::traits::ToolExecutor;

/// One advertised tool with the given schema.
fn advertised(name: &str, input_schema: Value) -> McpTool {
    McpTool {
        name: name.to_string(),
        description: Some("Does a thing.".to_string()),
        input_schema,
    }
}

/// Why: the registry key and the advertised function name must agree, and the
/// `mcp__` prefix is what keeps a server's tool from shadowing a built-in.
#[test]
fn composed_names_follow_the_claude_code_convention() {
    assert_eq!(
        composed_name("github", "create_issue"),
        "mcp__github__create_issue"
    );
}

/// Why: the server told us the real argument shape, so it must reach the model
/// unmodified rather than being flattened to an open object.
#[test]
fn a_real_input_schema_passes_through() {
    let tool = advertised(
        "echo",
        json!({"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"]}),
    );
    let schema = build_schema("fixture", &tool).expect("schema");
    assert_eq!(schema["function"]["name"], "mcp__fixture__echo");
    assert_eq!(schema["function"]["description"], "[fixture] Does a thing.");
    assert_eq!(
        schema["function"]["parameters"],
        json!({"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"]}),
    );
}

/// Why: a server that advertises no schema is incomplete, not abusive — the
/// tool still registers, with an open-object shape.
#[test]
fn an_absent_input_schema_falls_back_to_an_open_object() {
    let tool = advertised("bare", Value::Null);
    let schema = build_schema("fixture", &tool).expect("schema");
    assert_eq!(
        schema["function"]["parameters"],
        json!({"type": "object", "properties": {}})
    );
}

/// Why: the advertised schema is cloned per tool and re-serialised on every
/// `registry.schemas()` call, so an oversized one is an unbounded cost a
/// hostile or buggy server can impose (#3266). Per the #5428 ruling it is
/// SKIPPED with a reason, not silently downgraded.
#[test]
fn an_oversized_input_schema_is_rejected() {
    let huge: Vec<String> = (0..20_000).map(|i| format!("value-{i}")).collect();
    let tool = advertised(
        "oversized",
        json!({"type": "object", "properties": {"choice": {"enum": huge}}}),
    );
    let reason = build_schema("fixture", &tool).expect_err("must refuse");
    assert!(
        reason.contains(&MAX_INPUT_SCHEMA_BYTES.to_string()),
        "the reason names the cap: {reason}",
    );
}

/// Why: MCP returns content frames and the agent loop takes a string.
#[test]
fn text_frames_are_joined() {
    let result =
        json!({"content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}]});
    assert_eq!(render_result(&result), "one\ntwo");
}

/// Why: a structured-only reply must still be visible; reporting it as empty
/// would tell the model the call did nothing.
#[test]
fn a_frameless_result_renders_as_json() {
    let result = json!({"structuredContent": {"ok": true}});
    let rendered = render_result(&result);
    assert!(rendered.contains("structuredContent"), "{rendered}");
}

/// Why: a server reports a tool-level failure in band, so `call_tool` returns
/// `Ok` and this flag is the only signal the call actually failed. Missing it
/// would report a failure to the model as a success.
#[test]
fn the_in_band_error_flag_is_detected() {
    assert!(is_mcp_error(&json!({"isError": true, "content": []})));
    assert!(!is_mcp_error(&json!({"content": []})));
    assert!(!is_mcp_error(&json!({"isError": false, "content": []})));
}

/// Why: a server can misbehave at any point in a run, and the call must come
/// back as a readable, RECOVERABLE error naming the server and the tool rather
/// than panicking, hanging, or ending the agent loop.
#[tokio::test]
#[cfg(unix)]
async fn execute_surfaces_a_broken_server_recoverably() {
    // `/bin/cat` is alive (so no respawn is attempted) and speaks no MCP: it
    // echoes the request back, which carries no `result` and fails the
    // transport immediately rather than burning the 30s request timeout.
    let client = StdioMcpClient::spawn("/bin/cat", &[], "trusty-code")
        .await
        .expect("/bin/cat spawns");
    let tool = advertised("echo", json!({"type": "object"}));
    let tool = McpBridgeTool::new("ghost", &tool, Arc::new(Mutex::new(client))).expect("schema");

    let result = tool.execute(json!({})).await;

    assert!(result.is_error());
    assert!(!result.is_fatal(), "one broken server must not end the run");
    assert!(
        result.content().contains("ghost") && result.content().contains("echo"),
        "the message identifies what failed: {}",
        result.content(),
    );
    assert_eq!(tool.server(), "ghost");
}
