//! `okg_sources` — what this knowledge graph is built from, and how far back.
//!
//! Why: before widening a window or adding a store, the operator (and the
//! assistant) needs to see what is already covered. Coverage is DERIVED from
//! each source's ledger rather than stored as a separate counter, so it can
//! never claim more than was actually ingested. It reports TWO coverages, not
//! one (#3892): what is in the tree, and what of that is actually searchable —
//! conflating them is exactly how an ingest could report "N entities ingested"
//! while `vector_search` found nothing.
//! What: lists every registered source with its kind, locator, destination
//! collection, watermark (item count, tombstones, oldest/newest item timestamp,
//! last run) and index coverage (synced / pending), plus the tree's bound search
//! index. Read-only, and offline-safe: the index coverage is derived from local
//! journals, never from a daemon call. It never registers or mutates anything.
//! Test: `super::tests::sources_tool_reports_registered_sources`,
//! `super::tests::sources_tool_reports_the_unsearchable_backlog`.

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
                "description": "List the sources feeding the assistant's knowledge graph, with per-source status: kind, locator, destination collection, how many items are ingested, how many are tombstoned, the oldest and newest item covered, when it last ran, and how many of its entities have actually reached the bound search index (index.synced) versus are ingested but NOT yet searchable (index.pending). Also reports which search index this tree feeds. Read-only.",
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
    let pending: usize = sources.iter().map(|s| s.index.pending).sum();
    // Resolution is pure config + path work (no daemon call), so naming the
    // bound index — or saying why there is none — costs this read nothing.
    let bound = crate::stores::bound_index_for_tree(
        &crate::agents::agents_dir_candidates(),
        &crate::tools::okg::knowledge_dir(),
        &store.root,
        crate::tools::okg::arg_str(args, "agent"),
    );
    let index = match &bound {
        Ok(b) => {
            json!({ "index": b.index, "store": b.store, "agent": b.agent, "pending": pending })
        }
        Err(reason) => json!({ "index": Value::Null, "reason": reason, "pending": pending }),
    };
    Ok(ok_json(&json!({
        "root": store.root.to_string_lossy(),
        "count": sources.len(),
        "bound_index": index,
        "sources": sources,
    })))
}
