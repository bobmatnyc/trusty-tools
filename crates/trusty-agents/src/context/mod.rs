//! Context + memory management subsystem (#68–#72).
//!
//! Why: Separates goal decomposition, context-window budgeting, background
//! history indexing, hybrid retrieval, and idle-phase cleanup into a single
//! top-level module so they can evolve together without polluting `agents/`
//! or `workflow/`.
//! What: Re-exports the public surface used by the workflow engine, the LLM
//! layer, and the memory_search tool.
//! Test: Per-submodule unit tests.

pub mod bm25;
pub mod cleaner;
pub mod cluster;
pub mod goals;
pub mod indexer;
pub mod manager;
pub mod retrieval;

pub use goals::GoalBlock;
#[allow(unused_imports)]
pub use indexer::{HistoryIndexer, IndexedEntry, TurnRecord};
pub use manager::ContextManager;
