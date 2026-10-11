//! Expected-chunk-count health check (#9275).
//!
//! Why: a drawer embedded before chunking, or whose repair stopped part way,
//! has a vector row and is still unfindable by its tail. Key presence cannot
//! see that; comparing the drawer's rows with the chunk count its text needs
//! can.
//! What: [`ChunkCheck`] says whether the comparison ran; [`short_chunk_ids`]
//! lists the drawers whose rows are not exactly chunks `0..expected`.
//! Test: `chunk_health_tests.rs`.

use super::embed_store::{ChunkPlanner, expected_chunk_count};
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
/// Why: see the module doc; this is the comparison `embed_health` runs.
/// What: `Ran` with an empty list when no live drawer has a vector. Otherwise
/// `Unavailable` when no embedder is ready, its tokenizer will not load, or
/// the index scan failed. A drawer whose text fits one chunk is checked under
/// the drawer read lock; the rest are cloned and split outside it with
/// [`expected_chunk_count`], so the check and the writers share one split. A
/// drawer whose text will not split counts as short (its repair reports why).
/// Test: `health_flags_a_drawer_with_fewer_chunk_rows_than_expected`,
/// `health_flags_a_gapped_group`,
/// `health_reports_chunk_check_unavailable_when_no_embedder_is_ready`.
pub(super) fn short_chunk_ids(
    embedder: Option<&dyn Embedder>,
    drawers: &RwLock<Vec<Drawer>>,
    with_vector: &HashSet<Uuid>,
    shapes: anyhow::Result<HashMap<Uuid, ChunkShape>>,
) -> (Vec<Uuid>, ChunkCheck) {
    if with_vector.is_empty() {
        return (Vec::new(), ChunkCheck::Ran);
    }
    let Some(embedder) = embedder else {
        return unavailable("no embedder has initialised in this process".to_string());
    };
    let planner = match ChunkPlanner::for_embedder(embedder) {
        Ok(planner) => planner,
        Err(e) => return unavailable(format!("{e:#}")),
    };
    let shapes = match shapes {
        Ok(shapes) => shapes,
        Err(e) => return unavailable(format!("scan vector keys: {e:#}")),
    };
    let shape_of = |id: &Uuid| shapes.get(id).copied().unwrap_or_default();

    let mut short = Vec::new();
    let mut long: Vec<(Uuid, String, ChunkShape)> = Vec::new();
    for drawer in drawers
        .read()
        .iter()
        .filter(|d| with_vector.contains(&d.id))
    {
        let shape = shape_of(&drawer.id);
        let fits = planner
            .as_ref()
            .is_none_or(|p| p.fits_one_chunk(drawer.content()));
        if fits {
            if !shape.is_complete(1) {
                short.push(drawer.id);
            }
        } else {
            long.push((drawer.id, drawer.content().to_string(), shape));
        }
    }
    for (id, text, shape) in long {
        match expected_chunk_count(embedder, &text) {
            Ok(expected) if shape.is_complete(expected) => {}
            Ok(_) => short.push(id),
            Err(e) => {
                tracing::warn!(drawer = %id, "#9275: drawer text will not split: {e:#}");
                short.push(id);
            }
        }
    }
    (short, ChunkCheck::Ran)
}

fn unavailable(reason: String) -> (Vec<Uuid>, ChunkCheck) {
    (Vec::new(), ChunkCheck::Unavailable(reason))
}
