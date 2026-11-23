//! `okg_search`: a read-only text search rooted at one instance's OKG (#11650).
//!
//! Why: the ask engine's registry holds MCP tools plus a search over the
//! instance's own OKG tree, and nothing else. trusty-code's `search_code`
//! picks an ancestor index when the OKG has none, so it is not rooted.
//! What: [`OkgSearchTool`] walks `<home>/okg` without following symlinks and
//! returns `path:line: text` for each line containing the query,
//! case-insensitively. It reads only; it never writes, spawns or connects.
//! Test: `super::tests::tools_tests::okg_search_finds_lines_under_the_okg_only`,
//! `super::tests::tools_tests::okg_search_rejects_an_empty_query`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_code::tools::{ToolExecutor, ToolResult};

/// The tool's registered name.
pub const OKG_SEARCH_TOOL_NAME: &str = "okg_search";

/// Matches returned when the model does not ask for a number.
const DEFAULT_MAX_RESULTS: usize = 20;

/// Hard cap on returned matches.
const MAX_RESULTS: usize = 50;

/// Files over this size are skipped.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Characters kept from one matching line.
const MAX_LINE_CHARS: usize = 300;

/// File extensions searched: the OKG tree is markdown.
const EXTENSIONS: &[&str] = &["md", "markdown", "txt"];

/// Search over one instance's OKG directory.
/// Test: `super::tests::tools_tests::okg_search_finds_lines_under_the_okg_only`.
pub struct OkgSearchTool {
    root: PathBuf,
}

impl OkgSearchTool {
    /// A tool rooted at `okg_dir`. Touches no filesystem.
    pub fn new(okg_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: okg_dir.into(),
        }
    }
}

#[async_trait]
impl ToolExecutor for OkgSearchTool {
    fn name(&self) -> &str {
        OKG_SEARCH_TOOL_NAME
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": OKG_SEARCH_TOOL_NAME,
                "description": "Search this assistant's own knowledge graph (OKG) for lines \
                                containing a phrase, case-insensitively. Read-only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Text to look for." },
                        "max_results": {
                            "type": "integer",
                            "description": "Most matches to return (default 20, at most 50)."
                        }
                    },
                    "required": ["query"]
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_lowercase();
        if query.is_empty() {
            return ToolResult::err("okg_search needs a non-empty `query`");
        }
        let limit = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_RESULTS, |n| (n as usize).clamp(1, MAX_RESULTS));
        if !self.root.is_dir() {
            return ToolResult::err(format!(
                "this assistant's OKG directory `{}` does not exist",
                self.root.display()
            ));
        }
        let root = self.root.clone();
        match tokio::task::spawn_blocking(move || search(&root, &query, limit)).await {
            Ok(hits) if hits.is_empty() => ToolResult::ok("no matches in this assistant's OKG"),
            Ok(hits) => ToolResult::ok(hits.join("\n")),
            Err(e) => ToolResult::err(format!("okg_search failed: {e}")),
        }
    }
}

/// Up to `limit` `relpath:line: text` hits for `query` (already lowercased).
fn search(root: &Path, query: &str, limit: usize) -> Vec<String> {
    let mut hits = Vec::new();
    let files = walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| EXTENSIONS.contains(&x))
        })
        .filter(|e| e.metadata().is_ok_and(|m| m.len() <= MAX_FILE_BYTES));
    for entry in files {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        for (n, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(query) {
                let shown: String = line.trim().chars().take(MAX_LINE_CHARS).collect();
                hits.push(format!("{}:{}: {shown}", rel.display(), n + 1));
                if hits.len() >= limit {
                    return hits;
                }
            }
        }
    }
    hits
}
