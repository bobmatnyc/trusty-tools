//! Unit tests for `discovery.rs` (#453, #3577).
//!
//! Why: Extracted to a sibling file to keep `discovery.rs` under the
//! 500-SLOC production cap once `parse_manifest`/`method_to_tool` were
//! added for #3577; see `scripts/check_line_cap.sh`.
//! What: Struct round-trip + TTL cache tests (pre-existing), plus
//! `parse_manifest` wire-shape coverage: the real gworkspace OpenRPC
//! payload shape, the legacy `tools`-shape fallback, and the
//! loud-error-not-silent-empty regression guard for #3577.
//! Test: This *is* the test module.

use super::*;

#[test]
fn discovered_tool_roundtrip() {
    let raw = serde_json::json!({
        "name": "gmail_read",
        "description": "Read mail",
        "scope": "google.gmail.read",
        "input_schema": {"type": "object"},
        "idempotent": true,
        "side_effects": "read"
    });
    let t: DiscoveredTool = serde_json::from_value(raw).unwrap();
    assert_eq!(t.name, "gmail_read");
    assert_eq!(t.scope, "google.gmail.read");
    assert!(t.idempotent);
    assert_eq!(t.side_effects, SideEffects::Read);
}

#[test]
fn cache_returns_value_within_ttl() {
    let cache = DiscoveryCache::new(Duration::from_secs(60));
    let m = EndpointManifest {
        server: ServerInfo::default(),
        protocol_version: "openrpc/1".into(),
        capabilities: EndpointCapabilities::default(),
        tools: vec![],
    };
    cache.put("ep".into(), m);
    assert!(cache.get("ep").is_some());
}

#[test]
fn cache_misses_on_unknown_key() {
    let cache = DiscoveryCache::new(Duration::from_secs(60));
    assert!(cache.get("missing").is_none());
}

/// The real `trusty-gworkspace-mcp --rpc` `rpc.discover` payload shape,
/// quoted from `crates/trusty-gworkspace/src/openrpc.rs`
/// (`discover_response()` lines ~133-152, `tool_to_method()` lines
/// ~166-213): top-level `openrpc`/`info`/`methods`, each method carrying
/// `params`/`result`/`x-google-scopes`. This is a fixture captured from
/// the actual emitted format, not an idealized one — three tools
/// exercising three distinct scope-mapping paths: a single-scope gmail
/// tool, a multi-scope gmail tool (`compose_email` requests both
/// `gmail.send` and `gmail.modify`), and a docs tool that also requests
/// the generic `drive` scope (exercising the priority rule in
/// `google_scope.rs`).
#[test]
fn parse_manifest_methods_shape_gworkspace_fixture() {
    let raw = serde_json::json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "gworkspace-mcp",
            "version": "0.4.2",
            "description": "Google Workspace tools exposed as JSON-RPC 2.0 methods over stdio.",
            "license": {
                "name": "Elastic-2.0",
                "url": "https://www.elastic.co/licensing/elastic-license"
            }
        },
        "methods": [
            {
                "name": "search_gmail_messages",
                "description": "Search Gmail messages",
                "params": [
                    {
                        "name": "query",
                        "description": "Gmail search query",
                        "required": true,
                        "schema": {"type": "string"}
                    },
                    {
                        "name": "max_results",
                        "description": "Maximum results to return",
                        "required": false,
                        "schema": {"type": "integer"}
                    }
                ],
                "result": {
                    "name": "search_gmail_messages_result",
                    "description": "Tool result envelope; structure varies by tool.",
                    "schema": {"type": "object"}
                },
                "x-google-scopes": ["https://www.googleapis.com/auth/gmail.modify"]
            },
            {
                "name": "compose_email",
                "description": "Compose and send an email",
                "params": [
                    {
                        "name": "to",
                        "description": "Recipient address",
                        "required": true,
                        "schema": {"type": "string"}
                    }
                ],
                "result": {
                    "name": "compose_email_result",
                    "description": "Tool result envelope; structure varies by tool.",
                    "schema": {"type": "object"}
                },
                "x-google-scopes": [
                    "https://www.googleapis.com/auth/gmail.send",
                    "https://www.googleapis.com/auth/gmail.modify"
                ]
            },
            {
                "name": "create_document",
                "description": "Create a Google Doc",
                "params": [],
                "result": {
                    "name": "create_document_result",
                    "description": "Tool result envelope; structure varies by tool.",
                    "schema": {"type": "object"}
                },
                "x-google-scopes": [
                    "https://www.googleapis.com/auth/documents",
                    "https://www.googleapis.com/auth/drive"
                ]
            }
        ]
    });

    let manifest = parse_manifest("gworkspace", &raw).expect("real gworkspace shape must parse");

    // This is the #3577 headline assertion: pre-fix, this manifest would
    // deserialize successfully with `tools: vec![]` because `methods` (not
    // `tools`) was the top-level key. Post-fix, all three tools survive.
    assert_eq!(manifest.tools.len(), 3, "expected all 3 methods to convert");
    assert_eq!(manifest.server.name, "gworkspace-mcp");

    let search = manifest
        .tools
        .iter()
        .find(|t| t.name == "search_gmail_messages")
        .expect("search_gmail_messages present");
    assert_eq!(search.scope, "google.gmail.write");
    assert_eq!(search.input_schema["type"], "object");
    assert_eq!(search.input_schema["properties"]["query"]["type"], "string");
    assert_eq!(
        search.input_schema["required"],
        serde_json::json!(["query"])
    );

    let compose = manifest
        .tools
        .iter()
        .find(|t| t.name == "compose_email")
        .expect("compose_email present");
    assert_eq!(
        compose.scope, "google.gmail.write",
        "multiple gmail scopes collapse to one dotted scope"
    );

    let create_doc = manifest
        .tools
        .iter()
        .find(|t| t.name == "create_document")
        .expect("create_document present");
    assert_eq!(
        create_doc.scope, "google.docs.write",
        "documents scope must win over the generic drive scope also requested"
    );
}

/// The legacy `{"tools": [...]}` shape (originally speced in
/// `docs/trusty-agents/research/openrpc-trusty-contract.md`, never
/// implemented by any real server) is still accepted so a future/test
/// endpoint that emits it directly is not broken by this fix.
#[test]
fn parse_manifest_tools_shape_still_accepted() {
    let raw = serde_json::json!({
        "server": {"name": "trusty-memory", "version": "0.4.0"},
        "protocol_version": "openrpc/1",
        "capabilities": {"supports_batch": true, "supports_streaming": false},
        "tools": [
            {
                "name": "memory_remember",
                "description": "Persist content",
                "scope": "memory.write",
                "input_schema": {"type": "object"},
                "idempotent": false,
                "side_effects": "write"
            }
        ]
    });

    let manifest = parse_manifest("trusty-memory", &raw).expect("legacy tools shape must parse");
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, "memory_remember");
    assert_eq!(manifest.tools[0].scope, "memory.write");
}

/// Regression guard for #3577: a payload with neither `methods` nor
/// `tools` must produce a LOUD error, not a silent empty tool list.
///
/// Causality, confirmed explicitly: the first assertion below exercises
/// the exact PRE-FIX code path (`serde_json::from_value::<EndpointManifest>`
/// — still valid since the struct's `Deserialize` impl is unchanged) and
/// shows it succeeds with `tools: vec![]` on this input. The second
/// assertion exercises `parse_manifest`, the POST-FIX path used by
/// `DirectDriver::discover()`, and shows it errors instead. Running this
/// test against pre-fix `direct.rs` (which called
/// `serde_json::from_value` directly, as the first assertion still does
/// here) would fail at the second assertion because `parse_manifest`
/// itself is new in this change — i.e. the second assertion is exactly
/// the behavior that did not exist before this fix.
#[test]
fn parse_manifest_unrecognized_shape_errors_loudly() {
    let raw = serde_json::json!({
        "openrpc": "1.3.2",
        "info": {"title": "some-future-server", "version": "0.1.0"},
        // Neither `methods` nor `tools` — an unrecognized/future shape.
        "endpoints": [{"name": "foo"}]
    });

    // Pre-fix behavior, still reachable via the derived Deserialize impl:
    // silently succeeds with an empty tool list.
    let legacy: EndpointManifest = serde_json::from_value(raw.clone())
        .expect("pre-fix path: struct deserialize silently succeeds on this input");
    assert!(
        legacy.tools.is_empty(),
        "pre-fix path silently produces zero tools — this is the #3577 bug"
    );

    // Fix: parse_manifest must recognize this as an unrecognized shape
    // and error loudly instead of returning the same empty manifest.
    let err = parse_manifest("gworkspace", &raw)
        .expect_err("unrecognized shape must error, not silently produce zero tools");
    let msg = err.to_string();
    assert!(
        msg.contains("gworkspace"),
        "error must name the endpoint: {msg}"
    );
    assert!(
        msg.contains("methods") && msg.contains("tools"),
        "error must name both the expected and missing shapes: {msg}"
    );
}

#[test]
fn parse_manifest_methods_not_an_array_errors() {
    let raw = serde_json::json!({"methods": "not-an-array"});
    let err = parse_manifest("bad-endpoint", &raw).unwrap_err();
    assert!(err.to_string().contains("bad-endpoint"));
}

#[test]
fn parse_manifest_non_object_errors() {
    let raw = serde_json::json!(["not", "an", "object"]);
    let err = parse_manifest("bad-endpoint", &raw).unwrap_err();
    assert!(err.to_string().contains("bad-endpoint"));
}

/// A malformed individual method entry (missing `name`) is skipped with a
/// warning rather than failing the whole endpoint's discovery — one bad
/// entry from a misbehaving server should not take down every other tool
/// it advertises correctly.
#[test]
fn parse_manifest_skips_malformed_method_entry() {
    let raw = serde_json::json!({
        "openrpc": "1.3.2",
        "info": {"title": "t", "version": "0.1.0"},
        "methods": [
            {"description": "missing name field", "params": []},
            {
                "name": "ok_tool",
                "description": "fine",
                "params": [],
                "x-google-scopes": ["https://www.googleapis.com/auth/tasks"]
            }
        ]
    });
    let manifest = parse_manifest("ep", &raw).expect("valid entries still parse");
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, "ok_tool");
}

/// A method with no recognizable scope extension gets an empty `scope`
/// (which every `ScopePattern` rejects) rather than panicking — the tool
/// is still returned so the "zero tools" warning in `mod.rs` distinguishes
/// "we understood the manifest but nothing survives scope filtering" from
/// a discovery-level failure.
#[test]
fn parse_manifest_method_without_scope_extension_gets_empty_scope() {
    let raw = serde_json::json!({
        "openrpc": "1.3.2",
        "info": {"title": "t", "version": "0.1.0"},
        "methods": [
            {"name": "no_scope_tool", "description": "d", "params": []}
        ]
    });
    let manifest = parse_manifest("ep", &raw).unwrap();
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].scope, "");
}

/// `x-scopes` (the generic dotted-string extension emitted by
/// `trusty-memory`/`trusty-search` via the shared
/// `trusty_common::mcp::openrpc` builder) is used as-is with no OAuth
/// translation.
#[test]
fn parse_manifest_generic_x_scopes_used_directly() {
    let raw = serde_json::json!({
        "openrpc": "1.3.2",
        "info": {"title": "trusty-search-mcp", "version": "0.1.0"},
        "methods": [
            {
                "name": "search",
                "description": "search",
                "params": [],
                "x-scopes": ["search.read"]
            }
        ]
    });
    let manifest = parse_manifest("trusty-search", &raw).unwrap();
    assert_eq!(manifest.tools[0].scope, "search.read");
}
