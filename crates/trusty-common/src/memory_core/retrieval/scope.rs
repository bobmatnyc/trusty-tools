//! Recall scoping: turn "which rooms may this read see?" into one filter.
//!
//! Why (ADR-0027 D2): a Wing enables "recall everything the `engineer` agent
//! type has learned" as a single query, instead of requiring the caller to
//! already know that agent's complete topic set — which is the discovery
//! problem the ADR exists to fix. The drawer table stores only `room_id`, so a
//! wing has to be projected onto a set of room ids before it can filter
//! anything; doing that in one place means `retrieve_l2` and `list_drawers`
//! cannot drift on what a scope means.
//! What: [`RecallScope`] (`All` / `Room` / `Wing`), its projection onto a set
//! of room ids, and [`list_drawers_in_wing`].
//!
//! **Fail-closed, unlike the room filter.** `resolve_room_filter_id` falls back
//! to the legacy fold when a room has no registry row, because filtering "as it
//! did before ADR-0027" is the safe answer for a topic. A *wing* is a
//! scope/ownership boundary — #3064's "two agent types cannot accidentally
//! read/write the same room unless configured to do so" — so a wing that cannot
//! be resolved yields the EMPTY set, never the unfiltered one. A topic filter
//! that fails open shows extra results; a scope that fails open is a leak.
//!
//! Test: `scope_all_matches_everything`, `wing_scope_is_fail_closed`,
//! `wing_scope_returns_only_that_wings_drawers`,
//! `same_named_rooms_in_two_wings_stay_distinct`.

use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::rooms::resolve_room_filter_id;
use crate::memory_core::store::wings::rooms_in_wing;
use std::collections::HashSet;
use uuid::Uuid;

use super::handle::PalaceHandle;

/// Which rooms a read is allowed to see.
///
/// Why: `retrieve_l2` used to take `Option<RoomType>`, which cannot express
/// "this wing". Generalising the filter — rather than adding a second,
/// parallel wing parameter — keeps one implementation of the matching rule.
/// What: `All` (no filter, the pre-T9 default), `Room` (one topic), `Wing`
/// (every room that wing owns).
/// Test: `scope_all_matches_everything`, `wing_scope_is_fail_closed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallScope {
    /// No filter — byte-identical to the behaviour every caller had before
    /// wings existed.
    All,
    /// One topic, resolved through the `ROOMS` registry.
    Room(RoomType),
    /// Every room owned by this wing.
    Wing(Uuid),
}

impl RecallScope {
    /// Interpret an optional room filter as a scope.
    ///
    /// Why: lets the pre-T9 `Option<RoomType>` signatures delegate to the
    /// scoped implementation without any caller changing.
    pub fn from_room_filter(room: Option<RoomType>) -> Self {
        match room {
            Some(r) => RecallScope::Room(r),
            None => RecallScope::All,
        }
    }

    /// Project this scope onto the set of room ids a drawer may belong to.
    ///
    /// Why: this is the single definition of what a scope admits; every read
    /// path compares `drawer.room_id` against the result.
    /// What: `None` means "no filter at all" and must be distinguished from
    /// `Some(empty)`, which means "this scope admits nothing" — collapsing the
    /// two would turn an unresolvable wing into an unfiltered read.
    /// Test: `scope_all_matches_everything`, `wing_scope_is_fail_closed`.
    pub fn allowed_room_ids(&self, kg: &KnowledgeGraph) -> Option<HashSet<Uuid>> {
        match self {
            RecallScope::All => None,
            RecallScope::Room(room) => Some(HashSet::from([resolve_room_filter_id(kg, room)])),
            RecallScope::Wing(wing_id) => Some(rooms_in_wing(kg, *wing_id).unwrap_or_else(|e| {
                // Fail CLOSED — see the module doc. An unresolvable scope
                // boundary must not widen into an unfiltered read.
                tracing::warn!(%wing_id, "wing scope unresolvable, admitting nothing: {e:#}");
                HashSet::new()
            })),
        }
    }
}

/// Whether `room_id` is admitted by a resolved scope.
///
/// Why: one predicate shared by `retrieve_l2` and `list_drawers_in_wing`, so a
/// future scope variant cannot be honoured by one and ignored by the other.
pub fn scope_admits(allowed: &Option<HashSet<Uuid>>, room_id: Uuid) -> bool {
    match allowed {
        Some(ids) => ids.contains(&room_id),
        None => true,
    }
}

/// List drawers belonging to `wing_id`, sorted by importance descending.
///
/// Why: the wing-axis counterpart of `PalaceHandle::list_drawers`. It lives
/// here rather than as a method because `retrieval/handle.rs` sits at 487 of
/// its 500-SLOC cap (ADR-0027 C5) and can absorb a call site, not an
/// implementation.
/// What: resolves the wing to its room set BEFORE taking the drawer read guard
/// — that resolution is a redb read transaction, and holding the lock across
/// I/O would stall every writer on the palace for its duration — then applies
/// the same tag filter, [`drawer_listing_order`] ranking, and truncation
/// `list_drawers` uses.
/// Test: `wing_scope_returns_only_that_wings_drawers`,
/// `list_drawers_in_wing_keeps_the_newest_drawer_within_an_importance_tie`.
pub fn list_drawers_in_wing(
    handle: &PalaceHandle,
    wing_id: Uuid,
    tag: Option<String>,
    limit: usize,
) -> Vec<Drawer> {
    let allowed = RecallScope::Wing(wing_id).allowed_room_ids(&handle.kg);
    let drawers = handle.drawers.read();
    let mut filtered: Vec<Drawer> = drawers
        .iter()
        .filter(|d| scope_admits(&allowed, d.room_id))
        .filter(|d| match &tag {
            Some(t) => d.tags.iter().any(|x| x == t),
            None => true,
        })
        .cloned()
        .collect();
    drop(drawers);
    // #4836: shares `list_drawers`' comparator so the two listers cannot drift
    // on what a `limit` cuts.
    filtered.sort_by(super::types::drawer_listing_order);
    filtered.truncate(limit);
    filtered
}
