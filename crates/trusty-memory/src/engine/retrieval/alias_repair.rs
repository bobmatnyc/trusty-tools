//! Alias-repair options, outcome, report and verdict for
//! `PalaceHandle::repair_aliases` (#5005).
//!
//! Why: split out of `embed_repair` so that file stays under the 500-SLOC cap
//! once #9278 A2b marked its public structs `#[non_exhaustive]`.
//! What: the crate-private types and `classify_repair`; `embed_repair`
//! re-exports them, so every existing path still resolves.
//! Test: `classify_refuses_to_call_an_unverified_write_repaired` and the
//! `repair_aliases_*` tests in `embed_repair_tests`.

use super::embed_repair::AliasAudit;
use crate::engine::store::vector::UnaliasOutcome;
use std::collections::HashSet;
use uuid::Uuid;

/// How an alias repair run should behave.
///
/// Why: mirrors [`VectorBackfillOptions`](super::VectorBackfillOptions) on
/// purpose — the operator learns one convention, and the destructive half is
/// opt-in on both surfaces. This run
/// deletes `VECTOR_KEYS` rows, so seeing the exact id list before anything
/// changes matters more here than it does for a re-embed.
/// What: `dry_run` reports the ids it would free and writes nothing.
/// Test: `repair_aliases_dry_run_names_the_group_and_changes_nothing`.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) struct AliasRepairOptions {
    pub(crate) dry_run: bool,
}

impl Default for AliasRepairOptions {
    fn default() -> Self {
        Self { dry_run: true }
    }
}

/// How an alias repair run ended.
///
/// Why (#5005): the defect being repaired was a success-shaped report over real
/// loss, so the repair must not be able to produce one. `Repaired` is reachable
/// ONLY after a post-repair audit ran and came back clean — every other ending,
/// including "the verification could not run", is its own variant. A caller
/// cannot reach a success by reading a count.
/// What: `Clean` (nothing aliased), `Planned` (dry run), `Repaired` (freed and
/// verified), `Partial` (freed, but verification still finds a problem), and
/// `Unavailable` (an audit could not run, so nothing is known).
/// Test: `repair_aliases_never_reports_success_over_a_partial_repair`,
/// `an_unavailable_or_partial_repair_is_never_a_success`.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) enum AliasRepairOutcome {
    /// The audit ran and found no collision. Nothing was written.
    Clean,
    /// Dry run. `freed_ids` is what a real run WOULD free; nothing changed.
    Planned,
    /// Freed, and the post-repair audit confirms no collision remains.
    Repaired,
    /// Something was written, but the palace is not provably clean afterwards.
    /// Never report this as success.
    Partial {
        /// Drawers a collision still covers after the repair.
        still_aliased: Vec<Uuid>,
        /// Ids the pre-repair audit named that this run did not free.
        not_freed: Vec<Uuid>,
        /// Keys freed that could not be named, so they are missing from the
        /// re-embed worklist.
        unparsed_keys: Vec<String>,
    },
    /// An audit could not run, before or after. Nothing is known about this
    /// palace's alias state; treat it as a block, never as a pass.
    Unavailable { reason: String },
}

#[cfg_attr(not(feature = "server"), allow(dead_code))]
impl AliasRepairOutcome {
    /// One word for a wire payload or a log line.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Planned => "planned",
            Self::Repaired => "repaired",
            Self::Partial { .. } => "partial",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    /// Whether the palace is provably free of aliasing after this run.
    ///
    /// `Planned` is false: a dry run repaired nothing. `Partial` and
    /// `Unavailable` are false by construction — that is the whole point.
    pub(crate) fn is_success(&self) -> bool {
        matches!(self, Self::Clean | Self::Repaired)
    }
}

/// Outcome of one alias repair run.
///
/// Why: `freed_ids` is a SET, not a count. #5005 was a count (`missing: 0`)
/// reporting all-clear over four destroyed drawers; a repair that answered
/// "3 repaired" without naming which three would be the same defect one layer
/// up, and the ids are also the operator's re-embed worklist.
/// What: the audit before, the exact ids freed, the audit after (absent on a
/// dry run, which reads nothing twice), and the outcome.
/// Test: `repair_aliases_frees_the_group_and_verifies_it`.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) struct AliasRepairReport {
    pub(crate) palace_id: String,
    pub(crate) dry_run: bool,
    /// The alias audit taken before anything was written.
    pub(crate) before: AliasAudit,
    /// Exact drawer ids freed — or, on a dry run, that would be freed. These
    /// now have no vector and need a `backfill_missing_vectors` run.
    pub(crate) freed_ids: Vec<Uuid>,
    /// Keys the pre-repair audit found in a collision group but could not name.
    /// Non-empty means `freed_ids` cannot be the whole story.
    pub(crate) unnameable_keys: Vec<String>,
    /// The verification audit. `None` on a dry run and when nothing was
    /// aliased, because neither wrote anything to verify.
    pub(crate) after: Option<AliasAudit>,
    pub(crate) outcome: AliasRepairOutcome,
}

impl AliasRepairReport {
    /// Whether the freed drawers still need a re-embed to become findable.
    ///
    /// Freeing an aliased group turns an invisible drawer into an ordinary
    /// missing one; only the backfill makes it retrievable again.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub(crate) fn reembed_required(&self) -> bool {
        !self.dry_run && !self.freed_ids.is_empty()
    }
}

/// Decide how a repair run ended, from the post-repair audit alone.
///
/// Why (#5005): this is the guard that makes a partial repair impossible to
/// report as a complete one, and it runs AFTER keys have already been deleted —
/// "wrote, then could not verify" is a worse state than "refused to write", so
/// it is the branch most worth testing. Pulling it out of `repair_aliases`
/// makes it reachable with a hand-built `after` value without a fault-injection
/// seam, and without adding any indirection to the production path: the
/// function tested IS the function called.
/// What: `Unavailable` when the verification audit could not run — that is not
/// a success, even though the write itself succeeded. Otherwise `Repaired` only
/// when nothing is still aliased, every id `expected` named was freed, and
/// every freed key could be named; anything else is `Partial` carrying all
/// three shortfalls.
/// Test: `classify_refuses_to_call_an_unverified_write_repaired`,
/// `classify_reports_partial_when_the_verification_still_finds_a_collision`,
/// and end-to-end through `repair_aliases_frees_the_group_and_verifies_it`.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) fn classify_repair(
    after: &AliasAudit,
    freed: &UnaliasOutcome,
    expected: &[Uuid],
) -> AliasRepairOutcome {
    let Some(still) = after.aliased_drawer_ids() else {
        return AliasRepairOutcome::Unavailable {
            reason: after
                .unavailable_reason()
                .unwrap_or("post-repair alias audit unavailable")
                .to_string(),
        };
    };
    let freed_set: HashSet<Uuid> = freed.freed.iter().copied().collect();
    let not_freed: Vec<Uuid> = expected
        .iter()
        .copied()
        .filter(|id| !freed_set.contains(id))
        .collect();
    let mut still_aliased = still.to_vec();
    still_aliased.sort();
    // A post-repair audit that is measured but NOT clean counts as still
    // aliased even when it can name nobody — same arithmetic-over-ids rule as
    // `is_clean`, so an all-unnameable group cannot verify as repaired.
    if still_aliased.is_empty()
        && not_freed.is_empty()
        && freed.unparsed_keys.is_empty()
        && after.is_clean()
    {
        return AliasRepairOutcome::Repaired;
    }
    AliasRepairOutcome::Partial {
        still_aliased,
        not_freed,
        unparsed_keys: freed.unparsed_keys.clone(),
    }
}
