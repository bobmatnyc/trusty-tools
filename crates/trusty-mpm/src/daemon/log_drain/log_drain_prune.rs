//! `prune_after_upload`: destination-side deletion of manifest-confirmed,
//! retention-aged objects, gated by a two-tick `RetentionDebounce` (#6536,
//! Phase 4 of #6533).
//!
//! Why: the epic's owner ruling (2026-09-01) wants a drained object removed
//! from the destination once it has aged past a retention window — but only
//! ever an object the manifest itself proves was fully uploaded. A failed or
//! partial upload never gets a [`trusty_common::log_drain::ManifestEntry`], so
//! it can never become a candidate here; there is no separate "was this
//! partial" check because the manifest already encodes exactly that
//! distinction.
//!
//! What: [`apply`] runs once per tick, after every destination's upload pass
//! has already produced its [`super::LogDrainDestinationStatus`]. It reads
//! each REACHABLE destination's manifest for candidates
//! ([`trusty_common::log_drain::prunable_candidates`]), puts the WHOLE
//! plan's candidates through one shared [`RetentionDebounce`], and only then
//! deletes what survived two consecutive ticks
//! ([`trusty_common::log_drain::prune_confirmed`]).
//!
//! # One `confirm()` call per tick, not one per destination
//!
//! [`RetentionDebounce::confirm`] REPLACES its whole armed set on every call.
//! Calling it once per destination within a single tick would make each
//! destination's call erase the arm state the previous destination's call
//! just set, silently breaking the two-tick gate the moment a plan has more
//! than one destination. So every group's candidates are collected first,
//! `confirm`ed together exactly once per tick, and only the union that
//! survives is ever deleted.
//!
//! # A destination that failed its upload pass gets no prune attempt
//!
//! Not because its existing objects are unsafe — nothing here would touch
//! them regardless — but because reading ITS manifest for new candidates
//! while it is unreachable would offer stale information to the debounce. A
//! destination that disappears from the candidate list disarms rather than
//! forcing a delete through on a guess; see [`RetentionDebounce`]'s own docs.

use std::collections::HashMap;
use std::path::Path;

use trusty_common::log_drain::{ObjectStoreDestination, prunable_candidates, prune_confirmed};

use crate::core::trusty_tools_config::ResolvedLogDrain;
use crate::session_manager::RetentionDebounce;

use super::{DrainOutcome, LogDrainDestinationStatus};

/// One prune candidate: which destination group it belongs to, and the
/// manifest's own identity for the object within that group.
///
/// `group_index` — the position in [`ResolvedLogDrain::destinations`] — is a
/// cheap, collision-free key: `resolve_log_drain` already merges any two
/// sources that would otherwise share an identity, so two different indices
/// can never mean the same destination/project pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PruneCandidate {
    group_index: usize,
    relative_file: String,
}

/// Run the prune phase for every destination in `plan`, folding each result
/// into the matching entry of `statuses` (same order as
/// `plan.destinations`). A no-op when nothing survives the debounce this
/// tick.
///
/// Test: `tests::prune_after_upload_deletes_only_on_the_second_confirming_tick`,
/// `tests::prune_after_upload_never_prunes_a_failed_upload`,
/// `tests::prune_after_upload_never_prunes_a_file_that_was_never_fully_uploaded`,
/// `tests::prune_after_upload_respects_the_off_switch`.
pub async fn apply(
    plan: &ResolvedLogDrain,
    state_dir: &Path,
    statuses: &mut [LogDrainDestinationStatus],
    debounce: &mut RetentionDebounce<PruneCandidate>,
) {
    let mut all_candidates = Vec::new();
    let mut connections: HashMap<usize, ObjectStoreDestination> = HashMap::new();

    for (index, group) in plan.destinations.iter().enumerate() {
        // See the module docs: a destination whose upload just failed gets no
        // prune attempt at all this tick.
        if statuses[index].outcome == DrainOutcome::Failed {
            continue;
        }
        let dest = match ObjectStoreDestination::connect(&group.destination).await {
            Ok(dest) => dest,
            // The upload phase just connected successfully; a failure here is
            // a genuinely transient race. Try again next tick.
            Err(_) => continue,
        };
        if let Ok(files) =
            prunable_candidates(&dest, &group.target, state_dir, plan.prune_retention).await
        {
            all_candidates.extend(files.into_iter().map(|relative_file| PruneCandidate {
                group_index: index,
                relative_file,
            }));
        }
        connections.insert(index, dest);
    }

    let confirmed = debounce.confirm(&all_candidates);
    if confirmed.is_empty() {
        return;
    }

    let mut by_group: HashMap<usize, Vec<String>> = HashMap::new();
    for candidate in confirmed {
        by_group
            .entry(candidate.group_index)
            .or_default()
            .push(candidate.relative_file);
    }

    for (index, files) in by_group {
        let Some(dest) = connections.get(&index) else {
            continue;
        };
        let group = &plan.destinations[index];
        match prune_confirmed(dest, &group.target, state_dir, &files).await {
            Ok(outcome) => statuses[index].apply_prune_outcome(outcome.pruned, &outcome.errors),
            Err(e) => {
                statuses[index].apply_prune_outcome(0, &[("<manifest>".to_string(), e.to_string())])
            }
        }
    }
}

#[cfg(test)]
#[path = "log_drain_prune_tests.rs"]
mod tests;
