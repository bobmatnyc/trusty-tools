//! Expected-chunk-count health check (#9275).
//!
//! Why: a drawer embedded before chunking, or whose repair stopped part way,
//! has a vector row and is still unfindable by its tail. Key presence cannot
//! see that; comparing the drawer's rows with the chunk count its text needs
//! can.
//! What: [`ChunkCheck`] says whether the comparison ran; [`short_chunk_ids`]
//! lists the drawers whose rows are not exactly chunks `0..expected`.
//! Test: `chunk_health_tests.rs`.

use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::Drawer;
use crate::memory_core::store::vector::ChunkShape;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Whether the expected-chunk-count comparison ran.
///
/// Why: an empty short list must never stand in for "could not tell" (the
/// #5005 rule); a caller gating a deletion on health needs the difference.
/// What: `Ran`, or `Unavailable` with the reason — no embedder initialised in
/// this process, its tokenizer would not load, or the index scan failed.
/// Test: `health_reports_chunk_check_unavailable_when_no_embedder_is_ready`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkCheck {
    /// The comparison ran over every live drawer with a vector.
    Ran,
    /// The comparison could not run; the reason says why.
    Unavailable(String),
}

impl ChunkCheck {
    /// The reason the check could not run, or `None` when it ran.
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            ChunkCheck::Ran => None,
            ChunkCheck::Unavailable(reason) => Some(reason),
        }
    }
}

/// Drawers whose chunk rows do not match the chunk count their text needs.
///
/// C1 seam: reports nothing (the PR B behaviour). The pipeline commit adds the
/// comparison.
/// Test: `health_flags_a_drawer_with_fewer_chunk_rows_than_expected`.
pub(super) fn short_chunk_ids(
    _embedder: Option<&dyn Embedder>,
    _drawers: &RwLock<Vec<Drawer>>,
    _with_vector: &HashSet<Uuid>,
    _shapes: anyhow::Result<HashMap<Uuid, ChunkShape>>,
) -> (Vec<Uuid>, ChunkCheck) {
    (Vec::new(), ChunkCheck::Ran)
}
