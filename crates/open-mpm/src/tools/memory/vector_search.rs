//! `vector_search` tool — semantic code search via the embedded code index.
//!
//! Why: Research agents benefit from fuzzy, intent-based lookups over the
//! codebase; the exact-regex `grep_files` tool complements it but doesn't
//! match paraphrases. When no index exists yet (first run, or a project
//! that hasn't run `--reindex`), the tool degrades to a regex fallback so
//! the agent still gets useful results.
//! What: `VectorSearchTool` opens `.open-mpm/state/code/` if present, embeds
//! the query via `FastEmbedder`, returns top-k hits; otherwise falls back to
//! `GrepFilesTool` with the query treated as a regex.
//! Test: See `super::tests` — `vector_search_returns_graceful_error_without_index`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{CodeStore, Embedder, FastEmbedder};
use crate::tools::fs_reader::GrepFilesTool;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::recall::{EMBED_DIM, HIT_MAX_CHARS};

/// Tool: `vector_search` — semantic code search via embedded index.
///
/// Why: Research agents benefit from fuzzy, intent-based lookups over the
/// codebase; the exact-regex `grep_files` tool complements it but doesn't
/// match paraphrases. When no index exists yet (first run, or a project
/// that hasn't run `--reindex`), the tool degrades to a regex fallback so
/// the agent still gets useful results.
/// What: Opens `.open-mpm/code/` if present, embeds the query via
/// `FastEmbedder`, returns top-k hits; otherwise falls back to
/// `GrepFilesTool` with the query treated as a regex.
/// Test: `vector_search_returns_graceful_error_without_index`.
pub struct VectorSearchTool {
    code_dir: PathBuf,
    fallback: Arc<GrepFilesTool>,
}

impl VectorSearchTool {
    /// Construct with the default index path (`.open-mpm/state/code/`).
    ///
    /// Why: Matches the indexer's default output location so the tool finds
    /// the embedded code without requiring explicit config.
    /// What: Plain struct literal with the bundled `GrepFilesTool` fallback.
    /// Test: `vector_search_returns_graceful_error_without_index`.
    pub fn new() -> Self {
        Self {
            code_dir: PathBuf::from(".open-mpm").join("state").join("code"),
            fallback: Arc::new(GrepFilesTool::new()),
        }
    }

    /// Override the on-disk index location (used by tests).
    #[allow(dead_code)]
    pub fn with_code_dir(mut self, path: PathBuf) -> Self {
        self.code_dir = path;
        self
    }

    /// Path accessor used by tests.
    #[allow(dead_code)]
    pub fn code_dir(&self) -> &Path {
        &self.code_dir
    }
}

impl Default for VectorSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for VectorSearchTool {
    fn name(&self) -> &str {
        "vector_search"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "vector_search",
                "description": "Semantic search over indexed project code (embedded vector index). Falls back to regex search when the index is unavailable. Returns JSON array of hits with path and snippet.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language or keyword query describing the code to find."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of hits to return (default 5).",
                            "minimum": 1,
                            "maximum": 50
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("vector_search: missing required 'query' string");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 50) as usize)
            .unwrap_or(5);

        // Fast path: embedded index available.
        if self.code_dir.exists() {
            match semantic_query(&self.code_dir, query, limit).await {
                Ok(payload) => return ToolResult::ok(payload),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        code_dir = %self.code_dir.display(),
                        "vector_search: semantic query failed; falling back to grep"
                    );
                    // Fall through to the grep fallback.
                }
            }
        }

        // Fallback: delegate to grep_files, but dress the result in the same
        // JSON envelope so the agent sees a consistent shape.
        let grep_args = json!({
            "pattern": query,
            "max_results": limit,
        });
        let r = self.fallback.execute(grep_args).await;
        let body = r.content().to_string();
        ToolResult::ok(format!(
            "{{\"mode\":\"grep_fallback\",\"reason\":\"no vector index at {}\",\"results\":{}}}",
            self.code_dir.display(),
            serde_json::to_string(&body).unwrap_or_else(|_| "\"\"".to_string())
        ))
    }
}

/// Run a semantic query against the on-disk `CodeStore`.
///
/// Why: Keeping the embedding + search plumbing in one helper means the
/// tool's `execute` path stays small and easy to read.
/// What: Opens `CodeStore` at `code_dir`, embeds the query, returns up to
/// `limit` hits as a JSON array `[{path, score, snippet}, ...]`.
/// Test: Covered via the `vector_search` tool's graceful-error test today;
/// a populated-index test is future work.
async fn semantic_query(code_dir: &Path, query: &str, limit: usize) -> anyhow::Result<String> {
    let store = CodeStore::open(code_dir, EMBED_DIM)?;
    let embedder = FastEmbedder::new()?;
    let vec = embedder.embed_single(query)?;
    let hits = store.search(Segment::CodeIndex, &vec, limit).await?;

    let out: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            // Payload shape depends on the indexer — pull common fields if
            // present, otherwise stringify the whole payload as snippet.
            let path = h.payload.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let snippet_raw = h
                .payload
                .get("content")
                .or_else(|| h.payload.get("snippet"))
                .or_else(|| h.payload.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| h.payload.to_string());
            let snippet = snippet_raw.chars().take(HIT_MAX_CHARS).collect::<String>();
            json!({
                "id": h.id,
                "path": path,
                "score": h.score,
                "snippet": snippet,
            })
        })
        .collect();

    Ok(serde_json::to_string(&out)?)
}
