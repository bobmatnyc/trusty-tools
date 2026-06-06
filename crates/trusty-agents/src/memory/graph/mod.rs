//! Agent memory graph — persist IPC round-trips as a searchable, traversable graph.
//!
//! Why: Every agent prompt/response is a potentially-useful artifact. Persisting
//! them with embeddings enables semantic recall across sessions; persisting
//! temporal and delegation edges enables replaying or tracing workflows. The
//! graph lives on top of the existing `MemoryStore` + `Embedder` layers so we
//! don't need a separate graph database.
//! What: Defines `AgentSession` (node payload) and `MemoryGraph` (facade) with
//! `record`, `search`, `get_session`, `get_run`, and `get_children`. Edges are
//! encoded as specially-keyed entries in `Segment::AgentMemory` so we can reuse
//! the same redb/usearch backend; zero-vector edge rows are filtered out of
//! semantic search results.
//! Test: See `tests` submodule — round-trip record+search, temporal ordering
//! within a run, parent→children delegation traversal, and edge-row exclusion
//! on search. (Split per #366: types + impl here, tests in `tests.rs`.)

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::code_store::CodeStore;
use super::embed::Embedder;
use super::session_store::SessionStore;
use super::store::{MemoryResult, MemoryStore, Segment};

/// One agent IPC round-trip captured as a node in the memory graph.
///
/// Why: Bundles everything callers need to reason about a past turn — who ran
/// (agent_name), when (timestamp), what was asked (prompt), what was returned
/// (response), and how this sits in a larger workflow (workflow_run_id, phase,
/// parent_id). Serialized as JSON into the store payload.
/// What: Plain serde struct; `id` is a fresh UUIDv4 assigned by the caller.
/// Test: `record_and_search_round_trip` verifies serialization survives a
/// store insert + retrieval via search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    pub agent_name: String,
    pub workflow_run_id: String,
    pub phase: String,
    pub prompt: String,
    pub response: String,
    pub timestamp: DateTime<Utc>,
    pub parent_id: Option<String>,
    /// Optional graph-native segment tier this session belongs to.
    ///
    /// Why: With Context/Brief/History segments now first-class, callers can
    /// route a session to a specific tier (e.g., a decision-recording phase
    /// writes to `History`). When unset (the default), `record()` falls back
    /// to `Segment::AgentMemory` so existing call sites remain unchanged.
    /// What: `None` means legacy AgentMemory routing; `Some(seg)` overrides.
    /// Test: Existing tests construct sessions without setting `segment` and
    /// continue to pass — backward compatibility verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<Segment>,
}

/// Facade that records `AgentSession`s and exposes graph-ish lookups.
///
/// Why: Keeps the store + embedder wiring in one place so callers
/// (`SubprocessAgentRunner`, future CLI inspection tools) can depend on a
/// small, ergonomic interface. The internals decide when to embed, when to
/// write edge rows, and how to reconstruct traversals.
/// What: Holds `Arc<dyn MemoryStore>` + `Arc<dyn Embedder>` so both are
/// trivially cloneable and shareable across tokio tasks.
/// Test: All behaviors covered by the `tests` module using mock store/embedder.
pub struct MemoryGraph {
    store: Arc<dyn MemoryStore>,
    embedder: Arc<dyn Embedder>,
}

/// Prefix for edge payload rows. Used for filtering out of search results.
const EDGE_PREFIX: &str = "edge:";
/// Prefix for "last session in a run" tracking rows.
const RUN_LAST_PREFIX: &str = "run:";
/// Prefix for "ordered list of session ids in a run" rows.
const RUN_SESSIONS_PREFIX: &str = "run-sessions:";
/// Prefix for "children ids of a parent session" rows.
const CHILDREN_PREFIX: &str = "children:";

impl MemoryGraph {
    /// Construct a new graph over the given store and embedder.
    pub fn new(store: Arc<dyn MemoryStore>, embedder: Arc<dyn Embedder>) -> Self {
        Self { store, embedder }
    }

    /// Construct a graph wired to the per-session agent-memory store.
    ///
    /// Why: In the multi-session world the caller holds both a `SessionStore`
    /// (for agent memory) and a `CodeStore` (for the shared code index); this
    /// helper makes the intent explicit at the call site. The graph currently
    /// only writes into `Segment::AgentMemory` so the `CodeStore` parameter is
    /// retained for symmetry / future use (e.g., cross-linking sessions to
    /// code chunks) — it's taken now so the signature doesn't have to change
    /// later.
    /// What: Discards `code_store` today and wraps `session_store` as the
    /// backing store. Returns a standard `MemoryGraph`.
    /// Test: Used by `src/subprocess.rs` wiring; exercised indirectly by any
    /// integration that constructs a session-scoped graph.
    pub fn new_session(
        session_store: Arc<SessionStore>,
        _code_store: Arc<CodeStore>,
        embedder: Arc<dyn Embedder>,
    ) -> Self {
        let store: Arc<dyn MemoryStore> = session_store;
        Self { store, embedder }
    }

    /// Merge semantic-search results across every session under `sessions_dir`.
    ///
    /// Why: When a user asks "have I done this before?" they rarely remember
    /// which run it happened in. This helper opens each session's store,
    /// searches it, filters out edge/auxiliary rows, and returns the global
    /// top-k by similarity.
    /// What: Enumerates child directories of `sessions_dir` (skipping the
    /// `index.redb` file), opens each as a `SessionStore`, calls `search` on
    /// the query embedding with `top_k * 2` (headroom for filtering), then
    /// merges and sorts descending by score and truncates to `top_k`.
    /// Test: `search_all_sessions_merges_across_run_ids`.
    pub async fn search_all_sessions(
        sessions_dir: &Path,
        embedder: &dyn Embedder,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        let query_vec = embedder
            .embed_single(query)
            .context("failed to embed cross-session search query")?;
        let fetch_k = top_k.saturating_mul(2).max(top_k);
        let vector_dim = embedder.dimension();

        let mut all: Vec<MemoryResult> = Vec::new();
        if !sessions_dir.exists() {
            return Ok(all);
        }

        for entry in std::fs::read_dir(sessions_dir)
            .with_context(|| format!("reading sessions dir {}", sessions_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            let run_id = match path.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            // Each subdirectory is a session; open and search it. Errors on
            // a single session don't abort the cross-session query — we log
            // and skip so one bad dir doesn't blind the user to others.
            let store = match SessionStore::open(sessions_dir, &run_id, vector_dim) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(run_id = %run_id, error = %e, "failed to open session store; skipping");
                    continue;
                }
            };
            let hits = match store
                .search(Segment::AgentMemory, &query_vec, fetch_k)
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(run_id = %run_id, error = %e, "search failed; skipping");
                    continue;
                }
            };
            for h in hits {
                if !Self::is_auxiliary_key(&h.id) {
                    all.push(h);
                }
            }
        }

        // Stable sort by score desc, then truncate.
        all.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all.truncate(top_k);
        Ok(all)
    }

    /// Record a session: insert the node, embed prompt+response, and maintain
    /// temporal / delegation / run-membership edges.
    ///
    /// Why: Persisting metadata alongside the vector makes the node immediately
    /// searchable AND traversable. Edge maintenance (`FOLLOWED_BY`,
    /// `DELEGATED_TO`, run-membership) happens inline so callers don't have to
    /// remember to wire it up — one `record` call captures the full context.
    /// What: Writes the session payload, then writes up to four auxiliary
    /// rows: a `FOLLOWED_BY` edge (if another session preceded it in the same
    /// run), a `DELEGATED_TO` edge (if `parent_id` is set), an append to the
    /// run's session list, and an append to the parent's children list.
    /// Test: Covered by every test in this module — all insert at least once.
    pub async fn record(&self, session: AgentSession) -> Result<()> {
        let payload = serde_json::to_value(&session).context("failed to serialize AgentSession")?;

        let embed_text = format!("{}\n{}", session.prompt, session.response);
        let vec = self
            .embedder
            .embed_single(&embed_text)
            .context("failed to embed session prompt+response")?;

        // Route the node to its declared segment tier; fall back to the legacy
        // AgentMemory bucket so callers that don't opt in stay backward compat.
        // Edge / marker / list rows below stay on AgentMemory regardless — they
        // are graph plumbing, not tier-routed knowledge.
        let node_segment = session.segment.unwrap_or(Segment::AgentMemory);

        self.store
            .insert(node_segment, &session.id, &vec, payload)
            .await
            .context("failed to insert session payload")?;

        // FOLLOWED_BY: link the previous session in this run to this one.
        if !session.workflow_run_id.is_empty() {
            let last_key = format!("{RUN_LAST_PREFIX}{}:last", session.workflow_run_id);
            if let Some(prev) = self
                .store
                .get(Segment::AgentMemory, &last_key)
                .await
                .context("failed to read last-session marker")?
                && let Some(prev_id) = prev.as_str()
            {
                let edge_key = format!("{EDGE_PREFIX}followed_by:{}:{}", prev_id, session.id);
                let zero_vec = self.zero_vec();
                let edge_payload = serde_json::json!({
                    "kind": "followed_by",
                    "from": prev_id,
                    "to": session.id,
                });
                self.store
                    .insert(Segment::AgentMemory, &edge_key, &zero_vec, edge_payload)
                    .await
                    .context("failed to insert followed_by edge")?;
            }

            // Update "last session in run" marker.
            let zero_vec = self.zero_vec();
            let marker = serde_json::Value::String(session.id.clone());
            self.store
                .insert(Segment::AgentMemory, &last_key, &zero_vec, marker)
                .await
                .context("failed to update last-session marker")?;

            // Append to run's session-id list.
            self.append_id_list(
                &format!("{RUN_SESSIONS_PREFIX}{}", session.workflow_run_id),
                &session.id,
            )
            .await
            .context("failed to append session to run list")?;
        }

        // DELEGATED_TO: parent -> this session.
        if let Some(pid) = &session.parent_id {
            let edge_key = format!("{EDGE_PREFIX}delegated_to:{}:{}", pid, session.id);
            let zero_vec = self.zero_vec();
            let edge_payload = serde_json::json!({
                "kind": "delegated_to",
                "from": pid,
                "to": session.id,
            });
            self.store
                .insert(Segment::AgentMemory, &edge_key, &zero_vec, edge_payload)
                .await
                .context("failed to insert delegated_to edge")?;

            self.append_id_list(&format!("{CHILDREN_PREFIX}{pid}"), &session.id)
                .await
                .context("failed to append child to parent children list")?;
        }

        Ok(())
    }

    /// Semantic search over stored sessions.
    ///
    /// Why: Callers want to recall "what have I done like this before?" — a
    /// vector-similarity query over prompt+response answers that.
    /// What: Embeds the query, calls `store.search`, then filters out edge /
    /// marker / list rows (they share the same segment but aren't sessions).
    /// Test: `search_excludes_edge_entries` verifies filtering; other tests
    /// confirm sessions surface correctly.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<MemoryResult>> {
        let vec = self
            .embedder
            .embed_single(query)
            .context("failed to embed search query")?;
        // Over-fetch a bit so filtering doesn't starve top_k when many edge
        // rows are in the neighborhood. 4x is arbitrary but generous.
        let fetch_k = top_k.saturating_mul(4).max(top_k);
        let raw = self
            .store
            .search(Segment::AgentMemory, &vec, fetch_k)
            .await?;
        let filtered: Vec<MemoryResult> = raw
            .into_iter()
            .filter(|r| !Self::is_auxiliary_key(&r.id))
            .take(top_k)
            .collect();
        Ok(filtered)
    }

    /// Fetch a single session by id.
    ///
    /// Why: Traversal results (children, run lists) give back ids; callers
    /// then want to hydrate each into a full `AgentSession`.
    /// What: Reads the payload and `serde_json::from_value`s it.
    /// Test: `get_run_returns_sessions_in_temporal_order` and
    /// `get_children_returns_delegated_sessions` exercise this path.
    pub async fn get_session(&self, session_id: &str) -> Result<Option<AgentSession>> {
        let raw = self.store.get(Segment::AgentMemory, session_id).await?;
        let Some(v) = raw else {
            return Ok(None);
        };
        let session: AgentSession =
            serde_json::from_value(v).context("failed to deserialize AgentSession payload")?;
        Ok(Some(session))
    }

    /// All sessions in a workflow run, sorted by timestamp ascending.
    ///
    /// Why: Workflow replay / debugging needs the full ordered sequence.
    /// What: Reads the `run-sessions:{run_id}` id-list payload, hydrates each
    /// id, sorts by `timestamp`. Missing ids (e.g. evicted) are silently
    /// skipped rather than producing a partial error.
    /// Test: `get_run_returns_sessions_in_temporal_order`.
    pub async fn get_run(&self, workflow_run_id: &str) -> Result<Vec<AgentSession>> {
        let key = format!("{RUN_SESSIONS_PREFIX}{workflow_run_id}");
        let ids = self.read_id_list(&key).await?;
        let mut sessions = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(s) = self.get_session(&id).await? {
                sessions.push(s);
            }
        }
        sessions.sort_by_key(|s| s.timestamp);
        Ok(sessions)
    }

    /// All sessions delegated from `session_id` (direct children only).
    ///
    /// Why: Drilling into a PM→sub-agent call tree — e.g. "what did the PM
    /// hand off on this turn?"
    /// What: Reads the `children:{session_id}` id-list and hydrates each entry.
    /// Test: `get_children_returns_delegated_sessions`.
    pub async fn get_children(&self, session_id: &str) -> Result<Vec<AgentSession>> {
        let key = format!("{CHILDREN_PREFIX}{session_id}");
        let ids = self.read_id_list(&key).await?;
        let mut sessions = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(s) = self.get_session(&id).await? {
                sessions.push(s);
            }
        }
        sessions.sort_by_key(|s| s.timestamp);
        Ok(sessions)
    }

    // --- helpers --------------------------------------------------------

    /// Return true if a store id belongs to an edge/marker/list row rather
    /// than an actual session node.
    fn is_auxiliary_key(id: &str) -> bool {
        id.starts_with(EDGE_PREFIX)
            || id.starts_with(RUN_LAST_PREFIX)
            || id.starts_with(RUN_SESSIONS_PREFIX)
            || id.starts_with(CHILDREN_PREFIX)
    }

    /// Build a zero-filled vector of the embedder's dimension.
    ///
    /// Why: Edge / marker / list rows aren't meant to be searched semantically
    /// but the store requires a vector per row. Zero vectors cluster together
    /// in cosine space, are cheap, and — crucially — are filtered out of
    /// search results by id prefix anyway.
    fn zero_vec(&self) -> Vec<f32> {
        vec![0.0f32; self.embedder.dimension()]
    }

    /// Read a JSON-array-of-strings payload, returning empty vec if absent.
    async fn read_id_list(&self, key: &str) -> Result<Vec<String>> {
        let raw = self.store.get(Segment::AgentMemory, key).await?;
        let Some(v) = raw else {
            return Ok(Vec::new());
        };
        let list: Vec<String> =
            serde_json::from_value(v).context("failed to deserialize id list payload")?;
        Ok(list)
    }

    /// Append `id` to the JSON-array payload at `key`, creating if missing.
    async fn append_id_list(&self, key: &str, id: &str) -> Result<()> {
        let mut list = self.read_id_list(key).await?;
        list.push(id.to_string());
        let payload = serde_json::to_value(&list).context("failed to serialize updated id list")?;
        let zero_vec = self.zero_vec();
        self.store
            .insert(Segment::AgentMemory, key, &zero_vec, payload)
            .await
    }
}
