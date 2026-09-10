//! Assistant-bound trusty-memory recall and independent code/OKG vector search.
//! Why: explicit memory must not fall back to a local session store (#7360).
//! What: recall is bound by the host registry; vector search retains its search-index contract.
//! Test: `tests` covers vector queries; `assistant_memory::tests` covers durable memory RPC.
mod okg_fence;
mod recall;
mod vector_search;

pub use recall::MemoryRecallTool;
pub use vector_search::VectorSearchTool;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
