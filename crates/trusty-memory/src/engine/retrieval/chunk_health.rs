//! Expected-chunk-count health check (#9275).
//!
//! Why: a drawer embedded before chunking, or whose repair stopped part way,
//! has a vector row and is still unfindable by its tail. Key presence cannot
//! see that; comparing the drawer's rows with the chunk count its text needs
//! can.
//! What: [`ChunkCheck`] says whether the comparison ran; [`short_chunk_ids`]
//! lists the drawers whose rows are not exactly chunks `0..expected`.
//! Test: `chunk_health_tests.rs`.

use super::embed_store::{ChunkPlanner, TooManyChunks, expected_chunk_count};
use crate::engine::embed::Embedder;
use crate::engine::palace::Drawer;
use crate::engine::store::vector::ChunkShape;
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
#[non_exhaustive] // #9278 A1: new variants stay additive for other crates.
pub enum ChunkCheck {
    /// The comparison ran over every live drawer with a vector.
    Ran,
    /// The comparison could not run; the reason says why.
    Unavailable(String),
}

impl ChunkCheck {
    /// The reason the check could not run, or `None` when it ran.
    pub(crate) fn unavailable_reason(&self) -> Option<&str> {
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
/// drawer whose text will not split counts as short (its repair reports why);
/// one over the chunk cap is logged once at `info` as needing a split (#9735).
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
            // #9735 PR 1b: over the chunk cap is work for the backfill split.
            Err(e) if e.downcast_ref::<TooManyChunks>().is_some() => {
                if first_sighting(id) {
                    tracing::info!(drawer = %id, "#9735: drawer needs a split: {e:#}");
                }
                short.push(id);
            }
            Err(e) => {
                // #9735 gap 13: health runs every tick; warn once per drawer.
                if first_sighting(id) {
                    tracing::warn!(drawer = %id, "#9275: drawer text will not split: {e:#}");
                } else {
                    tracing::debug!(drawer = %id, "#9275: drawer text will not split: {e:#}");
                }
                short.push(id);
            }
        }
    }
    (short, ChunkCheck::Ran)
}

fn unavailable(reason: String) -> (Vec<Uuid>, ChunkCheck) {
    (Vec::new(), ChunkCheck::Unavailable(reason))
}

/// Most drawers remembered by [`first_sighting`]; past it every call is `false`.
pub(super) const SIGHTING_CAP: usize = 4096;

/// A capped set of drawer ids already reported.
/// Test: `first_sighting_set_is_bounded`.
pub(super) struct Sightings {
    seen: HashSet<Uuid>,
    cap: usize,
}

impl Sightings {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            seen: HashSet::new(),
            cap,
        }
    }

    /// True the first time `id` is seen while the set has room; else false.
    pub(super) fn first(&mut self, id: Uuid) -> bool {
        if self.seen.len() >= self.cap {
            return false;
        }
        self.seen.insert(id)
    }
}

/// Whether this is the first unsplittable-drawer report for `id` in this
/// process.
///
/// Why (#9735 gap 13): the backfill ticker runs the health check every tick,
/// and a warning per unsplittable drawer per tick floods the log.
/// What: a process-wide [`Sightings`] capped at [`SIGHTING_CAP`]; true on the
/// first call for an id, false after, and false for any id once it is full.
/// Test: `first_sighting_is_true_once_per_drawer_per_process`,
/// `repeated_health_calls_warn_once_for_an_unsplittable_drawer`.
pub(super) fn first_sighting(id: Uuid) -> bool {
    static SEEN: std::sync::OnceLock<parking_lot::Mutex<Sightings>> = std::sync::OnceLock::new();
    SEEN.get_or_init(|| parking_lot::Mutex::new(Sightings::new(SIGHTING_CAP)))
        .lock()
        .first(id)
}
