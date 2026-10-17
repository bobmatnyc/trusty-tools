//! #9970 AC3: the MCP search tools relay the daemon's `meta.embedder` note.
//!
//! Why: an agent reads search results through these tools, not the socket,
//! so the note must survive compaction and the byte cap on the way out.
//! What: a mock daemon answers every search with a body carrying the note;
//! each tool's payload must carry it unchanged. The relay is a pass-through,
//! so this is a guard against a later rewrite of `meta`, not a red-first test.
//! Test: this module.

use serde_json::Value;

use super::tests::{req, spawn_mock_daemon};

#[tokio::test]
async fn search_tools_relay_the_embedder_note() {
    let embedder = serde_json::json!({
        "active": "ort",
        "intended": "python",
        "bootstrap": "installing_venv",
        "note": "Degraded: search results come from the ort fallback embedder",
    });
    let body = serde_json::json!({
        "results": [],
        "intent": "Keyword",
        "latency_ms": 1,
        "meta": { "dropped_total": 0, "embedder": embedder },
    });
    let status = serde_json::json!({
        "stages": { "lexical": { "status": "ready" } },
        "search_capabilities": ["bm25"],
    });
    for (tool, compact) in [
        ("search", false),
        ("search", true),
        ("search_lexical", true),
    ] {
        let (daemon, _bodies, _paths) = spawn_mock_daemon(status.clone(), body.clone()).await;
        let args = serde_json::json!({ "index_id": "demo", "query": "x", "compact": compact });
        let resp = daemon
            .server()
            .dispatch(req(
                "tools/call",
                serde_json::json!({ "name": tool, "arguments": args }),
            ))
            .await;
        let result = resp.result.expect("tools/call result");
        let text = result["content"][0]["text"].as_str().expect("text node");
        let payload: Value = serde_json::from_str(text).expect("JSON payload");
        assert_eq!(
            payload["meta"]["embedder"], embedder,
            "{tool} compact={compact}"
        );
    }
}
