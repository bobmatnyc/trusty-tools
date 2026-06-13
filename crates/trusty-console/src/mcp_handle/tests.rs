//! Tests for `mcp_handle` — split to `tests.rs` (1500-SLOC budget) per #1131.

use std::time::{Duration, Instant};

use super::*;

// ── MCP content envelope helper tests ────────────────────────────────────

/// Why: The MCP envelope must be stripped so route handlers return clean JSON.
/// What: pass a well-formed envelope and assert the inner array is returned.
/// Test: this test.
#[test]
fn unwrap_mcp_content_extracts_text_json() {
    use serde_json::json;
    let envelope = json!({
        "content": [{"type": "text", "text": "[{\"id\":\"foo\"}]"}],
        "isError": false
    });
    let result = unwrap_mcp_content(envelope);
    assert!(result.is_array(), "expected array, got: {result}");
    assert_eq!(result[0]["id"], "foo");
}

/// Why: a non-JSON text payload must be returned as a JSON string, not crash.
/// What: pass an envelope with plain-text content, assert a string Value.
/// Test: this test.
#[test]
fn unwrap_mcp_content_non_json_text_returns_string() {
    use serde_json::json;
    let envelope = json!({
        "content": [{"type": "text", "text": "plain text, not json"}],
        "isError": false
    });
    let result = unwrap_mcp_content(envelope);
    assert!(result.is_string(), "expected string for non-JSON text");
}

/// Why: if the envelope shape is unexpected (no content key), the raw value
/// must be returned unchanged so callers always get something useful.
/// What: pass a value without a content key, assert it is returned as-is.
/// Test: this test.
#[test]
fn unwrap_mcp_content_passthrough_on_unknown_shape() {
    use serde_json::json;
    let raw = json!({"data": [1, 2, 3]});
    let result = unwrap_mcp_content(raw.clone());
    assert_eq!(result, raw);
}

// ── Pure backoff function tests ───────────────────────────────────────────

/// Why: attempt=1 (first failure) must wait exactly base_ms, not 2*base_ms.
/// What: call compute_backoff_delay(1, 1000, 60000) and assert 1000.
/// Test: this test.
#[test]
fn compute_backoff_delay_base() {
    assert_eq!(
        compute_backoff_delay(1, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
        BACKOFF_BASE_MS,
        "first failure must wait base_ms"
    );
}

/// Why: second failure must double the delay (2*base_ms = 2000 ms).
/// What: call compute_backoff_delay(2, 1000, 60000) and assert 2000.
/// Test: this test.
#[test]
fn compute_backoff_delay_doubles() {
    assert_eq!(
        compute_backoff_delay(2, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
        2 * BACKOFF_BASE_MS,
        "second failure must double the delay"
    );
}

/// Why: large attempt counts must be capped at cap_ms so the delay never
/// grows without bound.
/// What: call compute_backoff_delay(100, 1000, 60000) and assert 60000.
/// Test: this test.
#[test]
fn compute_backoff_delay_caps() {
    assert_eq!(
        compute_backoff_delay(100, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
        BACKOFF_CAP_MS,
        "large attempt must cap at cap_ms"
    );
}

/// Why: attempt=0 is a sentinel (no failures yet); it should return base_ms
/// (shift by saturating_sub(1) → 0, so 2^0 * base = base).
/// What: call compute_backoff_delay(0, 1000, 60000) and assert 1000.
/// Test: this test.
#[test]
fn compute_backoff_delay_attempt_zero() {
    assert_eq!(
        compute_backoff_delay(0, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
        BACKOFF_BASE_MS,
        "attempt=0 must not underflow — returns base_ms"
    );
}

// ── Integration-style handle tests ────────────────────────────────────────

/// Why: When the binary is absent from PATH, `poll_metrics` must return an
/// error immediately (not hang or panic) so the poller can degrade gracefully.
/// What: Create a handle pointing at a binary that does not exist, call
/// `poll_metrics`, assert it returns `Err`.
/// Test: This test.
#[tokio::test]
async fn mcp_handle_absent_binary_returns_error() {
    let handle =
        McpServiceHandle::new("/nonexistent/trusty-analyze-xyzzy", vec!["mcp".to_string()]);
    let result = handle.poll_metrics().await;
    assert!(result.is_err(), "absent binary must return Err");
}

/// Why: `new()` must succeed synchronously without performing I/O so
/// the console can construct handles at startup without async.
/// What: Construct a handle and assert the binary/args fields are stored.
/// Test: This test (checks construction is cheap/sync-compatible).
#[test]
fn mcp_handle_constructs_without_io() {
    let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
    assert_eq!(handle.binary, "trusty-analyze");
    assert_eq!(handle.args, vec!["mcp"]);
}

/// Why: Once a binary is marked `Absent` (not found on PATH) the handle
/// must never retry — every subsequent poll must return Err immediately.
/// What: Poll twice; both must return Err with no hang.
/// Test: This test.
#[tokio::test]
async fn mcp_handle_absent_never_retries() {
    let handle = McpServiceHandle::new(
        "/nonexistent/trusty-analyze-xyzzy2",
        vec!["mcp".to_string()],
    );
    let r1 = handle.poll_metrics().await;
    let r2 = handle.poll_metrics().await;
    assert!(r1.is_err(), "first poll must return Err for absent binary");
    assert!(r2.is_err(), "second poll must also return Err (no retry)");
}

/// Why: When the handle is in the Degraded state (tools/list succeeded but
/// console_metrics was not listed), poll_metrics must return
/// McpHandleError::Degraded with the remediation hint — not Absent or Other.
/// The handle must stay Degraded while inside its backoff window.
/// What: Primes the handle to Degraded with a 60 s future backoff, then
/// calls poll_metrics and asserts the Degraded variant with a non-empty hint.
/// Test: This test.
#[tokio::test]
async fn mcp_handle_degraded_state_returns_degraded_error() {
    let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
    // Set Degraded with a 60 s future backoff so the self-healing window
    // has NOT elapsed — the handle must stay Degraded and return the hint.
    handle
        .prime_degraded_with_backoff_for_test(Duration::from_secs(60))
        .await;
    let result = handle.poll_metrics().await;
    assert!(result.is_err(), "degraded handle must return Err");
    match result.unwrap_err() {
        McpHandleError::Degraded { hint } => {
            assert!(!hint.is_empty(), "degraded hint must not be empty");
            assert!(
                hint.contains("console_metrics"),
                "hint must mention console_metrics, got: {hint}"
            );
        }
        other => panic!("expected McpHandleError::Degraded, got: {other}"),
    }
}

/// Why: `degraded_hint()` must return `Some(hint)` only for the `Degraded`
/// state, so the services handler can both set `status = Degraded` and
/// populate the `hint` field in one call.
/// What: primes three different states (Degraded / None / Absent) and
/// asserts the expected `Option<String>` for each.
/// Test: this test.
#[tokio::test]
async fn test_degraded_hint_returns_some_when_degraded() {
    // Degraded → Some(hint containing "console_metrics")
    let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
    {
        let mut guard = handle.state.lock().await;
        let (state_opt, _) = &mut *guard;
        *state_opt = Some(HandleState::Degraded);
    }
    let hint = handle.degraded_hint().await;
    assert!(hint.is_some(), "Degraded state must return Some(hint)");
    assert!(
        hint.unwrap().contains("console_metrics"),
        "hint must mention console_metrics"
    );

    // None (uninitialised) → None
    let h2 = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
    assert!(
        h2.degraded_hint().await.is_none(),
        "None state must return None"
    );

    // Absent → None
    let h3 = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
    {
        let mut guard = h3.state.lock().await;
        let (state_opt, _) = &mut *guard;
        *state_opt = Some(HandleState::Absent);
    }
    assert!(
        h3.degraded_hint().await.is_none(),
        "Absent state must return None"
    );
}

/// Why: After a `call_tool` / respawn failure on an already-connected handle,
/// `SpawnBackoff` must gate subsequent poll attempts — the respawn path must
/// NOT be unbounded just because the handle reached `Connected` once. This
/// verifies the fix for the backoff gap identified in PR #1124 review.
///
/// Mechanism under test: when `call_tool` returns `Err`, `poll_metrics`
/// records a failure via `backoff.record_failure()`, resets state to `None`,
/// and returns `Err`. The very next call re-enters the lazy-init block; since
/// `backoff.should_attempt()` returns `false` (backoff window not yet elapsed),
/// it returns `Err` immediately without attempting another spawn — proving
/// the respawn path is now gated by the same backoff mechanism as the initial
/// connect path.
///
/// What: Manually insert a `SpawnBackoff` in the failure state (failure_count=1,
/// next_attempt = far future) into a handle whose state is `None`, then verify
/// that `poll_metrics` returns `Err` immediately without trying to spawn.
/// Test: This test (no real binary or network required).
#[tokio::test]
async fn mcp_handle_respawn_failure_applies_backoff() {
    // Construct a handle whose backoff is already in the penalty window:
    // failure_count = 1, next_attempt = 60 seconds in the future.
    let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
    {
        let mut guard = handle.state.lock().await;
        let (state_opt, backoff) = &mut *guard;
        // Simulate one prior failure that put us in backoff.
        backoff.failure_count = 1;
        backoff.next_attempt = Instant::now() + Duration::from_secs(60);
        // State remains None (as if we transitioned back from Connected after
        // a failed call_tool / respawn).
        assert!(state_opt.is_none());
    }

    // The binary exists on the machine (trusty-analyze may or may not be on
    // PATH). We prime the binary name to something that IS on PATH to avoid
    // the `which` miss path, so the test exercises the backoff gate rather
    // than the absent-binary gate. Use "true" (always present on Unix).
    let handle_with_true = McpServiceHandle::new("true", vec![]);
    {
        let mut guard = handle_with_true.state.lock().await;
        let (_state_opt, backoff) = &mut *guard;
        backoff.failure_count = 1;
        backoff.next_attempt = Instant::now() + Duration::from_secs(60);
    }

    let result = handle_with_true.poll_metrics().await;
    assert!(
        result.is_err(),
        "poll_metrics must return Err while in backoff window — respawn path must be gated"
    );
    // Should be the Backoff variant
    assert!(
        matches!(result.unwrap_err(), McpHandleError::Backoff { .. }),
        "error must be McpHandleError::Backoff"
    );
}

/// Why: The tools/list probe must transition the handle to DEGRADED when the
/// handshake succeeds but console_metrics is not in the tool listing. This
/// test exercises the real probe path with a minimal shell-script MCP stub
/// that answers initialize and tools/list (without console_metrics).
/// What: Spawns a sh script that completes the MCP handshake and returns an
/// empty tools/list, then verifies poll_metrics returns McpHandleError::Degraded.
/// Test: This test. Marked #[ignore] to keep CI fast (requires Unix + sh).
#[tokio::test]
#[cfg(unix)]
#[ignore]
async fn mcp_handle_probe_detects_missing_console_metrics_tool() {
    // Minimal MCP stub: answers initialize, then tools/list with no tools.
    let script = r#"
while IFS= read -r line; do
  id=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('id',''))" 2>/dev/null)
  method=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('method',''))" 2>/dev/null)
  case "$method" in
    initialize) echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"stub\",\"version\":\"0.0.1\"},\"capabilities\":{}}}" ;;
    "notifications/initialized") ;;
    "tools/list") echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[]}}" ;;
    *) echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{}}" ;;
  esac
done
"#;
    let handle = McpServiceHandle::new("sh", vec!["-c".to_string(), script.to_string()]);
    let result = handle.poll_metrics().await;
    assert!(
        result.is_err(),
        "stub with no console_metrics must return Err"
    );
    assert!(
        matches!(result.unwrap_err(), McpHandleError::Degraded { .. }),
        "error must be McpHandleError::Degraded when console_metrics absent"
    );
}

/// Why: A Degraded handle within its backoff window must NOT re-probe —
/// it must return `McpHandleError::Degraded` immediately so the UI keeps
/// showing the degraded badge and hint until the window expires.
/// What: Primes a handle to Degraded with a 60 s future backoff, calls
/// `poll_metrics`, asserts `Degraded` error (not a spawn attempt).
/// Test: This test.
#[tokio::test]
async fn mcp_handle_degraded_within_backoff_window_stays_degraded() {
    // Binary must be on PATH so the which() miss path is not taken.
    let handle = McpServiceHandle::new("true", vec![]);
    // Degrade with a 60 s future window (window has NOT elapsed).
    handle
        .prime_degraded_with_backoff_for_test(Duration::from_secs(60))
        .await;

    let result = handle.poll_metrics().await;
    assert!(
        result.is_err(),
        "Degraded handle within backoff window must return Err"
    );
    assert!(
        matches!(result.unwrap_err(), McpHandleError::Degraded { .. }),
        "error must remain Degraded while inside the backoff window"
    );
    // State must still be Degraded — no re-probe attempted.
    let guard = handle.state.lock().await;
    let (state_opt, _) = &*guard;
    assert!(
        matches!(state_opt, Some(HandleState::Degraded)),
        "state must remain Degraded when backoff window has not elapsed"
    );
}

/// Why: A Degraded handle whose backoff window has elapsed must attempt a
/// re-probe (self-heal), transitioning out of Degraded when the binary is
/// not on PATH. This verifies that `ensure_connected` drops state to `None`
/// and re-enters the lazy-init path rather than returning `Degraded` forever.
///
/// The test uses `/nonexistent/binary-xyzzy` so the re-probe reaches the
/// `which()` miss branch (→ Absent) rather than spawning a real process.
/// This is sufficient to verify the self-heal path: the handle is no longer
/// Degraded after the re-probe.
///
/// What: Primes a handle to Degraded with a zeroed backoff (`Duration::ZERO`
/// so the window is already elapsed), calls `poll_metrics`, asserts the
/// result is Err but NOT `Degraded` (proving a re-probe occurred and the
/// Degraded state was cleared).
/// Test: This test.
#[tokio::test]
async fn mcp_handle_degraded_self_heals_after_backoff_window() {
    // Use an absent binary so the re-probe terminates quickly (which miss →
    // Absent) without needing a real MCP daemon.
    let handle = McpServiceHandle::new("/nonexistent/xyzzy-selfheal-test", vec!["mcp".to_string()]);
    // Degrade with Duration::ZERO so the backoff window is already elapsed.
    handle
        .prime_degraded_with_backoff_for_test(Duration::ZERO)
        .await;

    let result = handle.poll_metrics().await;
    assert!(
        result.is_err(),
        "self-healing re-probe must still return Err (binary absent)"
    );
    // The error must NOT be Degraded — a re-probe occurred and the handle
    // transitioned to Absent (binary not found), proving the Degraded state
    // was cleared and the init path re-ran.
    assert!(
        !matches!(result.unwrap_err(), McpHandleError::Degraded { .. }),
        "after backoff window the handle must not return Degraded — \
         re-probe should have cleared it (expected Absent)"
    );
    // State must be Absent (which miss).
    let guard = handle.state.lock().await;
    let (state_opt, _) = &*guard;
    assert!(
        matches!(state_opt, Some(HandleState::Absent)),
        "state must be Absent after re-probe with nonexistent binary, \
         not Degraded"
    );
}

/// Why: This is the key regression test for issue #1170. A stale daemon that
/// lacks a specific tool (e.g. `list_analyze_indexes`) previously caused route
/// handlers to receive a raw JSON-RPC -32601 error → HTTP 502 with empty body.
/// `call_tool_checked` must return `McpHandleError::ToolUnavailable` WITHOUT
/// making a JSON-RPC call, so the route handler can return a clean 503+hint.
/// What: Manually prime a handle into `Connected` state with a tool set that
/// does NOT include `list_analyze_indexes`, then call `call_tool_checked` with
/// that tool name and assert the result is `McpHandleError::ToolUnavailable`
/// with a non-empty hint mentioning the tool name. No real MCP call is made —
/// the guard fires before any I/O.
/// Test: This test. Simulates the exact production failure that prompted #1170.
#[tokio::test]
async fn call_tool_checked_returns_tool_unavailable_when_tool_absent() {
    use std::collections::HashSet;
    use trusty_common::stdio_mcp_client::StdioMcpClient;

    let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);

    // Manually prime the handle to Connected state with a limited tool set —
    // console_metrics is present (required sentinel) but list_analyze_indexes
    // is absent (simulating a stale daemon built before that tool was added).
    // We use `cat` as a benign placeholder binary since we need a valid child
    // handle but will never actually make a call through the MCP client.
    let cat_client = StdioMcpClient::spawn("cat", &[], "test-client")
        .await
        .expect("cat must be available");
    let client_arc = std::sync::Arc::new(tokio::sync::Mutex::new(
        Box::new(cat_client) as Box<StdioMcpClient>
    ));
    let mut tool_set = HashSet::new();
    tool_set.insert("console_metrics".to_string());
    // Intentionally do NOT add list_analyze_indexes — simulating stale daemon.
    {
        let mut guard = handle.state.lock().await;
        let (state_opt, backoff) = &mut *guard;
        backoff.reset();
        *state_opt = Some(HandleState::Connected {
            client: std::sync::Arc::clone(&client_arc),
            tool_names: tool_set,
            daemon_version: "0.7.0".to_string(),
        });
    }

    let result = handle
        .call_tool_checked("list_analyze_indexes", serde_json::json!({}))
        .await;

    assert!(
        result.is_err(),
        "call_tool_checked must return Err when tool is absent from cached set"
    );
    match result.unwrap_err() {
        McpHandleError::ToolUnavailable { tool, hint } => {
            assert_eq!(
                tool, "list_analyze_indexes",
                "ToolUnavailable must name the requested tool"
            );
            assert!(
                !hint.is_empty(),
                "ToolUnavailable must include a non-empty actionable hint"
            );
            assert!(
                hint.contains("list_analyze_indexes"),
                "hint must mention the missing tool name, got: {hint}"
            );
        }
        other => panic!(
            "expected McpHandleError::ToolUnavailable, got: {other} — \
             capability-gate must fire BEFORE any JSON-RPC call"
        ),
    }
}

/// Why: `daemon_version()` must return `Some(version)` when the handle is
/// `Connected` and the version is non-empty, so route handlers and the UI
/// can surface the daemon's reported version without additional I/O.
/// What: Manually primes the handle to `Connected` with a known version string,
/// then asserts `daemon_version()` returns that exact string. Also asserts
/// `None` for `Absent` and uninitialised states.
/// Test: This test.
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_version_returns_some_when_connected() {
    use std::collections::HashSet;
    use trusty_common::stdio_mcp_client::StdioMcpClient;

    let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);

    // Uninitialised → None
    assert!(
        handle.daemon_version().await.is_none(),
        "uninitialised handle must return None for daemon_version"
    );

    // Prime to Connected with known version
    let cat_client = StdioMcpClient::spawn("cat", &[], "test-client")
        .await
        .expect("cat must be available");
    let client_arc = std::sync::Arc::new(tokio::sync::Mutex::new(
        Box::new(cat_client) as Box<StdioMcpClient>
    ));
    let mut tool_set = HashSet::new();
    tool_set.insert("console_metrics".to_string());
    {
        let mut guard = handle.state.lock().await;
        let (state_opt, _) = &mut *guard;
        *state_opt = Some(HandleState::Connected {
            client: client_arc,
            tool_names: tool_set,
            daemon_version: "1.2.3-test".to_string(),
        });
    }
    let v = handle.daemon_version().await;
    assert_eq!(
        v.as_deref(),
        Some("1.2.3-test"),
        "Connected handle must return the cached daemon version"
    );

    // Absent → None
    let h2 = McpServiceHandle::new("/nonexistent/binary", vec![]);
    {
        let mut guard = h2.state.lock().await;
        let (state_opt, _) = &mut *guard;
        *state_opt = Some(HandleState::Absent);
    }
    assert!(
        h2.daemon_version().await.is_none(),
        "Absent handle must return None for daemon_version"
    );
}
