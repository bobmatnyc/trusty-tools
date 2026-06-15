//! Unit tests for the trusty-mpm MCP dispatch layer (#1221 + earlier phases).
//!
//! Why: extracting the test module out of `mcp/mod.rs` keeps that production
//! file under the 500-SLOC cap while the tests (classified as a test file, 1500
//! cap) live alongside it. Included via `#[path = "tests.rs"] mod tests;`.
//! What: a `MockBackend` implementing every `OrchestratorBackend` method plus
//! dispatch tests for the handshake, the core tools, the bug-reporting tools,
//! and the six session-lifecycle tools.
//! Test: this IS the test module.

use super::*;

/// In-memory backend that records calls and returns canned values.
struct MockBackend;

#[async_trait]
impl OrchestratorBackend for MockBackend {
    async fn session_list(&self) -> Result<Value, String> {
        Ok(json!([{ "id": "s1", "status": "active" }]))
    }
    async fn session_status(&self, session_id: &str) -> Result<Value, String> {
        Ok(json!({ "id": session_id, "status": "active" }))
    }
    async fn agent_delegate(
        &self,
        _session_id: &str,
        agent: &str,
        _task: &str,
        _tier: Option<&str>,
    ) -> Result<Value, String> {
        Ok(json!({ "delegation_id": "d1", "agent": agent }))
    }
    async fn memory_protect(
        &self,
        _session_id: &str,
        used_tokens: u64,
        window_tokens: u64,
    ) -> Result<Value, String> {
        Ok(json!({ "fraction": used_tokens as f64 / window_tokens as f64 }))
    }
    async fn circuit_breaker_status(&self, _agent: Option<&str>) -> Result<Value, String> {
        Ok(json!({ "state": "closed" }))
    }
    async fn hook_event(
        &self,
        _session_id: &str,
        event: &str,
        _payload: Value,
    ) -> Result<Value, String> {
        Ok(json!({ "received": event }))
    }
    async fn list_recent_errors(&self, limit: u64) -> Result<Value, String> {
        Ok(json!({ "errors": [], "limit": limit, "total": 0 }))
    }
    async fn preview_bug_report(&self, fingerprint: &str) -> Result<Value, String> {
        Ok(json!({ "fingerprint": fingerprint, "title": "mock title", "body": "mock body" }))
    }
    async fn report_bug(&self, fingerprint: &str, confirm: bool) -> Result<Value, String> {
        if confirm {
            Ok(json!({ "filed": false, "note": "mock: no token in test" }))
        } else {
            Ok(
                json!({ "filed": false, "note": "confirm:false — preview only", "fingerprint": fingerprint }),
            )
        }
    }

    // ── #1221: session-lifecycle mock impls ──────────────────────────────
    async fn session_new(
        &self,
        repo_url: &str,
        git_ref: &str,
        _task: &str,
        _name_hint: Option<&str>,
        runtime: Option<&str>,
    ) -> Result<Value, String> {
        Ok(json!({
            "id": "m-1",
            "repo_url": repo_url,
            "branch": git_ref,
            "runtime": runtime.unwrap_or("claude-code"),
            "state": "active",
        }))
    }
    async fn session_stop(&self, session_id: &str) -> Result<Value, String> {
        Ok(json!({ "id": session_id, "state": "stopped" }))
    }
    async fn session_resume(&self, session_id: &str) -> Result<Value, String> {
        Ok(json!({ "id": session_id, "state": "active" }))
    }
    async fn session_decommission(&self, session_id: &str) -> Result<Value, String> {
        Ok(json!({ "id": session_id, "state": "decommissioned" }))
    }
    async fn session_activity(&self, session_id: &str, lines: u32) -> Result<Value, String> {
        Ok(json!({ "id": session_id, "lines": lines, "raw_pane": "" }))
    }
    async fn session_send(&self, session_id: &str, text: &str) -> Result<Value, String> {
        Ok(json!({ "id": session_id, "sent": true, "text": text }))
    }

    // ── #1222: console-facing mock impls ─────────────────────────────────────
    async fn console_metrics(&self) -> Result<Value, String> {
        Ok(json!({
            "service_id": "trusty-mpm",
            "display_name": "Trusty MPM",
            "version": "0.0.0",
            "status": "ok",
            "metrics": { "fleet": { "active": 0 }, "auto_resume": { "desired": false } },
            "metrics_schema_version": 1,
        }))
    }
    async fn supervisor_status(&self) -> Result<Value, String> {
        Ok(json!({
            "fleet": { "active": 0, "stopped": 0, "total": 0 },
            "auto_resume": { "desired": false, "env": false, "pending_restart": false },
        }))
    }
    async fn auto_resume_set(&self, enabled: bool) -> Result<Value, String> {
        Ok(
            json!({ "desired": enabled, "env": false, "effective": false, "pending_restart": enabled }),
        )
    }
}

fn call(name: &str, args: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    }
}

#[tokio::test]
async fn dispatch_initialize_returns_server_info() {
    let req = Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: None,
    };
    let resp = dispatch(&MockBackend, req).await;
    let result = resp.result.unwrap();
    assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
}

#[tokio::test]
async fn dispatch_tools_list_returns_full_catalog() {
    // 9 pre-existing + 6 session-lifecycle + 3 console-facing tools = 18 (#1222).
    let req = Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: "tools/list".into(),
        params: None,
    };
    let resp = dispatch(&MockBackend, req).await;
    let tools = resp.result.unwrap()["tools"].clone();
    assert_eq!(tools.as_array().unwrap().len(), 18);
}

/// Why: the console poller calls `console_metrics`; dispatch must route it to the
/// backend and return a non-error result carrying the report.
/// Test: this test.
#[tokio::test]
async fn dispatch_console_metrics_tool() {
    let resp = dispatch(&MockBackend, call("console_metrics", json!({}))).await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trusty-mpm")
    );
}

/// Why: the supervisor widget calls `supervisor_status`; dispatch must route it.
/// Test: this test.
#[tokio::test]
async fn dispatch_supervisor_status_tool() {
    let resp = dispatch(&MockBackend, call("supervisor_status", json!({}))).await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("fleet")
    );
}

/// Why: the auto-resume toggle calls `auto_resume_set` with `enabled`; dispatch
/// must parse the bool and route it.
/// Test: this test.
#[tokio::test]
async fn dispatch_auto_resume_set_tool() {
    let resp = dispatch(
        &MockBackend,
        call("auto_resume_set", json!({ "enabled": true })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("desired")
    );
}

/// Why: `auto_resume_set` must reject a call missing the required `enabled` bool.
/// Test: this test.
#[tokio::test]
async fn dispatch_auto_resume_set_requires_enabled() {
    let resp = dispatch(&MockBackend, call("auto_resume_set", json!({}))).await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("enabled")
    );
}

#[tokio::test]
async fn dispatch_session_list_tool() {
    let resp = dispatch(&MockBackend, call("session_list", json!({}))).await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("s1")
    );
}

#[tokio::test]
async fn dispatch_agent_delegate_tool() {
    let resp = dispatch(
        &MockBackend,
        call(
            "agent_delegate",
            json!({ "session_id": "s1", "agent": "research", "task": "find" }),
        ),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("research")
    );
}

#[tokio::test]
async fn dispatch_missing_argument_is_tool_error() {
    // `session_id` is required; omitting it yields an isError result.
    let resp = dispatch(&MockBackend, call("session_status", json!({}))).await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("session_id")
    );
}

#[tokio::test]
async fn dispatch_unknown_tool_is_error() {
    let resp = dispatch(&MockBackend, call("nope", json!({}))).await;
    assert_eq!(resp.result.unwrap()["isError"], true);
}

#[tokio::test]
async fn dispatch_unknown_method_returns_jsonrpc_error() {
    let req = Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: "frobnicate".into(),
        params: None,
    };
    let resp = dispatch(&MockBackend, req).await;
    assert_eq!(resp.error.unwrap().code, error_codes::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn dispatch_notification_is_suppressed() {
    let req = Request {
        jsonrpc: Some("2.0".into()),
        id: None,
        method: "notifications/initialized".into(),
        params: None,
    };
    let resp = dispatch(&MockBackend, req).await;
    assert!(resp.suppress);
}

// ── Phase 3: bug-reporting dispatch tests ─────────────────────────────────

#[tokio::test]
async fn dispatch_list_recent_errors_tool() {
    let resp = dispatch(
        &MockBackend,
        call("list_recent_errors", json!({ "limit": 10 })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("errors")
    );
}

#[tokio::test]
async fn dispatch_list_recent_errors_default_limit() {
    // No limit specified — should default to 20 without error.
    let resp = dispatch(&MockBackend, call("list_recent_errors", json!({}))).await;
    assert_eq!(resp.result.unwrap()["isError"], false);
}

#[tokio::test]
async fn dispatch_preview_bug_report_tool() {
    let fp = "a".repeat(64);
    let resp = dispatch(
        &MockBackend,
        call("preview_bug_report", json!({ "fingerprint": fp })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(result["content"][0]["text"].as_str().unwrap().contains(&fp));
}

#[tokio::test]
async fn dispatch_preview_bug_report_requires_fingerprint() {
    let resp = dispatch(&MockBackend, call("preview_bug_report", json!({}))).await;
    assert_eq!(resp.result.unwrap()["isError"], true);
}

#[tokio::test]
async fn dispatch_report_bug_no_confirm_is_preview_only() {
    let fp = "b".repeat(64);
    let resp = dispatch(
        &MockBackend,
        call("report_bug", json!({ "fingerprint": fp, "confirm": false })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    let text = result["content"][0]["text"].as_str().unwrap();
    // The mock returns `filed: false` with the preview-only note.
    assert!(text.contains("false"), "expected filed:false: {text}");
}

#[tokio::test]
async fn dispatch_report_bug_confirm_true_calls_backend() {
    let fp = "c".repeat(64);
    let resp = dispatch(
        &MockBackend,
        call("report_bug", json!({ "fingerprint": fp, "confirm": true })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    // Mock returns filed:false + no-token note (still a valid response shape).
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("filed"), "expected 'filed' key: {text}");
}

// ── #1221: session-lifecycle dispatch tests ───────────────────────────────

/// Why: `session_new` is the spawn tool; it must parse the three required
/// fields and forward them to the backend.
/// What: calls the tool with repo/ref/task and asserts a non-error result
/// echoing the repo url.
/// Test: this test.
#[tokio::test]
async fn dispatch_session_new_tool() {
    let resp = dispatch(
        &MockBackend,
        call(
            "session_new",
            json!({ "repo_url": "https://example.com/r.git", "ref": "main", "task": "do it" }),
        ),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("example.com")
    );
}

/// Why: `session_new` must reject a call missing the required `repo_url`.
/// Test: this test.
#[tokio::test]
async fn dispatch_session_new_requires_repo_url() {
    let resp = dispatch(
        &MockBackend,
        call("session_new", json!({ "ref": "main", "task": "x" })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("repo_url")
    );
}

/// Why: the four single-id session tools must each parse `session_id` and
/// forward it; one parameterised test covers stop/resume/decommission.
/// Test: this test.
#[tokio::test]
async fn dispatch_single_id_session_tools() {
    for tool in ["session_stop", "session_resume", "session_decommission"] {
        let resp = dispatch(&MockBackend, call(tool, json!({ "session_id": "s9" }))).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], false, "{tool} should succeed");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("s9"),
            "{tool} should echo the id"
        );
    }
}

/// Why: a single-id session tool must error when `session_id` is absent.
/// Test: this test.
#[tokio::test]
async fn dispatch_session_stop_requires_id() {
    let resp = dispatch(&MockBackend, call("session_stop", json!({}))).await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("session_id")
    );
}

/// Why: `session_activity` must honour an explicit `lines` value.
/// Test: this test.
#[tokio::test]
async fn dispatch_session_activity_tool() {
    let resp = dispatch(
        &MockBackend,
        call(
            "session_activity",
            json!({ "session_id": "s1", "lines": 25 }),
        ),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("25"),
        "should reflect the requested line count"
    );
}

/// Why: `session_activity` must default `lines` to 60 when omitted (parity
/// with the HTTP activity route).
/// Test: this test.
#[tokio::test]
async fn dispatch_session_activity_default_lines() {
    let resp = dispatch(
        &MockBackend,
        call("session_activity", json!({ "session_id": "s1" })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("60"),
        "should default to 60 lines"
    );
}

/// Why: `session_send` requires both `session_id` and `text`.
/// Test: this test.
#[tokio::test]
async fn dispatch_session_send_tool() {
    let resp = dispatch(
        &MockBackend,
        call("session_send", json!({ "session_id": "s1", "text": "yes" })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("yes")
    );
}

/// Why: `session_send` must error when `text` is missing.
/// Test: this test.
#[tokio::test]
async fn dispatch_session_send_requires_text() {
    let resp = dispatch(
        &MockBackend,
        call("session_send", json!({ "session_id": "s1" })),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("text")
    );
}
