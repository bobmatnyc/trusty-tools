//! User-scoped memory store at `~/.trusty-agents/memory/`.
//!
//! Why: Project memory captures context specific to one codebase; user memory
//! captures preferences, patterns, and cross-project learnings that should
//! survive project switches. By injecting user memory at *lower priority*
//! (after project context) we ensure project-specific knowledge takes
//! precedence while still enriching prompts with durable user knowledge.
//! What: `UserMemoryStore` wraps `RedbUsearchStore` opened at the user's
//! `~/.trusty-agents/memory/` directory. The `insert` / `search_relevant`
//! methods delegate to the inner store with a 384-dim all-MiniLM-L6-v2
//! embedding dimension (same as the project store).
//!
//! Note: This module previously also slurped `*.md` / `*.txt` snippets from
//! `~/.kuzu-memory/user/` produced by the now-archived KùzuDB Python shim
//! and rendered them as a `## User Memory` prompt suffix. That path was
//! removed; user-level recall is now expected to flow through
//! `memory_recall` semantic search against the embedded store.
//! Test: Open with HOME set to a tempdir, assert `open` succeeds and
//! `to_prompt_suffix` returns empty string (no static suffix anymore).

use anyhow::Result;
use tokio::fs;

use super::redb_usearch::RedbUsearchStore;
use super::store::{MemoryStore, Segment};

/// Embedding dimension for all-MiniLM-L6-v2 (fastembed default).
///
/// Why: Must match the dimension used by `FastEmbedder` so vectors are
/// compatible between the user store and project store.
const EMBED_DIM: usize = 384;

/// User-scoped memory store backed by redb + usearch at `~/.trusty-agents/memory/`.
///
/// Why: Separating user memory from project memory prevents project-specific
/// data from polluting cross-project recall, and lets the store survive
/// project deletion.
/// What: Wraps `RedbUsearchStore` for vector search + persistence.
pub struct UserMemoryStore {
    inner: RedbUsearchStore,
}

impl UserMemoryStore {
    /// Open (or create) the user-scoped store at `~/.trusty-agents/memory/`.
    ///
    /// Why: A single `open` call handles first-run directory creation and
    /// store initialization so callers have one entry point.
    /// What: Creates `~/.trusty-agents/memory/` if absent, opens
    /// `RedbUsearchStore` with 384-dim vectors.
    /// Test: Call with HOME pointing to a tempdir; assert Ok and that
    /// `to_prompt_suffix` returns empty.
    pub async fn open() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let path = home.join(".trusty-agents").join("memory");
        fs::create_dir_all(&path).await?;
        let inner = RedbUsearchStore::open(&path, EMBED_DIM)?;
        Ok(Self { inner })
    }

    /// Render user memory as a prompt suffix (lower priority than project memory).
    ///
    /// Why: Kept for API compatibility with callers that still ask for a
    /// static suffix. Returns empty now that we no longer batch-inject
    /// kuzu-memory snippets — recall happens on demand via `memory_recall`.
    /// What: Always returns an empty string.
    /// Test: Construct via `open` with a tempdir HOME; assert suffix is empty.
    pub fn to_prompt_suffix(&self) -> String {
        String::new()
    }

    /// Search user memory for the most relevant snippets given a query vector.
    ///
    /// Why: Semantic search over user memory lets agents retrieve durable
    /// cross-project learnings that are relevant to the current task without
    /// injecting all user memory unconditionally (which would bloat prompts).
    /// What: Delegates to `RedbUsearchStore::search` on the `AgentMemory`
    /// segment, returns the payload `content` string from each hit.
    /// Test: Insert a record, embed the same text, assert `search_relevant`
    /// returns it in the top-k results.
    pub async fn search_relevant(&self, query_vec: &[f32], top_k: usize) -> Result<Vec<String>> {
        let results = self
            .inner
            .search(Segment::AgentMemory, query_vec, top_k)
            .await?;
        Ok(results
            .into_iter()
            .filter_map(|r| {
                r.payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect())
    }

    /// Store a new user-level memory (preferences, patterns, cross-project learnings).
    ///
    /// Why: Agents can accumulate user-scoped learnings during task execution
    /// (e.g., "user prefers snake_case", "project X uses actor pattern") and
    /// persist them so future sessions benefit.
    /// What: Wraps `RedbUsearchStore::insert` for the `AgentMemory` segment.
    /// Uses a UUID v4 as the record id. Payload is `{"content": content}`.
    /// Test: Insert, then search with the same embedding, assert content matches.
    pub async fn insert(&self, content: &str, embedding: &[f32]) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.inner
            .insert(
                Segment::AgentMemory,
                &id,
                embedding,
                serde_json::json!({ "content": content }),
            )
            .await
    }
}
