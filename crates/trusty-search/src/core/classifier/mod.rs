//! Query intent classification for hybrid search routing.
//!
//! Why: the search pipeline selects different BM25/vector/KG weights depending
//! on the shape of the query; centralising the classification logic here keeps
//! the routing decision out of the search hot path.
//! What: exposes [`QueryIntent`] (the enum) and [`QueryClassifier`] (the
//! stateless classifier) as the two public items in this module.
//! Test: see `tests_intent` and `tests_identifiers` submodules for
//! comprehensive per-intent examples split by concern.

mod classify;
mod intent;
#[cfg(test)]
mod tests_identifiers;
#[cfg(test)]
mod tests_intent;

pub use self::classify::QueryClassifier;
pub use self::intent::QueryIntent;
