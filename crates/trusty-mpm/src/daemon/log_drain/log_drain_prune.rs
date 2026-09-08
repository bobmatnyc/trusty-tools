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

use trusty_common::log_drain::{
    LogDestination, ObjectStoreDestination, prunable_candidates, prune_confirmed,
};

use crate::core::trusty_tools_config::ResolvedLogDrain;
use crate::session_manager::RetentionDebounce;

use super::{DrainOutcome, LogDrainDestinationStatus};

/// Stable identity of one destination/project pass, independent of its
/// position in [`ResolvedLogDrain::destinations`].
///
/// #7154: the plan is rebuilt from config every tick, so a config reorder can
/// change which destination sits at a given index between two ticks while
/// `prune_debounce`'s armed set — keyed by the OLD index — survives across
/// them. `cache_namespace` ([`LogDestination::cache_namespace`]) plus the
/// target's own `<owner>/<project>` key is what actually distinguishes two
/// passes: it is exactly the pair the manifest cache path is namespaced by
/// (see `DrainManifest::cache_path`), so two groups can share this identity
/// only when they would also share a manifest — which is precisely when
/// transferring armed state between them is safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DestinationIdentity {
    cache_namespace: String,
    target_key: String,
}

/// One prune candidate: the destination/project pass it belongs to, keyed by
/// stable identity rather than plan position, and the manifest's own identity
/// for the object within that pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PruneCandidate {
    destination_id: DestinationIdentity,
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
    // Identity, not plan position: `connections` is resolved fresh every tick
    // from THIS tick's plan, so a candidate confirmed by identity can only
    // ever be resolved back to the destination that produced it this tick.
    let mut connections: HashMap<DestinationIdentity, (usize, ObjectStoreDestination)> =
        HashMap::new();

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
        let destination_id = DestinationIdentity {
            cache_namespace: dest.cache_namespace().to_string(),
            target_key: group.target.key_prefix(),
        };
        if let Ok(files) =
            prunable_candidates(&dest, &group.target, state_dir, plan.prune_retention).await
        {
            all_candidates.extend(files.into_iter().map(|relative_file| PruneCandidate {
                destination_id: destination_id.clone(),
                relative_file,
            }));
        }
        connections.insert(destination_id, (index, dest));
    }

    let confirmed = debounce.confirm(&all_candidates);
    if confirmed.is_empty() {
        return;
    }

    let mut by_destination: HashMap<DestinationIdentity, Vec<String>> = HashMap::new();
    for candidate in confirmed {
        by_destination
            .entry(candidate.destination_id)
            .or_default()
            .push(candidate.relative_file);
    }

    for (destination_id, files) in by_destination {
        // A candidate whose identity no longer resolves in THIS tick's plan
        // (the destination was removed, or failed its connect/upload above)
        // is dropped rather than pruned against stale state.
        let Some((index, dest)) = connections.get(&destination_id) else {
            continue;
        };
        let group = &plan.destinations[*index];
        match prune_confirmed(dest, &group.target, state_dir, &files).await {
            Ok(outcome) => {
                if let Some(refusal) = &outcome.refused {
                    statuses[*index].apply_prune_refusal(refusal);
                } else {
                    statuses[*index].apply_prune_outcome(outcome.pruned, &outcome.errors);
                }
            }
            Err(e) => statuses[*index]
                .apply_prune_outcome(0, &[("<manifest>".to_string(), e.to_string())]),
        }
    }
}

#[cfg(test)]
#[path = "log_drain_prune_tests.rs"]
mod tests;
