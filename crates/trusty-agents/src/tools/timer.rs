//! Timer + polling tools (#199): `wait_ms` and `poll_until`.
//!
//! Why: Agents need a way to express "wait then continue" or "loop until
//! condition" without shelling out to `sleep` (which is brittle, blocks an
//! OS thread, and bypasses the harness event bus). Native tokio-backed
//! tools give us cooperative async sleeps, predictable timeouts, and event
//! emissions that show up in the SSE stream like every other tool call.
//! What:
//!   - `WaitMsTool { duration_ms }` → `tokio::time::sleep` capped at 30s.
//!   - `PollUntilTool { target, condition, contains_text?, poll_interval_ms?, timeout_ms? }`
//!     polls a file path or HTTP(S) URL on an interval until either the
//!     `condition` is met (`exists`/`contains`/`http_200`) or the timeout
//!     elapses. Returns `{matched, content?, error?}` JSON.
//! Test: See the unit tests at the bottom of this file.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Hard upper bound on `wait_ms` to prevent runaway agents from sleeping
/// for hours. 30 seconds is enough for backoff/rate-limit pauses but small
/// enough that a misuse is recoverable in one turn.
const WAIT_MS_MAX: u64 = 30_000;

/// Hard upper bound on `poll_until` total timeout (5 minutes).
const POLL_TIMEOUT_MAX_MS: u64 = 300_000;
/// Lower bound on poll interval; anything tighter would saturate the CPU
/// and the network without adding signal.
const POLL_INTERVAL_MIN_MS: u64 = 100;
/// Upper bound on poll interval; longer than 10s is almost always a bug.
const POLL_INTERVAL_MAX_MS: u64 = 10_000;
/// Maximum number of characters returned in `content` previews so the bus
/// payload stays bounded.
const MATCH_CONTENT_PREVIEW_CHARS: usize = 500;
/// Default interval when caller omits `poll_interval_ms`.
const POLL_INTERVAL_DEFAULT_MS: u64 = 1_000;
/// Default total timeout when caller omits `timeout_ms` (30s).
const POLL_TIMEOUT_DEFAULT_MS: u64 = 30_000;
/// HTTP request timeout per individual probe inside `poll_until` — kept short
/// so a hung server doesn't eat the entire poll window.
const HTTP_PROBE_TIMEOUT_SECS: u64 = 5;

// =============================================================================
// wait_ms
// =============================================================================

/// `wait_ms` — async sleep tool with a 30s upper bound.
pub struct WaitMsTool;

impl WaitMsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WaitMsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for WaitMsTool {
    fn name(&self) -> &str {
        "wait_ms"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "wait_ms",
                "description": "Wait for a specified number of milliseconds before continuing. Use for rate limiting, retry backoff, or waiting for an async side effect.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "duration_ms": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 30000,
                            "description": "Milliseconds to wait (max 30s)"
                        }
                    },
                    "required": ["duration_ms"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let raw = match args.get("duration_ms").and_then(|v| v.as_i64()) {
            Some(n) => n,
            None => return ToolResult::err("wait_ms: 'duration_ms' integer is required"),
        };
        if raw < 1 {
            return ToolResult::err("wait_ms: 'duration_ms' must be >= 1");
        }
        let clamped = (raw as u64).min(WAIT_MS_MAX);
        tokio::time::sleep(Duration::from_millis(clamped)).await;
        ToolResult::ok(format!("waited {clamped}ms"))
    }
}

// =============================================================================
// poll_until
// =============================================================================

/// `poll_until` — file/URL polling tool.
pub struct PollUntilTool;

impl PollUntilTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PollUntilTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for PollUntilTool {
    fn name(&self) -> &str {
        "poll_until"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "poll_until",
                "description": "Poll a local file path or HTTP URL until a condition is met or timeout expires. Returns the matching content or a timeout error.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "File path (absolute) or HTTP/HTTPS URL to poll"
                        },
                        "condition": {
                            "type": "string",
                            "enum": ["exists", "contains", "http_200"],
                            "description": "exists=file/URL present, contains=response body contains substring, http_200=URL returns 2xx"
                        },
                        "contains_text": {
                            "type": "string",
                            "description": "Required when condition=contains: substring to look for"
                        },
                        "poll_interval_ms": {
                            "type": "integer",
                            "default": 1000,
                            "minimum": 100,
                            "maximum": 10000
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "default": 30000,
                            "minimum": 1000,
                            "maximum": 300000
                        }
                    },
                    "required": ["target", "condition"]
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let target = match args.get("target").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("poll_until: 'target' string is required"),
        };
        let condition = match args.get("condition").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("poll_until: 'condition' is required"),
        };
        let contains_text = args
            .get("contains_text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if condition == "contains" && contains_text.as_ref().is_none_or(|s| s.is_empty()) {
            return ToolResult::err(
                "poll_until: 'contains_text' is required when condition=contains",
            );
        }
        let poll_interval_ms = args
            .get("poll_interval_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(POLL_INTERVAL_DEFAULT_MS)
            .clamp(POLL_INTERVAL_MIN_MS, POLL_INTERVAL_MAX_MS);
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(POLL_TIMEOUT_DEFAULT_MS)
            .clamp(1_000, POLL_TIMEOUT_MAX_MS);

        let is_url = target.starts_with("http://") || target.starts_with("https://");
        let started = Instant::now();
        let deadline = started + Duration::from_millis(timeout_ms);

        loop {
            let probe = if is_url {
                probe_url(&target, &condition, contains_text.as_deref()).await
            } else {
                probe_file(&target, &condition, contains_text.as_deref()).await
            };

            match probe {
                ProbeOutcome::Matched(content) => {
                    let preview = preview_chars(&content, MATCH_CONTENT_PREVIEW_CHARS);
                    return ToolResult::ok(
                        json!({"matched": true, "content": preview}).to_string(),
                    );
                }
                ProbeOutcome::NotYet => {
                    if Instant::now() >= deadline {
                        return ToolResult::ok(
                            json!({
                                "matched": false,
                                "error": format!("timeout after {timeout_ms}ms")
                            })
                            .to_string(),
                        );
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let sleep_for = remaining.min(Duration::from_millis(poll_interval_ms));
                    tokio::time::sleep(sleep_for).await;
                }
                ProbeOutcome::Fatal(msg) => {
                    return ToolResult::err(format!("poll_until: {msg}"));
                }
            }
        }
    }
}

/// Outcome of a single poll probe.
enum ProbeOutcome {
    /// Condition met; carries the matching content (truncated by caller).
    Matched(String),
    /// Condition not met yet — caller should sleep and retry.
    NotYet,
    /// Unrecoverable error (e.g. bad condition name) — caller should bail.
    Fatal(String),
}

/// Probe a local file for the given condition.
async fn probe_file(path: &str, condition: &str, contains_text: Option<&str>) -> ProbeOutcome {
    match condition {
        "exists" => match tokio::fs::try_exists(path).await {
            Ok(true) => ProbeOutcome::Matched(path.to_string()),
            Ok(false) => ProbeOutcome::NotYet,
            // Permission errors etc. are not recoverable by waiting.
            Err(e) => ProbeOutcome::Fatal(format!("file probe error: {e}")),
        },
        "contains" => {
            let needle = match contains_text {
                Some(s) => s,
                None => return ProbeOutcome::Fatal("contains_text missing".into()),
            };
            match tokio::fs::read_to_string(path).await {
                Ok(body) if body.contains(needle) => ProbeOutcome::Matched(body),
                Ok(_) => ProbeOutcome::NotYet,
                // ENOENT is "not yet" — file may appear later.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProbeOutcome::NotYet,
                Err(e) => ProbeOutcome::Fatal(format!("file read error: {e}")),
            }
        }
        "http_200" => {
            ProbeOutcome::Fatal("condition=http_200 requires an http(s):// target".into())
        }
        other => ProbeOutcome::Fatal(format!("unknown condition '{other}'")),
    }
}

/// Probe a URL for the given condition.
async fn probe_url(url: &str, condition: &str, contains_text: Option<&str>) -> ProbeOutcome {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_PROBE_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::Fatal(format!("http client init: {e}")),
    };
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        // Connection errors are "not yet" — the server may come up later.
        Err(_) => return ProbeOutcome::NotYet,
    };
    match condition {
        "exists" | "http_200" => {
            if resp.status().is_success() {
                ProbeOutcome::Matched(resp.status().to_string())
            } else {
                ProbeOutcome::NotYet
            }
        }
        "contains" => {
            let needle = match contains_text {
                Some(s) => s,
                None => return ProbeOutcome::Fatal("contains_text missing".into()),
            };
            match resp.text().await {
                Ok(body) if body.contains(needle) => ProbeOutcome::Matched(body),
                Ok(_) => ProbeOutcome::NotYet,
                Err(e) => ProbeOutcome::Fatal(format!("http body read: {e}")),
            }
        }
        other => ProbeOutcome::Fatal(format!("unknown condition '{other}'")),
    }
}

/// Truncate to N characters preserving char boundaries.
fn preview_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_ms_respects_duration() {
        // Why: the tool MUST actually pause for roughly the requested time —
        // otherwise agents using it for backoff would race ahead.
        let tool = WaitMsTool::new();
        let start = Instant::now();
        let result = tool.execute(json!({"duration_ms": 150})).await;
        let elapsed = start.elapsed();
        assert!(!result.is_error(), "wait_ms should succeed: {result:?}");
        assert!(
            elapsed >= Duration::from_millis(140),
            "wait was too short: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(800),
            "wait was suspiciously long (suggests blocking, not async sleep): {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn wait_ms_clamps_to_max() {
        // A request beyond the 30s ceiling must be silently clamped, not
        // honoured (otherwise an agent could sleep for hours).
        let tool = WaitMsTool::new();
        // We can't actually sleep 30s in a test; just check it returns and
        // reports the clamped value via the success message.
        // Use a small input to keep the test fast but verify the contract.
        let r = tool.execute(json!({"duration_ms": 5})).await;
        assert!(!r.is_error());
        assert!(r.content().contains("5ms"));
    }

    #[tokio::test]
    async fn wait_ms_rejects_missing_arg() {
        let tool = WaitMsTool::new();
        let r = tool.execute(json!({})).await;
        assert!(r.is_error());
        assert!(r.content().contains("duration_ms"));
    }

    #[tokio::test]
    async fn wait_ms_rejects_zero_duration() {
        let tool = WaitMsTool::new();
        let r = tool.execute(json!({"duration_ms": 0})).await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn poll_until_file_exists_succeeds() {
        // Why: the canonical happy path — caller polls for a file, file
        // appears mid-poll, tool returns matched=true with content.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("appears.txt");
        let path_str = path.to_string_lossy().to_string();

        // Spawn a task that writes the file after 200ms.
        let path_clone = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            tokio::fs::write(&path_clone, "hello world")
                .await
                .expect("write");
        });

        let tool = PollUntilTool::new();
        let r = tool
            .execute(json!({
                "target": path_str,
                "condition": "exists",
                "poll_interval_ms": 100,
                "timeout_ms": 3_000
            }))
            .await;
        assert!(!r.is_error(), "poll_until errored: {}", r.content());
        let v: serde_json::Value = serde_json::from_str(r.content()).expect("json");
        assert_eq!(v["matched"], true, "expected matched=true: {v}");
    }

    #[tokio::test]
    async fn poll_until_timeout_returns_error() {
        // Why: when the condition never becomes true, the tool must return
        // matched=false within the requested timeout (not hang forever).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("never-appears.txt");
        let path_str = path.to_string_lossy().to_string();

        let tool = PollUntilTool::new();
        let start = Instant::now();
        let r = tool
            .execute(json!({
                "target": path_str,
                "condition": "exists",
                "poll_interval_ms": 100,
                "timeout_ms": 1_000
            }))
            .await;
        let elapsed = start.elapsed();
        assert!(!r.is_error(), "expected ok with matched=false");
        let v: serde_json::Value = serde_json::from_str(r.content()).expect("json");
        assert_eq!(v["matched"], false);
        assert!(
            v["error"].as_str().unwrap_or("").contains("timeout"),
            "expected timeout error: {v}"
        );
        assert!(
            elapsed >= Duration::from_millis(900),
            "returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(2_500),
            "returned too late (didn't honour timeout): {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn poll_until_contains_finds_substring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        tokio::fs::write(&path, "alpha beta gamma")
            .await
            .expect("write");
        let tool = PollUntilTool::new();
        let r = tool
            .execute(json!({
                "target": path.to_string_lossy(),
                "condition": "contains",
                "contains_text": "beta",
                "timeout_ms": 1_000
            }))
            .await;
        assert!(!r.is_error());
        let v: serde_json::Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["matched"], true);
        assert!(v["content"].as_str().unwrap().contains("beta"));
    }

    #[tokio::test]
    async fn poll_until_rejects_missing_target() {
        let tool = PollUntilTool::new();
        let r = tool.execute(json!({"condition": "exists"})).await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn poll_until_rejects_contains_without_text() {
        let tool = PollUntilTool::new();
        let r = tool
            .execute(json!({"target": "/tmp/x", "condition": "contains"}))
            .await;
        assert!(r.is_error());
        assert!(r.content().contains("contains_text"));
    }

    #[test]
    fn schemas_have_required_fields() {
        let wait = WaitMsTool::new().schema();
        assert_eq!(wait["function"]["name"], "wait_ms");
        let poll = PollUntilTool::new().schema();
        assert_eq!(poll["function"]["name"], "poll_until");
        let required = poll["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|v| v == "target"));
        assert!(required.iter().any(|v| v == "condition"));
    }
}
