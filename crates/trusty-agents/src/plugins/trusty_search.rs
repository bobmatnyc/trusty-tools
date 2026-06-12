//! `trusty-search` plugin wrapper.
//!
//! Why: trusty-agents wants pluggable code-search backends. `trusty-search`
//! distributes as a binary that speaks MCP (JSON-RPC 2.0 over stdio), so we
//! reuse `StdioMcpClient` and expose a small typed surface for the
//! operations the rest of the harness actually needs.
//! What: Spawns `trusty-search serve` and routes `search_code` / `index_file`
//! / `remove_file` / `health` to MCP `tools/call` with the matching tool
//! name. `try_spawn` returns None when the binary isn't on PATH or the
//! handshake fails (the external repo is being developed in parallel and
//! may not implement MCP yet).
//! Test: Unit tests cover `extract_text_results` parsing. The spawn flow is
//! exercised opportunistically — when the binary is missing, `try_spawn`
//! returns None, which is the contract the manager relies on.

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::warn;

use super::stdio_mcp::StdioMcpClient;

/// Plugin handle that owns the `trusty-search` MCP child process.
///
/// Why: Wraps `StdioMcpClient` behind operation-specific methods so callers
/// don't need to know the underlying tool names or argument shapes.
/// What: Holds the client behind a `Mutex` because MCP requests are not
/// concurrency-safe (one in-flight request per stdio pair).
/// Test: See `extract_text_results_*` unit tests below; integration coverage
/// requires the real `trusty-search` binary on PATH.
pub struct TrustySearchPlugin {
    client: Mutex<StdioMcpClient>,
}

impl TrustySearchPlugin {
    /// Try to spawn `trusty-search serve` and run the MCP handshake.
    ///
    /// Why: The plugin is optional — operators may not have it installed.
    /// Returning `Option` lets `PluginManager` degrade gracefully instead of
    /// failing harness startup.
    /// What: Checks PATH via `which`, spawns the binary, runs `initialize`.
    /// Any failure returns None (logged at debug level by the caller).
    /// Test: Returns None when binary missing — observable from
    /// `PluginManager::init` integration with no binary present.
    pub async fn try_spawn() -> Option<Self> {
        if !binary_on_path("trusty-search") {
            return None;
        }
        let mut client = StdioMcpClient::spawn("trusty-search", &["serve"], "trusty-agents")
            .await
            .ok()?;
        client.initialize().await.ok()?;
        Some(Self {
            client: Mutex::new(client),
        })
    }

    /// Search the indexed corpus for `query`, returning up to `limit` hits.
    ///
    /// Why: Primary read path used by agent search tools.
    /// What: Calls MCP `search_code` and parses the text content frames into
    /// JSON values. The trusty-search server is expected to return JSON-encoded
    /// hit objects in the `text` field.
    /// Test: `extract_text_results_*` covers the parsing layer.
    pub async fn search_code(&self, query: &str, limit: usize) -> Result<Vec<Value>> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-search: plugin process restarted");
        }
        let resp = client
            .call_tool("search_code", json!({ "query": query, "limit": limit }))
            .await?;
        Ok(extract_text_results(&resp))
    }

    /// Index or re-index a single file by path.
    pub async fn index_file(&self, path: &str) -> Result<()> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-search: plugin process restarted");
        }
        client
            .call_tool("index_file", json!({ "path": path }))
            .await?;
        Ok(())
    }

    /// Remove a single file from the index by path.
    pub async fn remove_file(&self, path: &str) -> Result<()> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-search: plugin process restarted");
        }
        client
            .call_tool("remove_file", json!({ "path": path }))
            .await?;
        Ok(())
    }

    /// Liveness probe. Returns Ok(true) when the server responds to ping.
    pub async fn health(&self) -> Result<bool> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-search: plugin process restarted");
        }
        client.ping().await?;
        Ok(true)
    }
}

/// Decide whether a binary is on PATH by scanning `$PATH` entries.
///
/// Why: Spawning a `which` subprocess on every startup probe adds avoidable
/// latency (process fork + exec). Scanning the `$PATH` directories in-process
/// is faster and keeps deps minimal — no `which` crate, no subprocess.
/// What: Returns true iff `<dir>/<name>` is a regular file for some `dir` on
/// `$PATH`.
/// Test: `binary_on_path_recognises_sh` below.
pub(super) fn binary_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
        .unwrap_or(false)
}

/// Decode an MCP `CallToolResult` into a vector of JSON values.
///
/// Why: MCP tools return `{ "content": [{ "type": "text", "text": "..." }] }`
/// where the text is typically a JSON-encoded payload. Callers want the
/// decoded structure, not a string.
/// What: Iterates `content`, attempts to JSON-parse each `text` field, and
/// falls back to a `{"text": "..."}` wrapper when parsing fails (so non-JSON
/// human-readable responses still surface to callers).
/// Test: `extract_text_results_*` unit tests.
fn extract_text_results(call_tool_result: &Value) -> Vec<Value> {
    let Some(items) = call_tool_result.get("content").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
        .map(|s| serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({ "text": s })))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The happy path — server returns JSON-encoded hits in text frames
    /// and we must decode them back to structured values for callers.
    #[test]
    fn extract_text_results_parses_json_payloads() {
        let resp = json!({
            "content": [
                { "type": "text", "text": "{\"path\":\"a.rs\",\"score\":0.9}" },
                { "type": "text", "text": "{\"path\":\"b.rs\",\"score\":0.7}" },
            ]
        });
        let out = extract_text_results(&resp);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["path"], "a.rs");
        assert_eq!(out[1]["score"], 0.7);
    }

    /// Why: Non-JSON text payloads (e.g., plain error messages) must still be
    /// returned to callers rather than silently dropped.
    #[test]
    fn extract_text_results_wraps_non_json_text() {
        let resp = json!({ "content": [{ "type": "text", "text": "no index yet" }] });
        let out = extract_text_results(&resp);
        assert_eq!(out, vec![json!({ "text": "no index yet" })]);
    }

    /// Why: A response without the `content` array must not panic — return
    /// an empty vec so callers see "zero results" rather than an error.
    #[test]
    fn extract_text_results_handles_missing_content() {
        let resp = json!({ "isError": false });
        assert!(extract_text_results(&resp).is_empty());
    }

    /// Why: `binary_on_path` is the gating check for `try_spawn`; a sanity
    /// test against a binary present on every supported dev platform proves
    /// the probe works.
    #[test]
    #[cfg(unix)]
    fn binary_on_path_recognises_sh() {
        assert!(binary_on_path("sh"));
        assert!(!binary_on_path("definitely-not-a-real-binary-xyzzy"));
    }
}
