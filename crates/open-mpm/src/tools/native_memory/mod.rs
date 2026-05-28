//! Native memory tools (#133, #137) — typed store/retrieve/list for agent memory.
//!
//! Why: Replaces ad-hoc shell access to memory backends with strongly-typed
//! per-operation tools. When a real `MemoryStore` + `Embedder` are injected,
//! these tools persist key/value entries into `Segment::AgentMemory` so they
//! survive process restarts. When no backend is wired (default constructor),
//! they degrade gracefully — `store_memory` / `retrieve_memory` /
//! `list_memory_keys` all return a structured `"memory store not available"`
//! payload so the agent can continue.
//! What: A shared `MemoryBackend` (`backend`) plus three tools —
//! `store_memory` (`store_memory`), `retrieve_memory` (`retrieve_memory`),
//! `list_memory_keys` (`list_keys`). Keys are prefixed with `kv:` internally
//! so they don't collide with the session/edge rows written by `MemoryGraph`.
//! A sibling `kv-index` row tracks the set of known keys.
//! Test: Name / schema / happy-path / missing-input / graceful-degradation
//! cases in `tests`.

mod backend;
mod list_keys;
mod retrieve_memory;
mod store_memory;

pub use backend::MemoryBackend;
pub use list_keys::ListMemoryKeysTool;
pub use retrieve_memory::RetrieveMemoryTool;
pub use store_memory::StoreMemoryTool;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
