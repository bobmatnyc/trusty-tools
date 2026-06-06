//! `trusty-memory` plugin wrapper.
//!
//! Why: Memory persistence is an optional plugin in trusty-agents. `trusty-memory`
//! ships as a binary speaking MCP, so we mirror the `trusty-search` wrapper
//! pattern and route operations through `StdioMcpClient`.
//! What: Spawns `trusty-memory serve` and exposes `remember`, `recall`, and
//! `health`. `try_spawn` returns None when the binary is unavailable or the
//! handshake fails.
//! Test: `extract_id` and `extract_recall_hits` parsing helpers are unit
//! tested. End-to-end coverage requires the real binary.

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::warn;

use super::stdio_mcp::StdioMcpClient;
use super::trusty_search::binary_on_path;

/// Plugin handle owning a `trusty-memory` MCP child process.
///
/// Why: Same rationale as `TrustySearchPlugin` — encapsulate the MCP client
/// behind operation-specific methods and serialise requests with a `Mutex`.
pub struct TrustyMemoryPlugin {
    client: Mutex<StdioMcpClient>,
}

impl TrustyMemoryPlugin {
    /// Try to spawn `trusty-memory serve` and run the MCP handshake.
    ///
    /// Why: Optional plugin; failures must not break harness startup.
    /// What: Returns None when binary missing or initialize fails.
    pub async fn try_spawn() -> Option<Self> {
        if !binary_on_path("trusty-memory") {
            return None;
        }
        let mut client = StdioMcpClient::spawn("trusty-memory", &["serve"])
            .await
            .ok()?;
        client.initialize().await.ok()?;
        Some(Self {
            client: Mutex::new(client),
        })
    }

    /// Persist `content` with optional `tags`, returning the server's ID for
    /// the new memory record (empty string if the server didn't include one).
    pub async fn remember(&self, content: &str, tags: &[&str]) -> Result<String> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-memory: plugin process restarted");
        }
        let resp = client
            .call_tool("remember", json!({ "content": content, "tags": tags }))
            .await?;
        Ok(extract_id(&resp))
    }

    /// Retrieve up to `limit` memories matching `query`.
    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<Value>> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-memory: plugin process restarted");
        }
        let resp = client
            .call_tool("recall", json!({ "query": query, "limit": limit }))
            .await?;
        Ok(extract_recall_hits(&resp))
    }

    /// Liveness probe.
    pub async fn health(&self) -> Result<bool> {
        let mut client = self.client.lock().await;
        if !client.is_alive() {
            warn!("trusty-memory: plugin process restarted");
        }
        client.ping().await?;
        Ok(true)
    }
}

/// Pull a memory ID out of a `tools/call` result.
///
/// Why: The remember tool reports back a record identifier, typically as JSON
/// like `{"id": "..."}` in a single text frame. Callers want the bare string.
/// What: Looks for `content[0].text`, JSON-parses it, returns `id` if present;
/// otherwise returns the raw text. Empty string if neither path matches.
/// Test: `extract_id_*` unit tests.
fn extract_id(resp: &Value) -> String {
    let text = resp
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|item| item.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if text.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<Value>(text)
        && let Some(id) = v.get("id").and_then(|x| x.as_str())
    {
        return id.to_string();
    }
    text.to_string()
}

/// Decode recall hits from a `tools/call` result.
///
/// Why: Recall returns multiple text frames, each a JSON-encoded match.
/// Callers want a Vec of structured values for downstream filtering.
/// What: Identical decoding strategy to `trusty_search::extract_text_results`,
/// duplicated here so each plugin owns its parsing.
/// Test: `extract_recall_hits_*` unit tests.
fn extract_recall_hits(resp: &Value) -> Vec<Value> {
    let Some(items) = resp.get("content").and_then(|v| v.as_array()) else {
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

    #[test]
    fn extract_id_reads_json_id_field() {
        let resp = json!({ "content": [{ "type": "text", "text": "{\"id\":\"mem_42\"}" }] });
        assert_eq!(extract_id(&resp), "mem_42");
    }

    #[test]
    fn extract_id_falls_back_to_raw_text() {
        let resp = json!({ "content": [{ "type": "text", "text": "mem_99" }] });
        assert_eq!(extract_id(&resp), "mem_99");
    }

    #[test]
    fn extract_id_returns_empty_when_missing() {
        let resp = json!({ "content": [] });
        assert_eq!(extract_id(&resp), "");
    }

    #[test]
    fn extract_recall_hits_parses_multiple() {
        let resp = json!({
            "content": [
                { "type": "text", "text": "{\"id\":\"a\",\"score\":0.9}" },
                { "type": "text", "text": "{\"id\":\"b\",\"score\":0.5}" },
            ]
        });
        let hits = extract_recall_hits(&resp);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["id"], "a");
    }
}
