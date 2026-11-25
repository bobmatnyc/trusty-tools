//! Git blame metadata for a chunk's line range.
//!
//! Wire-compatible with `trusty_search_core::blame::ChunkBlame`. Field
//! names match exactly. Optional `decay_score` is computed downstream.

use serde::{Deserialize, Serialize};

/// Git blame metadata for a single chunk's line range.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChunkBlame {
    /// Short commit hash (12 chars) or empty if not tracked.
    #[serde(default)]
    pub commit_hash: String,
    #[serde(default)]
    pub author_email: String,
    /// ISO 8601 date string, e.g. "2026-05-09".
    #[serde(default)]
    pub last_modified: String,
    /// Days between `last_modified` and the time the search daemon ran blame.
    #[serde(default)]
    pub days_since_modified: u32,
}
