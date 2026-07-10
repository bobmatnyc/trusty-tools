//! Unit tests for the pure layer of `core::mcp_test`.
//!
//! Why: selection, transport classification, JSON-RPC message build/parse, and
//! exit-code aggregation must all be verifiable with no live server. The
//! subprocess handshake itself is integration-level and exercised end-to-end by
//! `tm mcp test`; here we drive its message layer against captured fixtures.

use super::*;
use serde_json::json;

// ─── timeout ────────────────────────────────────────────────────────────────

#[test]
fn default_timeout_is_bounded() {
    assert_eq!(DEFAULT_PROBE_TIMEOUT, Duration::from_secs(5));
}

// ─── transport classification ────────────────────────────────────────────────

#[test]
fn classify_transport_explicit() {
    assert_eq!(
        classify_transport(&json!({"type":"stdio","command":"x","args":[]})),
        TestTransport::Stdio
    );
    assert_eq!(
        classify_transport(&json!({"type":"http","url":"https://x"})),
        TestTransport::Http
    );
    assert_eq!(
        classify_transport(&json!({"type":"sse","url":"https://x"})),
        TestTransport::Sse
    );
}

#[test]
fn classify_transport_inferred() {
    // No `type`: a url implies http, otherwise stdio (matches entry_type default).
    assert_eq!(
        classify_transport(&json!({"url":"https://x"})),
        TestTransport::Http
    );
    assert_eq!(
        classify_transport(&json!({"command":"x","args":[]})),
        TestTransport::Stdio
    );
    // Unknown `type` with no url falls back to stdio.
    assert_eq!(
        classify_transport(&json!({"type":"weird"})),
        TestTransport::Stdio
    );
}

// ─── selection ───────────────────────────────────────────────────────────────

fn map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().cloned().unwrap()
}

#[test]
fn select_bare_unions_builtins() {
    // A single user server plus the three built-ins, deduped + sorted.
    let servers = map(json!({
        "aaa-custom": { "type": "stdio", "command": "x", "args": [] }
    }));
    let targets = select_targets(&servers, None).unwrap();
    let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "aaa-custom",
            "trusty-memory",
            "trusty-review",
            "trusty-search"
        ]
    );
    // A built-in absent from the user config resolves via builtin_server_entry.
    let mem = targets.iter().find(|t| t.name == "trusty-memory").unwrap();
    assert_eq!(mem.entry["command"], "trusty-memory");
    assert_eq!(mem.transport, TestTransport::Stdio);
    // The user server keeps its own entry.
    let custom = targets.iter().find(|t| t.name == "aaa-custom").unwrap();
    assert_eq!(custom.entry["command"], "x");
}

#[test]
fn select_bare_dedups_builtin_configured_by_user() {
    // trusty-search is both a built-in AND user-configured: appears once, with
    // the user's entry winning.
    let servers = map(json!({
        "trusty-search": { "type": "stdio", "command": "custom-search", "args": [] }
    }));
    let targets = select_targets(&servers, None).unwrap();
    let matches: Vec<_> = targets
        .iter()
        .filter(|t| t.name == "trusty-search")
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "no duplicate for a builtin+configured name"
    );
    assert_eq!(matches[0].entry["command"], "custom-search");
}

#[test]
fn select_named_uses_config() {
    let servers = map(json!({
        "echo": { "type": "stdio", "command": "echo", "args": ["hi"] }
    }));
    let targets = select_targets(&servers, Some("echo")).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name, "echo");
    assert_eq!(targets[0].entry["command"], "echo");
}

#[test]
fn select_named_falls_back_to_builtin() {
    // Empty user config, but a built-in name resolves to its stdio launch entry.
    let servers = map(json!({}));
    let targets = select_targets(&servers, Some("trusty-review")).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].entry["command"], "trusty-review");
    assert_eq!(targets[0].entry["args"], json!(["serve", "--stdio"]));
}

#[test]
fn select_named_unknown_errors() {
    let servers = map(json!({}));
    let err = select_targets(&servers, Some("nope")).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "clear not-found error"
    );
}

// ─── message builders ────────────────────────────────────────────────────────

#[test]
fn initialize_request_has_protocol_version() {
    let req = initialize_request(1);
    assert_eq!(req["jsonrpc"], "2.0");
    assert_eq!(req["id"], 1);
    assert_eq!(req["method"], "initialize");
    assert_eq!(req["params"]["protocolVersion"], "2024-11-05");
    assert_eq!(req["params"]["clientInfo"]["name"], "tm-mcp-test");
}

#[test]
fn initialized_notification_is_idless() {
    let n = initialized_notification();
    assert_eq!(n["method"], "notifications/initialized");
    assert!(n.get("id").is_none(), "a notification carries no id");
}

#[test]
fn tools_list_request_has_method() {
    let req = tools_list_request(2);
    assert_eq!(req["id"], 2);
    assert_eq!(req["method"], "tools/list");
}

// ─── response parsers (fixtures) ─────────────────────────────────────────────

#[test]
fn validate_initialize_accepts_valid() {
    // Shape mirrors trusty_common::mcp::initialize_response.
    let resp = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "trusty-memory", "version": "0.1.0" }
        }
    });
    assert!(validate_initialize(&resp).is_ok());
}

#[test]
fn validate_initialize_rejects_error_envelope() {
    let resp = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": { "code": -32601, "message": "Method not found" }
    });
    let err = validate_initialize(&resp).unwrap_err();
    assert!(err.contains("-32601"));
    assert!(err.contains("Method not found"));
}

#[test]
fn validate_initialize_rejects_missing_protocol_version() {
    let resp = json!({ "jsonrpc": "2.0", "id": 1, "result": { "capabilities": {} } });
    assert!(validate_initialize(&resp).is_err());
}

#[test]
fn parse_tool_count_counts_array() {
    let resp = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": { "tools": [ {"name":"a"}, {"name":"b"}, {"name":"c"} ] }
    });
    assert_eq!(parse_tool_count(&resp).unwrap(), 3);
    // Empty tool list is still a valid (zero-tool) success.
    let empty = json!({ "jsonrpc":"2.0","id":2,"result":{ "tools": [] } });
    assert_eq!(parse_tool_count(&empty).unwrap(), 0);
}

#[test]
fn parse_tool_count_rejects_error_and_missing() {
    let err = json!({ "jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"boom"} });
    assert!(parse_tool_count(&err).is_err());
    let missing = json!({ "jsonrpc":"2.0","id":2,"result":{} });
    assert!(parse_tool_count(&missing).is_err());
}

// ─── result rendering + aggregation ──────────────────────────────────────────

fn result(name: &str, transport: TestTransport, outcome: McpTestOutcome) -> McpTestResult {
    McpTestResult {
        name: name.to_string(),
        transport,
        outcome,
    }
}

#[test]
fn passed_only_true_for_ok() {
    assert!(
        result(
            "a",
            TestTransport::Stdio,
            McpTestOutcome::Ok { tools: Some(3) }
        )
        .passed()
    );
    assert!(result("b", TestTransport::Http, McpTestOutcome::Ok { tools: None }).passed());
    assert!(!result("c", TestTransport::Stdio, McpTestOutcome::Timeout).passed());
    assert!(
        !result(
            "d",
            TestTransport::Stdio,
            McpTestOutcome::Spawn("nope".into())
        )
        .passed()
    );
}

#[test]
fn render_labels_pass_and_fail() {
    let (label, detail) = result(
        "a",
        TestTransport::Stdio,
        McpTestOutcome::Ok { tools: Some(12) },
    )
    .render();
    assert_eq!(label, "PASS");
    assert_eq!(detail, "12 tools");

    let (label, detail) =
        result("b", TestTransport::Http, McpTestOutcome::Ok { tools: None }).render();
    assert_eq!(label, "PASS");
    assert_eq!(detail, "reachable");

    let (label, detail) = result("c", TestTransport::Stdio, McpTestOutcome::Timeout).render();
    assert_eq!(label, "FAIL");
    assert_eq!(detail, "timeout");
}

#[test]
fn to_json_shapes_ok_and_failure() {
    let ok = result(
        "mem",
        TestTransport::Stdio,
        McpTestOutcome::Ok { tools: Some(7) },
    )
    .to_json();
    assert_eq!(ok["name"], "mem");
    assert_eq!(ok["transport"], "stdio");
    assert_eq!(ok["result"], "ok");
    assert_eq!(ok["tools"], 7);
    assert_eq!(ok["error"], serde_json::Value::Null);
    assert_eq!(ok["passed"], true);

    let fail = result(
        "sentry",
        TestTransport::Http,
        McpTestOutcome::Unreachable("connection refused".into()),
    )
    .to_json();
    assert_eq!(fail["result"], "unreachable");
    assert_eq!(fail["tools"], serde_json::Value::Null);
    assert_eq!(fail["error"], "connection refused");
    assert_eq!(fail["passed"], false);
}

#[test]
fn any_failed_flags_a_single_failure() {
    let all_ok = vec![
        result(
            "a",
            TestTransport::Stdio,
            McpTestOutcome::Ok { tools: Some(1) },
        ),
        result("b", TestTransport::Http, McpTestOutcome::Ok { tools: None }),
    ];
    assert!(!any_failed(&all_ok));

    let with_fail = vec![
        result(
            "a",
            TestTransport::Stdio,
            McpTestOutcome::Ok { tools: Some(1) },
        ),
        result("b", TestTransport::Stdio, McpTestOutcome::Timeout),
    ];
    assert!(any_failed(&with_fail));
}

// ─── integration: real stdio handshake against a live server ─────────────────

/// Full spawn + handshake against a real MCP stdio server.
///
/// Why: the message layer above is fixture-tested; this proves the framing and
/// child lifecycle interoperate with an actual trusty server. Ignored by
/// default (like the ONNX embedder tests) because it needs `trusty-memory` on
/// PATH and its daemon reachable.
/// Run: `cargo test -p trusty-mpm probe_stdio_against_trusty_memory -- --ignored`.
#[tokio::test]
#[ignore = "requires trusty-memory on PATH + reachable daemon"]
async fn probe_stdio_against_trusty_memory() {
    let entry = crate::core::mcp_config::builtin_server_entry("trusty-memory").unwrap();
    let target = make_target("trusty-memory".to_string(), entry);
    let outcome = probe_target(&target, DEFAULT_PROBE_TIMEOUT).await;
    match outcome {
        McpTestOutcome::Ok { tools: Some(n) } => assert!(n > 0, "expected a non-empty tool list"),
        other => panic!("expected Ok with tools, got {other:?}"),
    }
}

/// A spawn failure (nonexistent binary) is reported, not panicked.
///
/// Why: the spawn-error path is cheap to exercise for real and guards the most
/// common failure mode (a server whose command isn't installed). No external
/// dependency, so this runs in CI.
#[tokio::test]
async fn probe_stdio_missing_binary_reports_spawn_error() {
    let entry = json!({
        "type": "stdio",
        "command": "definitely-not-a-real-binary-xyz-123",
        "args": []
    });
    let target = make_target("ghost".to_string(), entry);
    let outcome = probe_target(&target, DEFAULT_PROBE_TIMEOUT).await;
    assert!(
        matches!(outcome, McpTestOutcome::Spawn(_)),
        "expected a spawn error, got {outcome:?}"
    );
}
