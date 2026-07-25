//! `okg_sources` — what this knowledge graph is built from, and how far back.
//!
//! Why: before widening a window or adding a store, the operator (and the
//! assistant) needs to see what is already covered. Coverage is DERIVED from
//! each source's ledger rather than stored as a separate counter, so it can
//! never claim more than was actually ingested.
//! What: lists every registered source with its kind, locator, destination
//! collection, and watermark (item count, tombstones, oldest/newest item
//! timestamp, last run). Read-only — it never registers or mutates anything.
//! Test: `super::tests::sources_tool_reports_registered_sources`.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::okg::{ok_json, resolve_store, with_root};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `okg_sources` — list the OKG's sources and their coverage.
pub struct OkgSourcesTool;

impl OkgSourcesTool {
    /// Construct the tool. Performs no I/O.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OkgSourcesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for OkgSourcesTool {
    fn name(&self) -> &str {
        "okg_sources"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "okg_sources",
                "description": "List the sources feeding the assistant's knowledge graph, with per-source status: kind, locator, destination collection, how many items are ingested, how many are tombstoned, the oldest and newest item covered, and when it last ran. Read-only.",
                "parameters": {
                    "type": "object",
                    "properties": with_root(json!({})),
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match run(&args) {
            Ok(result) => result,
            Err(e) => ToolResult::err(format!("okg_sources failed: {e}")),
        }
    }
}

/// The fallible body — any `Err` becomes an error result.
fn run(args: &Value) -> anyhow::Result<ToolResult> {
    let store = resolve_store(args)?;
    let sources = store.okg_sources()?;
    Ok(ok_json(&json!({
        "root": store.root.to_string_lossy(),
        "count": sources.len(),
        "sources": sources,
    })))
}
