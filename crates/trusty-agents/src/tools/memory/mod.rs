//! `memory_recall` and `vector_search` tools — agent-side memory access.
//!
//! Why: #53 — agents need two complementary lookup surfaces: a semantic
//! `memory_recall` that queries the embedded `RedbUsearchStore`
//! (`Segment::AgentMemory`) for previously-stored facts/decisions, and a
//! `vector_search` that queries the local embedded code index at
//! `.trusty-agents/code/` (semantic code search). Both are optional — when the
//! underlying store is unavailable, the tool returns a structured payload
//! rather than failing the LLM loop, so agents can gracefully skip the
//! lookup and proceed with the task.
//! What:
//!   - `MemoryRecallTool` (`recall`) embeds the query via `FastEmbedder` and
//!     runs HNSW search against `Segment::AgentMemory` in the injected
//!     `MemoryBackend`. When constructed without a backend it returns a
//!     graceful "not available" JSON payload.
//!   - `VectorSearchTool` (`vector_search`) tries to open the embedded
//!     `CodeStore`; if absent or unreadable, it falls back to a plain
//!     `grep`-style scan via the existing `GrepFilesTool`.
//! Test: See `tests` submodule — both tools return a graceful error when the
//! underlying store is missing, and both appear in the research-agent registry.

mod recall;
mod vector_search;

pub use recall::MemoryRecallTool;
pub use vector_search::VectorSearchTool;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
