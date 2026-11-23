//! Public trait and types for the embedded memory store.
//!
//! Why: Decouple callers from the concrete redb+usearch implementation so we
//! can later add alternate backends (e.g., Postgres+pgvector) without rewriting
//! consumers. Also forces all data access through a narrow, testable contract.
//! What: Defines `Segment` (namespace selector), `MemoryResult` (search hit),
//! and the `MemoryStore` async trait with insert/search/get/delete methods.
//! Test: Trait is exercised via `RedbUsearchStore` integration tests in the
//! sibling module — see `redb_usearch::tests`.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Logical namespace for stored records.
///
/// Why: Agent memory and code-index vectors have different dimensions,
/// churn rates, and lifecycle; keeping them in separate usearch indexes
/// and redb key spaces prevents cross-contamination on search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Segment {
    /// Legacy catch-all for agent IPC round-trips and untiered facts. Kept for
    /// backward compatibility — existing callers continue to write here.
    AgentMemory,
    /// Code chunk vectors (separate dimension/lifecycle from agent memory).
    CodeIndex,
    /// Stable architecture facts, stack choices, conventions. Low churn,
    /// long-lived; loaded at session start to ground the model.
    Context,
    /// Active goals, current sprint, live tickets. Higher churn than Context;
    /// represents what the team is doing right now.
    Brief,
    /// Past decisions, append-only. Records what was decided and why so the
    /// rationale survives even after the active brief moves on.
    History,
}

impl Segment {
    /// Key prefix used in the shared redb payload table.
    ///
    /// Why: Payloads for all segments live in one table so we only manage
    /// one file; the prefix scopes keys to their segment.
    /// What: Returns the short prefix for each variant.
    /// Test: `assert_eq!(Segment::AgentMemory.prefix(), "mem");`
    pub fn prefix(&self) -> &'static str {
        match self {
            Segment::AgentMemory => "mem",
            Segment::CodeIndex => "code",
            Segment::Context => "ctx",
            Segment::Brief => "brief",
            Segment::History => "hist",
        }
    }

    /// Parse a human-friendly name (CLI-friendly hyphen form) into a Segment.
    ///
    /// Why: The CLI export `--segment` flag accepts names like `context`,
    /// `brief`, `history`, `agent-memory`, `code-index`. Centralising the
    /// parse keeps callers consistent.
    /// What: Returns `Some(Segment)` for a known name (case-insensitive,
    /// hyphen or underscore), `None` otherwise.
    /// Test: `assert_eq!(Segment::from_name("context"), Some(Segment::Context));`
    pub fn from_name(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "agent-memory" | "mem" | "agentmemory" => Some(Segment::AgentMemory),
            "code-index" | "code" | "codeindex" => Some(Segment::CodeIndex),
            "context" | "ctx" => Some(Segment::Context),
            "brief" => Some(Segment::Brief),
            "history" | "hist" => Some(Segment::History),
            _ => None,
        }
    }
}

/// Compound segment routing spec. Wraps a `Segment` tier with optional
/// scope guards (project_id, session_id) for future multi-tenant isolation.
///
/// Why: G1-G3 added segment tiers via the bare `Segment` enum. As we add more
/// scoping dimensions (per-project memory, per-session memory), threading
/// each through `insert`/`search` as separate parameters would balloon the
/// trait surface. `MemorySegmentSpec` packages the tier with optional scope
/// guards so future scoping can be added without breaking the trait.
/// What: Plain struct with `tier`, `project_id`, `session_id`. Use
/// `MemorySegmentSpec::from(Segment::X)` for the common single-tier case,
/// or the builder methods for scoped routing.
/// Test: `segment_spec_from_segment`, `segment_spec_builder`,
/// `segment_spec_default_no_scope`, `segment_spec_into_works` below.
#[derive(Debug, Clone)]
pub struct MemorySegmentSpec {
    pub tier: Segment,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
}

impl MemorySegmentSpec {
    /// Construct a spec for `tier` with no scope guards.
    pub fn new(tier: Segment) -> Self {
        Self {
            tier,
            project_id: None,
            session_id: None,
        }
    }

    /// Attach a `project_id` scope guard to this spec.
    pub fn with_project(mut self, id: impl Into<String>) -> Self {
        self.project_id = Some(id.into());
        self
    }

    /// Attach a `session_id` scope guard to this spec.
    pub fn with_session(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }
}

impl From<Segment> for MemorySegmentSpec {
    fn from(tier: Segment) -> Self {
        Self::new(tier)
    }
}

/// A single search or lookup hit.
///
/// Why: Callers want the id, relevance score, payload, and segment
/// together without re-querying. `score` is similarity (higher = more
/// similar) derived from the underlying distance metric.
/// What: Plain serde-serializable struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResult {
    pub id: String,
    pub score: f32,
    pub payload: serde_json::Value,
    pub segment: String,
}

/// Async interface for the embedded memory store.
///
/// Why: Expressing the surface as a trait lets callers depend on the
/// abstraction (DI) and swap implementations for tests. `Send + Sync`
/// bounds make stores safely shareable across tokio tasks.
/// What: Four methods — insert, search, get, delete — all async and
/// segment-scoped. All methods return `anyhow::Result` so implementations
/// can surface backend-specific errors without leaking types.
/// Test: Exercised by `RedbUsearchStore` tests; mock impls may be added
/// later for consumers that want to isolate from disk I/O.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Insert a record with an id, a vector, and a JSON payload.
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: serde_json::Value,
    ) -> Result<()>;

    /// Return the top-`top_k` nearest records to `query_vec` in `segment`.
    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>>;

    /// Fetch the raw payload for a given id, or `None` if absent.
    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>>;

    /// Remove the record from both the vector index and payload store.
    async fn delete(&self, segment: Segment, id: &str) -> Result<()>;

    /// Insert via a `MemorySegmentSpec` (compound routing).
    ///
    /// Why: Parallel to `insert` but accepts the structured spec so future
    /// scope guards (project_id, session_id) can be honored without adding
    /// trait parameters. Default impl delegates to `insert(spec.tier, ...)`
    /// for backward compatibility — implementations may override to honor
    /// scope guards.
    /// What: Forwards `tier` to `insert`. Scope guards are currently ignored.
    /// Test: Covered indirectly by existing `insert` tests via the default
    /// impl; spec-aware impls add their own coverage.
    async fn insert_spec(
        &self,
        spec: MemorySegmentSpec,
        id: &str,
        vector: &[f32],
        payload: serde_json::Value,
    ) -> Result<()> {
        self.insert(spec.tier, id, vector, payload).await
    }

    /// Search via a `MemorySegmentSpec` (compound routing).
    ///
    /// Why: Parallel to `search` but accepts the structured spec so future
    /// scope guards can filter results without changing the trait signature.
    /// Default impl delegates to `search(spec.tier, ...)`.
    /// What: Forwards `tier` to `search`. Scope guards are currently ignored.
    /// Test: Covered indirectly by existing `search` tests via the default
    /// impl; spec-aware impls add their own coverage.
    async fn search_spec(
        &self,
        spec: &MemorySegmentSpec,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        self.search(spec.tier, query_vec, top_k).await
    }

    /// List all segments that contain at least one record.
    ///
    /// Why: Memory introspection tools (CLI status, migration utilities) need
    /// to know which segments are populated without scanning every record.
    /// What: Returns the `Segment` variants whose backing storage has ≥1 row.
    /// Test: `list_segments_returns_only_populated` in `redb_usearch::tests`.
    async fn list_segments(&self) -> Result<Vec<Segment>> {
        Err(anyhow::anyhow!(
            "list_segments not implemented for this store"
        ))
    }

    /// Move a record from one segment to another.
    ///
    /// Why: Records get reclassified as their lifecycle changes — e.g., a
    /// brief item becomes history once a decision is locked in. Letting
    /// callers move records preserves the embedding/payload without
    /// recomputation.
    /// What: Reads the record from `from`, inserts into `to`, deletes from
    /// `from`. Atomicity is best-effort — implementations may not roll back
    /// partial failures.
    /// Test: `move_segment_transfers_and_deletes` in `redb_usearch::tests`.
    async fn move_segment(&self, id: &str, from: Segment, to: Segment) -> Result<()> {
        let _ = (id, from, to);
        Err(anyhow::anyhow!(
            "move_segment not implemented for this store"
        ))
    }

    /// Evict the in-memory vector index for `segment` to free RAM.
    ///
    /// Why: HNSW indexes can be large; after a period of search inactivity
    /// it's wasteful to keep them resident. Stores that hold an in-memory
    /// HNSW (e.g., `RedbUsearchStore`) can drop it here so memory is
    /// reclaimed without losing data — durable state lives on disk.
    /// What: Default impl is a no-op for stores that don't carry an
    /// evictable in-memory index (mocks, tests). Implementations that do
    /// must guarantee `warm_segment` plus subsequent calls still serve
    /// correct results — eviction is invisible to callers.
    /// Test: `RedbUsearchStore::tests::evict_then_warm_returns_same_results`.
    async fn evict_segment(&self, segment: Segment) -> Result<()> {
        let _ = segment;
        Ok(())
    }

    /// Reload `segment`'s in-memory index from durable storage if it was
    /// evicted (or noop if already warm).
    ///
    /// Why: Pairs with `evict_segment`. Callers (e.g., `CodeIndexer`) call
    /// this before serving a query so the eviction window is transparent.
    /// What: Default impl is a no-op. Implementations that evict must make
    /// this idempotent — calling repeatedly when already warm must be cheap.
    /// Test: `RedbUsearchStore::tests::evict_then_warm_returns_same_results`.
    async fn warm_segment(&self, segment: Segment) -> Result<()> {
        let _ = segment;
        Ok(())
    }

    /// Returns true if `segment`'s in-memory index is currently resident.
    ///
    /// Why: Tests need to assert eviction actually happened; production
    /// callers can use this to skip warm-up work when redundant.
    /// What: Default impl returns `true` since stores without an evictable
    /// index are always "warm".
    /// Test: `RedbUsearchStore::tests::evict_then_warm_returns_same_results`.
    async fn is_segment_warm(&self, segment: Segment) -> Result<bool> {
        let _ = segment;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `From<Segment>` is the most ergonomic conversion path; verify it
    /// produces the right tier with empty scope guards.
    /// What: `MemorySegmentSpec::from(Segment::Context)` yields a spec with
    /// `tier == Context` and no scopes set.
    /// Test: This test.
    #[test]
    fn segment_spec_from_segment() {
        let spec = MemorySegmentSpec::from(Segment::Context);
        assert_eq!(spec.tier, Segment::Context);
        assert!(spec.project_id.is_none());
        assert!(spec.session_id.is_none());
    }

    /// Why: The builder pattern is the API for adding scope guards; verify
    /// chained calls populate both fields correctly.
    /// What: `.new(Brief).with_project("p1").with_session("s1")` sets all
    /// three fields as expected.
    /// Test: This test.
    #[test]
    fn segment_spec_builder() {
        let spec = MemorySegmentSpec::new(Segment::Brief)
            .with_project("p1")
            .with_session("s1");
        assert_eq!(spec.tier, Segment::Brief);
        assert_eq!(spec.project_id.as_deref(), Some("p1"));
        assert_eq!(spec.session_id.as_deref(), Some("s1"));
    }

    /// Why: Default construction (no builder calls) must leave scope guards
    /// unset so the spec degrades to a bare-tier route.
    /// What: `from(AgentMemory)` produces a spec with `project_id == None`
    /// and `session_id == None`.
    /// Test: This test.
    #[test]
    fn segment_spec_default_no_scope() {
        let spec = MemorySegmentSpec::from(Segment::AgentMemory);
        assert_eq!(spec.tier, Segment::AgentMemory);
        assert_eq!(spec.project_id, None);
        assert_eq!(spec.session_id, None);
    }

    /// Why: Verify the type-inferred `Into` form works so callers can write
    /// `let s: MemorySegmentSpec = Segment::History.into();` without naming
    /// the impl explicitly.
    /// What: `Segment::History.into()` resolves to a `MemorySegmentSpec`
    /// with `tier == History`.
    /// Test: This test.
    #[test]
    fn segment_spec_into_works() {
        let spec: MemorySegmentSpec = Segment::History.into();
        assert_eq!(spec.tier, Segment::History);
        assert!(spec.project_id.is_none());
        assert!(spec.session_id.is_none());
    }
}
