//! Containment guard for a new index root (#4289).
//!
//! Why: `POST /indexes` refused only an EXACT root collision (#2336/#2519/#3993
//! — same path, or the same `(dev, ino)` entry under another spelling). A
//! candidate that sits INSIDE an existing index's root, or that ENCLOSES one,
//! was accepted, and overlapping roots are not cosmetic: #402 / P0 #2178
//! recorded a reindex hijacking an existing index and pruning its corpus, and
//! duplicate coverage doubles embedding compute, heap, and `fseventsd` load.
//! What: [`find_root_overlap`] answers "does this candidate contain, or sit
//! inside, a root some registration already owns?" over live handles and cold
//! entries alike, and [`root_overlap_response`] turns a hit into the 409 the
//! route returns. Containment is decided with the same `(dev, ino)` primitive
//! the exact-match guard uses, one path segment at a time, so a symlink alias
//! is caught and a sibling sharing a name prefix (`/srv/app2` under
//! `/srv/app`) is not.
//! Test: `create_index_refuses_a_root_inside_an_existing_index_root`,
//! `create_index_refuses_a_root_that_encloses_an_existing_index_root`,
//! `create_index_accepts_a_sibling_of_an_existing_index_root`,
//! `create_index_refuses_a_symlinked_ancestor_of_an_existing_index_root`,
//! `overlap_check_fails_closed_when_the_candidate_cannot_be_canonicalized` in
//! `tests_4289.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;

use crate::core::registry::IndexHandle;
use crate::core::IndexId;

/// How a candidate root relates to a root some index already owns (#4289).
///
/// Why: the two containment directions need distinct operator-facing wording —
/// "you are inside one" and "you would swallow one" — and the ancestor case is
/// the one a user picking a folder is least likely to notice.
/// What: the relation OF THE CANDIDATE TO the registered root. Exact identity
/// is deliberately absent: `find_root_path_collision` (#2336) already owns
/// that answer and refuses it one step earlier in the same handler.
/// Test: `create_index_refuses_a_root_inside_an_existing_index_root`,
/// `create_index_refuses_a_root_that_encloses_an_existing_index_root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootOverlap {
    /// The candidate lives below the registered root.
    InsideExistingRoot,
    /// The candidate is an ancestor of the registered root and would enclose it.
    EnclosesExistingRoot,
}

/// The registration a candidate root would overlap (#4289).
///
/// Why: a refusal that does not name the existing index leaves the caller
/// guessing which registration is in the way.
/// What: the relation plus the registered index's id and root.
/// Test: `create_index_refuses_a_root_inside_an_existing_index_root`.
#[derive(Debug, Clone)]
pub(crate) struct RootOverlapConflict {
    /// Relation of the candidate to [`Self::root_path`].
    pub(crate) overlap: RootOverlap,
    /// Id of the registration the candidate overlaps.
    pub(crate) index_id: IndexId,
    /// Root of the registration the candidate overlaps.
    pub(crate) root_path: PathBuf,
}

/// The containment check could not be performed at all (#4289).
///
/// Why: a check that could not run must never read as "no overlap, create it".
/// The candidate passed `validate_root_path` moments earlier, so a
/// canonicalization failure here means the directory was removed, unmounted,
/// or became unreadable in between — exactly the state where guessing is worst.
/// What: the path that could not be resolved and the OS reason.
/// Test: `overlap_check_fails_closed_when_the_candidate_cannot_be_canonicalized`.
#[derive(Debug)]
pub(crate) struct OverlapCheckFailed {
    /// The candidate root that could not be canonicalized.
    pub(crate) path: PathBuf,
    /// The OS-level reason, rendered for the response body.
    pub(crate) reason: String,
}

/// Does `candidate` contain, or sit inside, `existing`?
///
/// Why: comparing canonical strings with `starts_with` is wrong twice over —
/// it misses an aliased spelling of `existing`, and it matches a sibling whose
/// name merely shares a prefix. `trusty_common::index_id::identifies_same_path`
/// already answers "same tree" over `(dev, ino)` (#2336/#2519), so containment
/// is that same question asked of each ancestor.
/// What: returns `None` when the two name one tree — the exact-match guard owns
/// that case — and otherwise walks each side's strict ancestors against the
/// shared primitive, so path-segment boundaries are structural, not textual.
/// Test: `create_index_accepts_a_sibling_of_an_existing_index_root`,
/// `create_index_refuses_a_symlinked_ancestor_of_an_existing_index_root`.
pub(crate) fn classify_root_overlap(candidate: &Path, existing: &Path) -> Option<RootOverlap> {
    if trusty_common::index_id::identifies_same_path(candidate, existing) {
        return None;
    }
    if is_below(candidate, existing) {
        return Some(RootOverlap::InsideExistingRoot);
    }
    if is_below(existing, candidate) {
        return Some(RootOverlap::EnclosesExistingRoot);
    }
    None
}

/// Is `inner` a strict descendant of `outer`?
///
/// Test: `create_index_accepts_a_sibling_of_an_existing_index_root`.
fn is_below(inner: &Path, outer: &Path) -> bool {
    inner
        .ancestors()
        .skip(1)
        .any(|ancestor| trusty_common::index_id::identifies_same_path(ancestor, outer))
}

/// Find the registration `candidate` would contain or sit inside (#4289).
///
/// Why: this is the create-index-time half of the containment logic
/// `hierarchy::build_tree_entries` already computes for the tree VIEW. Until
/// now it was only displayed, never enforced, so a GUI or CLI directory pick
/// could register an index straddling one the daemon was already serving.
/// What: canonicalizes the candidate FIRST and returns `Err` when that fails,
/// so an unresolvable path is never reported as "no overlap". Then scans live
/// handles and cold entries — a parked registration owns its root just as much
/// as a resident one (#3993) — skipping `exclude_id`, and returns the first
/// overlap in registration order.
/// Test: `create_index_refuses_a_root_inside_an_existing_index_root`,
/// `create_index_refuses_a_root_that_encloses_an_existing_index_root`,
/// `overlap_check_fails_closed_when_the_candidate_cannot_be_canonicalized`.
pub(crate) fn find_root_overlap(
    handles: &[Arc<IndexHandle>],
    cold_entries: &[crate::service::persistence::PersistedIndex],
    candidate: &Path,
    exclude_id: Option<&IndexId>,
) -> Result<Option<RootOverlapConflict>, OverlapCheckFailed> {
    let candidate = std::fs::canonicalize(candidate).map_err(|e| OverlapCheckFailed {
        path: candidate.to_path_buf(),
        reason: e.to_string(),
    })?;
    let live = handles
        .iter()
        .filter(|h| exclude_id != Some(&h.id))
        .map(|h| (h.id.clone(), h.root_path.clone()));
    let cold = cold_entries.iter().filter_map(|entry| {
        let id = IndexId::new(entry.id.clone());
        (exclude_id != Some(&id)).then(|| (id, entry.root_path.clone()))
    });
    Ok(live.chain(cold).find_map(|(index_id, root_path)| {
        classify_root_overlap(&candidate, &root_path).map(|overlap| RootOverlapConflict {
            overlap,
            index_id,
            root_path,
        })
    }))
}

/// Build the `409 Conflict` a containment refusal returns (#4289).
///
/// Why: the caller cannot act on "refused" alone — it needs to know which
/// registration is in the way and where its root is, so it can attach to that
/// index instead of creating a second one over the same files.
/// What: `409 { error, overlap, existing_index_id, existing_root_path,
/// requested_root_path }`. Paths are rendered lossily because `json!` on a
/// `&Path` panics for a non-UTF-8 path (#5827).
/// Test: `create_index_refuses_a_root_inside_an_existing_index_root`.
pub(crate) fn root_overlap_response(
    conflict: &RootOverlapConflict,
    requested: &Path,
) -> (StatusCode, serde_json::Value) {
    let (relation, kind) = match conflict.overlap {
        RootOverlap::InsideExistingRoot => ("is inside", "inside_existing_root"),
        RootOverlap::EnclosesExistingRoot => ("contains", "encloses_existing_root"),
    };
    (
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": format!(
                "{:?} {} the root of index '{}' ({:?}); overlapping index roots let one \
                 reindex prune the other's corpus. Attach to '{}' instead, or pick a \
                 directory outside it",
                requested.display(),
                relation,
                conflict.index_id,
                conflict.root_path.display(),
                conflict.index_id,
            ),
            "overlap": kind,
            "existing_index_id": conflict.index_id.0,
            "existing_root_path": conflict.root_path.display().to_string(),
            "requested_root_path": requested.display().to_string(),
        }),
    )
}

/// Build the `500` an unrunnable containment check returns (#4289).
///
/// Why: the fail-open alternative — treating an unresolvable candidate as
/// non-overlapping — is the exact hazard this guard exists to close.
/// What: `500 { error, root_path, reason }`; nothing is registered.
/// Test: `overlap_check_fails_closed_when_the_candidate_cannot_be_canonicalized`.
pub(crate) fn overlap_check_failed_response(
    failure: &OverlapCheckFailed,
) -> (StatusCode, serde_json::Value) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({
            "error": format!(
                "could not check {:?} against the registered index roots: {}",
                failure.path.display(),
                failure.reason,
            ),
            "root_path": failure.path.display().to_string(),
            "reason": failure.reason,
        }),
    )
}
