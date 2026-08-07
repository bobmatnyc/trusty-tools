//! Answering "which drawers have no vector", and repairing the ones that don't.
//!
//! Why (#4906): fixing the write path forward repairs nothing. On the live
//! `trusty-tools` palace 39 of 1,241 drawers were already durable-but-unfindable
//! before the retry lane existed, and no amount of correct future behaviour
//! makes them retrievable. Two things were missing: a way to ASK the question
//! without a full re-scan guess, and a way to ACT on the answer.
//!
//! What: [`PalaceHandle::embed_health`] set-differences the drawer table against
//! the vector index — the exact, cheap detector, since a drawer id absent from
//! the index has no vector by definition — and
//! [`PalaceHandle::backfill_missing_vectors`] re-embeds what it finds. The
//! backfill is idempotent: a second run over a repaired palace finds nothing
//! missing and does no work, and a dry run never touches the embedder at all.
//!
//! Note on the cheap `vector_count == drawer_count` check suggested by PR #4903:
//! it is the right SUMMARY (it is how the 39 were first measured, and it is
//! what the `trusty-memory doctor` check reports because the daemon already
//! serves both counts) but it is not sufficient on its own — the vector index
//! can also hold ORPHANS (vectors whose drawer was forgotten), so equal counts
//! do not prove full coverage. The set difference here is what the repair path
//! acts on; the count comparison is the alarm that sends you to it.
//!
//! Test: `health_reports_the_drawer_with_no_vector`,
//! `backfill_reembeds_a_marked_drawer`, `backfill_is_a_noop_on_a_healthy_palace`.

use super::deferred_embed::{RetryPolicy, embed_and_store};
use super::embedder::{shared_embedder, shared_embedder_initialized};
use super::handle::PalaceHandle;
use crate::memory_core::store::embed_ledger::{self, EmbedFailure};
use crate::memory_core::store::vector::{AliasScan, UnaliasOutcome};
use anyhow::{Context, Result};
use std::collections::HashSet;
use uuid::Uuid;

/// Whether the vector-id alias audit actually ran (#5005).
///
/// Why: the audit's whole job is to answer "is any drawer's vector owned by a
/// different drawer". A failed scan has NO answer, and reporting it as zeros
/// would be the exact defect this ticket exists to fix, one level up — a
/// failure branch leaving state that looks successful. So "could not tell" is a
/// state of its own, not a number: nothing can read it as clean by accident.
/// What: `Measured` carries the two counts and the aliased ids; `Unavailable`
/// carries why. [`EmbedHealth::is_healthy`] is false for `Unavailable`.
/// Test: `alias_audit_failure_is_never_reported_as_clean`.
#[derive(Debug, Clone)]
pub enum AliasAudit {
    /// The audit ran; these numbers are authoritative.
    Measured {
        /// Rows in the `VECTOR_KEYS` table.
        key_rows: usize,
        /// Distinct vector ids those rows point at. Below `key_rows` exactly
        /// when drawers share an id.
        distinct_vector_ids: usize,
        /// Drawers whose vector was overwritten by another drawer's. These have
        /// a key, so they are NOT in `missing_vector_ids` — that is precisely
        /// why the count gap missed them — but their content is embedded
        /// nowhere. May be SHORT of the real group: a key that is not a uuid
        /// names no drawer. Never read its emptiness as "no collision" — that
        /// is what `key_rows` vs `distinct_vector_ids` is for.
        aliased_drawer_ids: Vec<Uuid>,
        /// Keys in a collision group that could not be parsed into a drawer id.
        unnameable_keys: Vec<String>,
    },
    /// The audit could not run. Nothing is known about aliasing in this palace.
    Unavailable {
        /// The scan error, for the operator.
        reason: String,
    },
}

impl AliasAudit {
    /// Turn a scan result into an outcome — the ONLY way this type is built
    /// from a fallible read.
    ///
    /// Why: the mapping is where a failed scan could be laundered into a
    /// clean-looking zero, so it is a named function with its own test rather
    /// than an inline `match` arm inside `embed_health`. `embed_health` now has
    /// no error branch of its own to get wrong.
    /// What: `Ok` → `Measured`; `Err` → `Unavailable` carrying the rendered
    /// error. Never returns zeros for a failure.
    /// Test: `alias_audit_failure_is_never_reported_as_clean`.
    pub fn from_scan(scan: anyhow::Result<AliasScan>) -> Self {
        match scan {
            Ok(s) => Self::Measured {
                key_rows: s.key_rows,
                distinct_vector_ids: s.distinct_vector_ids,
                aliased_drawer_ids: s.aliased_drawer_ids,
                unnameable_keys: s.unnameable_keys,
            },
            Err(e) => Self::Unavailable {
                reason: format!("{e:#}"),
            },
        }
    }

    /// Whether the audit ran AND no drawer shares a vector id.
    ///
    /// Why: this used to test `aliased_drawer_ids.is_empty()` alone, which a
    /// collision whose keys are not uuids satisfies while the collision is
    /// still there — the id list shrinks, the table does not. `key_rows` vs
    /// `distinct_vector_ids` comes straight off `VECTOR_KEYS` and no parse can
    /// affect it, so it is the signal that cannot be fooled. Both are checked:
    /// if they ever disagree the answer is "not clean", which fails closed.
    /// `Unavailable` is false — an unread palace is not a clean one.
    /// Test: `a_collision_whose_keys_do_not_parse_is_never_clean`.
    pub fn is_clean(&self) -> bool {
        matches!(
            self,
            Self::Measured {
                key_rows,
                distinct_vector_ids,
                aliased_drawer_ids,
                unnameable_keys,
            } if aliased_drawer_ids.is_empty()
                && unnameable_keys.is_empty()
                && key_rows == distinct_vector_ids
        )
    }

    /// Keys in a collision group that name no drawer, or `None` when the audit
    /// did not run. Non-empty means [`Self::aliased_drawer_ids`] is short.
    pub fn unnameable_keys(&self) -> Option<&[String]> {
        match self {
            Self::Measured {
                unnameable_keys, ..
            } => Some(unnameable_keys),
            Self::Unavailable { .. } => None,
        }
    }

    /// Drawers caught in a collision, or `None` when the audit did not run.
    ///
    /// Why: this returned `&[]` for `Unavailable` in the first cut, and the
    /// `palace_reembed` payload then reported `aliased: 0` for a palace nobody
    /// had read — the same zero-standing-in-for-unknown this ticket exists to
    /// remove, one field short of the two beside it. An `Option` makes that
    /// unrepresentable: a caller cannot reach a length without first deciding
    /// what to do about the `None`.
    /// What: `Some(ids)` only when the audit ran. `Some(&[])` means "looked,
    /// found nothing" — the only state a zero legitimately describes.
    /// Test: `alias_audit_failure_is_never_reported_as_clean`.
    pub fn aliased_drawer_ids(&self) -> Option<&[Uuid]> {
        match self {
            Self::Measured {
                aliased_drawer_ids, ..
            } => Some(aliased_drawer_ids),
            Self::Unavailable { .. } => None,
        }
    }

    /// `(key_rows, distinct_vector_ids)`, or `None` when the audit did not run.
    pub fn counts(&self) -> Option<(usize, usize)> {
        match self {
            Self::Measured {
                key_rows,
                distinct_vector_ids,
                ..
            } => Some((*key_rows, *distinct_vector_ids)),
            Self::Unavailable { .. } => None,
        }
    }

    /// Why the audit could not run, when it could not.
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable { reason } => Some(reason),
            Self::Measured { .. } => None,
        }
    }
}

/// Vector-coverage snapshot for one palace.
///
/// Why: the question "is this palace's memory actually findable?" had no answer
/// short of self-retrieving every drawer and guessing from the ranking, which
/// is how a 3.1 % coverage hole went unnoticed. This makes it a lookup.
/// What: the two counts, the exact set of drawer ids with no vector, and any
/// durable failure rows the deferred lane recorded. `embedder_ready` says
/// whether an embedder has initialised in this process — without it a shortfall
/// is unexplained, and "no embedder on this host" reads identically to "the
/// embedder is dropping writes".
/// Test: `health_reports_the_drawer_with_no_vector`.
#[derive(Debug, Clone)]
pub struct EmbedHealth {
    pub palace_id: String,
    /// Live drawers considered (expired non-Tier-C rows are excluded — they are
    /// reclaimable and never served, so a missing vector for one is not a hole).
    pub drawer_count: usize,
    /// Entries in the vector index, orphans included.
    pub vector_count: usize,
    /// Drawer ids with no entry in the vector index. This is the authoritative
    /// answer; everything else on this struct is context for it.
    pub missing_vector_ids: Vec<Uuid>,
    /// Rows the deferred lane wrote when it gave up (#4906 ledger).
    pub recorded_failures: Vec<EmbedFailure>,
    /// Whether a shared embedder has initialised in this process.
    pub embedder_ready: bool,
    /// Vector-id alias audit (#5005), including whether it ran at all.
    pub alias_audit: AliasAudit,
}

impl EmbedHealth {
    /// Whether every live drawer is vector-searchable.
    ///
    /// #5005: an aliased drawer has a vector key and still resolves to nothing,
    /// so key presence alone is not the health condition — and an alias audit
    /// that could not run is not a passing one. Healthy requires no missing
    /// drawers AND an audit that ran and came back clean.
    pub fn is_healthy(&self) -> bool {
        self.missing_vector_ids.is_empty() && self.alias_audit.is_clean()
    }
}

/// How a backfill run should behave.
///
/// Why: the operator's first run should be a read-only measurement — the
/// migration this unblocks (#4834) deletes source files on the strength of the
/// answer, so being able to see the number before changing anything is the
/// point.
/// What: `dry_run` reports without embedding; `limit` caps the repairs per run
/// so a huge estate can be worked through in bounded chunks; `retry` is the
/// per-drawer policy.
/// Test: `backfill_dry_run_repairs_nothing`.
#[derive(Debug, Clone, Copy)]
pub struct VectorBackfillOptions {
    pub dry_run: bool,
    pub limit: Option<usize>,
    pub retry: RetryPolicy,
}

impl Default for VectorBackfillOptions {
    fn default() -> Self {
        Self {
            dry_run: true,
            limit: None,
            retry: RetryPolicy::default(),
        }
    }
}

/// Outcome of one backfill run.
///
/// Why: the counts are the evidence an operator needs before deleting a source
/// file — "repaired 39, still_failing 0" is the statement that unblocks it.
/// What: what was found, what was attempted, and what is still broken.
/// Test: `backfill_reembeds_a_marked_drawer`.
#[derive(Debug, Clone)]
pub struct VectorBackfillReport {
    pub palace_id: String,
    pub dry_run: bool,
    pub drawer_count: usize,
    pub vector_count: usize,
    /// Drawers found with no vector before this run.
    pub missing: usize,
    /// Drawers this run tried to embed (0 for a dry run, `<= limit` otherwise).
    pub attempted: usize,
    /// Drawers that now have a vector because of this run.
    pub repaired: usize,
    /// Drawers this run tried and could not repair.
    pub still_failing: usize,
    /// Ids still without a vector after this run (including any skipped by
    /// `limit`), so a caller can act on the remainder.
    pub still_missing_ids: Vec<Uuid>,
    /// The alias audit for this palace (#5005). This run never repairs an
    /// aliased drawer — a re-embed alone would not, since it already has a key.
    /// A deletion-bearing workflow must require `alias_audit.is_clean()` as well
    /// as `missing == 0` (#5000 resolution item 3); an `Unavailable` audit
    /// blocks exactly as a non-empty one does.
    pub alias_audit: AliasAudit,
}

/// How an alias repair run should behave.
///
/// Why: mirrors [`VectorBackfillOptions`] on purpose — the operator learns one
/// convention, and the destructive half is opt-in on both surfaces. This run
/// deletes `VECTOR_KEYS` rows, so seeing the exact id list before anything
/// changes matters more here than it does for a re-embed.
/// What: `dry_run` reports the ids it would free and writes nothing.
/// Test: `repair_aliases_dry_run_names_the_group_and_changes_nothing`.
#[derive(Debug, Clone, Copy)]
pub struct AliasRepairOptions {
    pub dry_run: bool,
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
pub enum AliasRepairOutcome {
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

impl AliasRepairOutcome {
    /// One word for a wire payload or a log line.
    pub fn as_str(&self) -> &'static str {
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
    pub fn is_success(&self) -> bool {
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
pub struct AliasRepairReport {
    pub palace_id: String,
    pub dry_run: bool,
    /// The alias audit taken before anything was written.
    pub before: AliasAudit,
    /// Exact drawer ids freed — or, on a dry run, that would be freed. These
    /// now have no vector and need a `backfill_missing_vectors` run.
    pub freed_ids: Vec<Uuid>,
    /// Keys the pre-repair audit found in a collision group but could not name.
    /// Non-empty means `freed_ids` cannot be the whole story.
    pub unnameable_keys: Vec<String>,
    /// The verification audit. `None` on a dry run and when nothing was
    /// aliased, because neither wrote anything to verify.
    pub after: Option<AliasAudit>,
    pub outcome: AliasRepairOutcome,
}

impl AliasRepairReport {
    /// Whether the freed drawers still need a re-embed to become findable.
    ///
    /// Freeing an aliased group turns an invisible drawer into an ordinary
    /// missing one; only the backfill makes it retrievable again.
    pub fn reembed_required(&self) -> bool {
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

impl PalaceHandle {
    /// Free every drawer caught in a vector-id collision so a re-embed can
    /// repair it — the operator surface for [`UsearchStore::unalias`].
    ///
    /// Why (#5005): stopping new aliasing does not repair the drawers already
    /// destroyed by it, and those are what block #4834. `unalias` existed but
    /// had no caller, so an operator had no way to run the repair. This adds
    /// the three things a destructive repair owes: a dry run that names what it
    /// would touch, a result that names ids rather than counting them, and a
    /// verification pass that makes a partial repair impossible to mistake for
    /// a complete one.
    /// What: audits, and refuses to write on an unreadable audit. A dry run
    /// (the default) returns the ids and stops. A real run frees the whole
    /// group, then re-audits: `Repaired` requires that second audit to have run
    /// AND come back clean AND account for every id the first one named.
    /// Idempotent — the second run finds no group and reports `Clean`.
    ///
    /// The freed drawers are left needing a re-embed on purpose: this call must
    /// not depend on an embedder, so it stays runnable on a host with no model
    /// and the two halves fail independently. Follow it with
    /// [`PalaceHandle::backfill_missing_vectors`].
    /// Test: `repair_aliases_frees_the_group_and_verifies_it`,
    /// `repair_aliases_dry_run_names_the_group_and_changes_nothing`,
    /// `repair_aliases_never_reports_success_over_a_partial_repair`,
    /// `an_unavailable_or_partial_repair_is_never_a_success`,
    /// `repair_aliases_then_reembed_makes_a_lost_drawer_retrievable`.
    pub fn repair_aliases(&self, opts: AliasRepairOptions) -> Result<AliasRepairReport> {
        let before = AliasAudit::from_scan(self.vector_store.alias_audit());
        let mut report = AliasRepairReport {
            palace_id: self.id.as_str().to_string(),
            dry_run: opts.dry_run,
            before: before.clone(),
            freed_ids: Vec::new(),
            unnameable_keys: before.unnameable_keys().unwrap_or_default().to_vec(),
            after: None,
            outcome: AliasRepairOutcome::Clean,
        };

        // An unreadable audit is not an empty one. Writing here would be
        // deleting vector keys with no idea which, or whether any, are aliased.
        let Some(aliased) = before.aliased_drawer_ids() else {
            let reason = before
                .unavailable_reason()
                .unwrap_or("alias audit unavailable")
                .to_string();
            tracing::error!(
                palace = %self.id,
                "#5005: refusing to repair aliases — the audit could not run: {reason}"
            );
            report.outcome = AliasRepairOutcome::Unavailable { reason };
            return Ok(report);
        };

        let mut expected: Vec<Uuid> = aliased.to_vec();
        expected.sort();
        // Gate on the audit, NOT on `expected.is_empty()`. A collision whose
        // keys are not uuids names no drawer, so the id list is empty while the
        // collision is still in the table — returning `Clean` there reported a
        // real collision as repaired. `is_clean()` consults the row-vs-distinct
        // arithmetic, which no parse can shrink, so this now falls through to
        // `unalias` and ends as `Partial`: the group IS freed, and the worklist
        // genuinely cannot be named.
        if before.is_clean() {
            return Ok(report);
        }

        if opts.dry_run {
            report.freed_ids = expected;
            report.outcome = AliasRepairOutcome::Planned;
            return Ok(report);
        }

        if self.is_read_only() {
            anyhow::bail!(
                "palace '{}' is read-only: the HTTP daemon holds the write lock — run the \
                 alias repair through the daemon (`palace_unalias`) or stop it first",
                self.id
            );
        }

        let freed = self.vector_store.unalias().context("repair_aliases")?;
        report.freed_ids = freed.freed.clone();
        report.freed_ids.sort();

        let after = AliasAudit::from_scan(self.vector_store.alias_audit());
        report.after = Some(after.clone());
        report.outcome = classify_repair(&after, &freed, &expected);
        match &report.outcome {
            AliasRepairOutcome::Repaired => tracing::warn!(
                palace = %self.id, freed = report.freed_ids.len(),
                "#5005: alias repair freed every aliased drawer and verified the palace \
                 clean; those drawers now need a re-embed"
            ),
            other => tracing::error!(
                palace = %self.id, freed = report.freed_ids.len(), outcome = other.as_str(),
                "#5005: alias repair is INCOMPLETE — do not treat this palace as repaired"
            ),
        }
        Ok(report)
    }

    /// Vector-coverage snapshot for this palace.
    ///
    /// Why: see the module docs — this is the queryable form of "which drawers
    /// have no vector", replacing a self-retrieval guess with a set difference.
    /// What: reads the in-memory drawer table (the complete mirror of the redb
    /// DRAWERS table, hydrated at open) and the vector index's id set. Excludes
    /// expired non-Tier-C drawers, which are never served regardless. Cheap: no
    /// embedding, no vector search, no redb write.
    /// Test: `health_reports_the_drawer_with_no_vector`.
    pub fn embed_health(&self) -> EmbedHealth {
        let vector_ids: HashSet<Uuid> = self.vector_store.all_ids().into_iter().collect();
        let now = chrono::Utc::now();
        let live: Vec<Uuid> = self
            .drawers
            .read()
            .iter()
            .filter(|d| !d.is_expired_at(now) || d.is_tier_c())
            .map(|d| d.id)
            .collect();
        let missing_vector_ids: Vec<Uuid> = live
            .iter()
            .copied()
            .filter(|id| !vector_ids.contains(id))
            .collect();
        let recorded_failures = self
            .data_dir
            .as_ref()
            .map(|d| embed_ledger::load(d))
            .unwrap_or_default();
        // #5005: an aliased drawer has a key, so it is invisible to the set
        // difference above. A failed scan becomes `Unavailable`, never zeros —
        // a numeric zero standing in for "I could not tell" is the same
        // false-all-clear shape this ticket exists to remove.
        let alias_audit = AliasAudit::from_scan(self.vector_store.alias_audit());
        if let Some(reason) = alias_audit.unavailable_reason() {
            tracing::error!(palace = %self.id, "#5005: alias audit failed: {reason}");
        }
        EmbedHealth {
            palace_id: self.id.as_str().to_string(),
            drawer_count: live.len(),
            vector_count: vector_ids.len(),
            missing_vector_ids,
            recorded_failures,
            embedder_ready: shared_embedder_initialized(),
            alias_audit,
        }
    }

    /// Re-embed every live drawer that has no vector.
    ///
    /// Why: the repair half of #4906. Fixing the write path does not make the
    /// drawers already written without a vector findable, and #4834 cannot
    /// delete a source file until they are.
    /// What: computes [`EmbedHealth`], then (unless `dry_run`) embeds each
    /// missing drawer through the same `embed_and_store` primitive the write
    /// path uses, clearing its ledger row on success and refreshing it on
    /// failure. Idempotent and safe to re-run: a repaired drawer is no longer in
    /// the missing set, so a second run does nothing.
    ///
    /// Errors, rather than half-running, when the palace is read-only (a
    /// snapshot handle cannot write vectors — route through the daemon) or when
    /// there is work to do and no embedder can be initialised. A dry run has
    /// neither constraint, so it still reports on a host with no model.
    /// Test: `backfill_reembeds_a_marked_drawer`,
    /// `backfill_is_a_noop_on_a_healthy_palace`, `backfill_dry_run_repairs_nothing`.
    pub async fn backfill_missing_vectors(
        &self,
        opts: VectorBackfillOptions,
    ) -> Result<VectorBackfillReport> {
        let health = self.embed_health();
        let mut report = VectorBackfillReport {
            palace_id: health.palace_id.clone(),
            dry_run: opts.dry_run,
            drawer_count: health.drawer_count,
            vector_count: health.vector_count,
            missing: health.missing_vector_ids.len(),
            attempted: 0,
            repaired: 0,
            still_failing: 0,
            still_missing_ids: health.missing_vector_ids.clone(),
            alias_audit: health.alias_audit.clone(),
        };

        // A healthy palace short-circuits BEFORE touching the embedder, so the
        // no-op case costs nothing and works on a host with no model at all.
        if opts.dry_run || health.missing_vector_ids.is_empty() {
            return Ok(report);
        }

        if self.is_read_only() {
            anyhow::bail!(
                "palace '{}' is read-only: the HTTP daemon holds the write lock — run the \
                 vector backfill through the daemon (`palace_reembed`) or stop it first",
                self.id
            );
        }

        let embedder = shared_embedder()
            .await
            .context("acquire shared embedder for vector backfill")?;

        let targets: Vec<Uuid> = match opts.limit {
            Some(n) => health.missing_vector_ids.iter().copied().take(n).collect(),
            None => health.missing_vector_ids.clone(),
        };

        let mut repaired: HashSet<Uuid> = HashSet::new();
        for id in targets {
            // Re-read the content under the lock each time: a `forget` may have
            // landed since `embed_health` ran, in which case there is nothing to
            // embed and adding a vector would only create an orphan.
            let content = {
                let drawers = self.drawers.read();
                drawers
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.content.clone())
            };
            let Some(content) = content else {
                continue;
            };
            report.attempted += 1;
            match embed_and_store(&embedder, &self.vector_store, id, &content, &opts.retry).await {
                Ok(attempts) => {
                    tracing::info!(
                        palace = %self.id, drawer = %id, attempts,
                        "#4906: vector backfill repaired a drawer"
                    );
                    repaired.insert(id);
                }
                Err(loss) => {
                    report.still_failing += 1;
                    tracing::error!(
                        palace = %self.id, drawer = %id,
                        "#4906: vector backfill could not repair this drawer: {}",
                        loss.reason()
                    );
                    self.record_backfill_failure(id, &loss).await;
                }
            }
        }

        report.repaired = repaired.len();
        report.still_missing_ids.retain(|id| !repaired.contains(id));
        if let Some(data_dir) = self.data_dir.as_ref()
            && !repaired.is_empty()
        {
            // `spawn_blocking`: `json_rmw::update` blocks on an advisory flock.
            let dir = data_dir.clone();
            let cleared =
                tokio::task::spawn_blocking(move || embed_ledger::clear(&dir, &repaired)).await;
            if let Ok(Err(e)) = cleared {
                tracing::warn!(palace = %self.id, "#4906: ledger clear after backfill failed: {e:#}");
            }
        }
        Ok(report)
    }

    /// Refresh a drawer's ledger row after a failed repair attempt.
    ///
    /// Why: a backfill that tries and fails must leave the drawer marked with
    /// the fresh attempt count, or the next operator sees a stale reason.
    /// What: skips a host-level embedder outage (same rule as the write path),
    /// otherwise records on `spawn_blocking` per `json_rmw`'s contract.
    /// Test: covered through `backfill_reembeds_a_marked_drawer`'s failure
    /// counterpart in `permanent_failure_writes_a_ledger_row`.
    async fn record_backfill_failure(&self, id: Uuid, loss: &super::deferred_embed::EmbedLoss) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            return;
        };
        if !loss.is_drawer_specific() {
            return;
        }
        let entry = EmbedFailure {
            drawer_id: id,
            failed_at: chrono::Utc::now(),
            attempts: loss.attempts(),
            reason: loss.reason(),
        };
        let dir = data_dir.clone();
        let written = tokio::task::spawn_blocking(move || embed_ledger::record(&dir, entry)).await;
        if let Ok(Err(e)) = written {
            tracing::error!(palace = %self.id, drawer = %id, "#4906: ledger write failed: {e:#}");
        }
    }
}
