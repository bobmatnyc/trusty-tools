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

use super::chunk_backfill::Repair;
use super::chunk_health::{ChunkCheck, short_chunk_ids};
use super::deferred_embed::RetryPolicy;
use super::embedder::{SHARED_EMBEDDER, shared_embedder, shared_embedder_initialized};
use super::handle::PalaceHandle;
use crate::engine::EngineError;
use crate::engine::embed::Embedder;
use crate::engine::store::embed_ledger::{self, EmbedFailure};
use crate::engine::store::vector::AliasScan;
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
pub(crate) enum AliasAudit {
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

#[cfg_attr(not(feature = "server"), allow(dead_code))]
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
    pub(crate) fn from_scan(scan: anyhow::Result<AliasScan>) -> Self {
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
    pub(crate) fn is_clean(&self) -> bool {
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
    pub(crate) fn unnameable_keys(&self) -> Option<&[String]> {
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
    pub(crate) fn aliased_drawer_ids(&self) -> Option<&[Uuid]> {
        match self {
            Self::Measured {
                aliased_drawer_ids, ..
            } => Some(aliased_drawer_ids),
            Self::Unavailable { .. } => None,
        }
    }

    /// `(key_rows, distinct_vector_ids)`, or `None` when the audit did not run.
    pub(crate) fn counts(&self) -> Option<(usize, usize)> {
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
    pub(crate) fn unavailable_reason(&self) -> Option<&str> {
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
#[non_exhaustive] // #9278 A2b: fields stay additive for other crates.
pub struct EmbedHealth {
    pub(crate) palace_id: String,
    /// Live drawers considered (expired non-Tier-C rows are excluded — they are
    /// reclaimable and never served, so a missing vector for one is not a hole).
    pub(crate) drawer_count: usize,
    /// Entries in the vector index, orphans included.
    pub(crate) vector_count: usize,
    /// Drawer ids with no entry in the vector index. This is the authoritative
    /// answer; everything else on this struct is context for it.
    pub missing_vector_ids: Vec<Uuid>,
    /// Rows the deferred lane wrote when it gave up (#4906 ledger).
    pub(crate) recorded_failures: Vec<EmbedFailure>,
    /// Whether a shared embedder has initialised in this process.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub(crate) embedder_ready: bool,
    /// Vector-id alias audit (#5005), including whether it ran at all.
    pub(crate) alias_audit: AliasAudit,
    /// #9275: drawers with a vector whose chunk rows are fewer than (or gapped
    /// against) the chunk count their text needs. Their tail is unfindable.
    pub short_chunk_ids: Vec<Uuid>,
    /// #9275: whether the chunk-count comparison ran; `Unavailable` is never
    /// an empty `short_chunk_ids` standing in for "could not tell".
    pub chunk_check: ChunkCheck,
}

impl EmbedHealth {
    /// Whether every live drawer is vector-searchable.
    ///
    /// #5005: an aliased drawer has a vector key and still resolves to nothing,
    /// so key presence alone is not the health condition — and an alias audit
    /// that could not run is not a passing one. Healthy requires no missing
    /// drawers AND an audit that ran and came back clean. #9275: and no drawer
    /// short of chunk rows, with a chunk check that ran.
    pub fn is_healthy(&self) -> bool {
        self.missing_vector_ids.is_empty()
            && self.alias_audit.is_clean()
            && self.short_chunk_ids.is_empty()
            && self.chunk_check == ChunkCheck::Ran
    }

    /// Whether a backfill has nothing it could repair: no missing drawer, no
    /// short drawer, and a chunk check that ran (#9275).
    fn needs_no_repair(&self) -> bool {
        self.missing_vector_ids.is_empty()
            && self.short_chunk_ids.is_empty()
            && self.chunk_check == ChunkCheck::Ran
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
#[non_exhaustive] // #9278 A2b: build with `Default`, then assign.
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
#[cfg_attr(not(feature = "server"), allow(dead_code))]
#[non_exhaustive] // #9278 A2b: fields stay additive for other crates.
pub struct VectorBackfillReport {
    pub(crate) palace_id: String,
    pub(crate) dry_run: bool,
    pub(crate) drawer_count: usize,
    pub(crate) vector_count: usize,
    /// Drawers found with no vector before this run.
    pub(crate) missing: usize,
    /// Drawers this run tried to embed (0 for a dry run, `<= limit` otherwise).
    pub attempted: usize,
    /// Drawers that now have a vector because of this run.
    pub repaired: usize,
    /// Drawers this run tried and could not repair.
    pub(crate) still_failing: usize,
    /// Ids still without a vector after this run (including any skipped by
    /// `limit`), so a caller can act on the remainder.
    pub(crate) still_missing_ids: Vec<Uuid>,
    /// The alias audit for this palace (#5005). This run never repairs an
    /// aliased drawer — a re-embed alone would not, since it already has a key.
    /// A deletion-bearing workflow must require `alias_audit.is_clean()` as well
    /// as `missing == 0` (#5000 resolution item 3); an `Unavailable` audit
    /// blocks exactly as a non-empty one does.
    pub(crate) alias_audit: AliasAudit,
    /// #9275: drawers found with fewer chunk rows than their text needs.
    pub short: usize,
    /// #9275: short drawers still not repaired after this run.
    pub still_short_ids: Vec<Uuid>,
    /// #9275: whether the chunk-count comparison ran; `short` means nothing
    /// when it is `Unavailable`.
    pub(crate) chunk_check: ChunkCheck,
}

impl VectorBackfillReport {
    /// The report before any repair: counts and id lists from `health`.
    fn from_health(health: &EmbedHealth, dry_run: bool) -> Self {
        Self {
            palace_id: health.palace_id.clone(),
            dry_run,
            drawer_count: health.drawer_count,
            vector_count: health.vector_count,
            missing: health.missing_vector_ids.len(),
            attempted: 0,
            repaired: 0,
            still_failing: 0,
            still_missing_ids: health.missing_vector_ids.clone(),
            alias_audit: health.alias_audit.clone(),
            short: health.short_chunk_ids.len(),
            still_short_ids: health.short_chunk_ids.clone(),
            chunk_check: health.chunk_check.clone(),
        }
    }
}

// #9278 A2b: the alias-repair types live in `alias_repair` (500-SLOC cap).
pub(crate) use super::alias_repair::{
    AliasRepairOptions, AliasRepairOutcome, AliasRepairReport, classify_repair,
};

impl PalaceHandle {
    /// Free every drawer caught in a vector-id collision so a re-embed can
    /// repair it — the operator surface for [`UsearchStore::unalias`](crate::engine::store::vector::UsearchStore::unalias).
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
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub(crate) fn repair_aliases(&self, opts: AliasRepairOptions) -> Result<AliasRepairReport> {
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
    /// #9275: also compares each drawer's chunk rows with the count its text
    /// needs, using the shared embedder only if it has already initialised
    /// (never a cold init); otherwise the chunk check is `Unavailable`.
    /// Test: `health_reports_the_drawer_with_no_vector`,
    /// `health_reports_chunk_check_unavailable_when_no_embedder_is_ready`.
    pub fn embed_health(&self) -> EmbedHealth {
        let embedder = SHARED_EMBEDDER.get().map(|e| e.as_ref() as &dyn Embedder);
        self.embed_health_with(embedder)
    }

    /// [`Self::embed_health`] against an explicit embedder (#9275 test seam).
    /// Test: `health_flags_a_drawer_with_fewer_chunk_rows_than_expected`.
    pub(crate) fn embed_health_with(&self, embedder: Option<&dyn Embedder>) -> EmbedHealth {
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
        let with_vector: HashSet<Uuid> = live
            .iter()
            .copied()
            .filter(|id| vector_ids.contains(id))
            .collect();
        // #9278 T2: `chunk_shapes` returns `HnswStoreError`.
        let shapes = self
            .vector_store
            .chunk_shapes()
            .map_err(anyhow::Error::from);
        let (short_chunk_ids, chunk_check) =
            short_chunk_ids(embedder, &self.drawers, &with_vector, shapes);
        if let Some(reason) = chunk_check.unavailable_reason() {
            tracing::warn!(palace = %self.id, "#9275: chunk-count check did not run: {reason}");
        }
        EmbedHealth {
            palace_id: self.id.as_str().to_string(),
            drawer_count: live.len(),
            vector_count: vector_ids.len(),
            missing_vector_ids,
            recorded_failures,
            embedder_ready: shared_embedder_initialized(),
            alias_audit,
            short_chunk_ids,
            chunk_check,
        }
    }

    /// Re-embed every live drawer that has no vector, or fewer chunk rows than
    /// its text needs (#9275).
    ///
    /// Why: the repair half of #4906. Fixing the write path does not make the
    /// drawers already written without a vector findable, and #4834 cannot
    /// delete a source file until they are. #9275: a drawer embedded before
    /// chunking has a vector and is still unfindable by its tail.
    /// What: computes [`EmbedHealth`], then (unless `dry_run`) embeds each
    /// missing drawer, then each short one (`limit` caps the union, missing
    /// first): the embed runs with no lock, then `write_mutex` is held only to
    /// recheck the text and upsert (#9735). Clears a ledger row on success and
    /// refreshes it on failure. A drawer forgotten or rewritten during its
    /// embed is skipped: not attempted, not failing, no ledger row, and it
    /// stays in `still_missing_ids`/`still_short_ids`. A lock wait past
    /// `TRUSTY_WRITE_LOCK_TIMEOUT_SECS` ends the run with an error naming the
    /// palace, after repaired drawers' rows are cleared.
    /// Idempotent and safe to re-run: a repaired drawer is in neither set, so a
    /// second run does nothing. When the chunk check could not run because no
    /// embedder had initialised, it initialises one and checks again.
    ///
    /// Errors, rather than half-running, when the palace is read-only (a
    /// snapshot handle cannot write vectors — route through the daemon) or when
    /// there is known work to do and no embedder can be initialised. A dry run
    /// has neither constraint, so it still reports on a host with no model; so
    /// does a palace whose only gap is a chunk check that could not run.
    /// #9278: the embedder failure is `EngineError::Embed`; the rest maps
    /// through `EngineError::from_internal`.
    /// Test: `backfill_reembeds_a_marked_drawer`,
    /// `backfill_is_a_noop_on_a_healthy_palace`, `backfill_dry_run_repairs_nothing`,
    /// `legacy_palace_recalls_unchanged_then_reembed_adds_the_tail`,
    /// `the_embedder_and_read_paths_return_engine_error`.
    pub async fn backfill_missing_vectors(
        &self,
        opts: VectorBackfillOptions,
    ) -> std::result::Result<VectorBackfillReport, EngineError> {
        let health = self.embed_health();
        // A healthy palace short-circuits BEFORE touching the embedder, so the
        // no-op case costs nothing and works on a host with no model at all.
        if opts.dry_run || health.needs_no_repair() {
            return Ok(VectorBackfillReport::from_health(&health, opts.dry_run));
        }
        // #9275: an unrun chunk check is not known work; it never turns a
        // no-op into an error on a read-only handle or a host with no model.
        let unknown_only =
            health.missing_vector_ids.is_empty() && health.short_chunk_ids.is_empty();
        if unknown_only && self.is_read_only() {
            return Ok(VectorBackfillReport::from_health(&health, opts.dry_run));
        }
        // #9278 T3c-2: typed for callers outside the crate.
        self.refuse_read_only_backfill()
            .map_err(EngineError::from_internal)?;
        let embedder = match shared_embedder().await {
            Ok(embedder) => embedder,
            Err(e) if unknown_only => {
                tracing::warn!(palace = %self.id, "#9275: no embedder for the chunk check: {e:#}");
                return Ok(VectorBackfillReport::from_health(&health, opts.dry_run));
            }
            // `Embed` as-is; Q7 would drop a context prefix on it.
            Err(e) => return Err(e),
        };
        self.backfill_with(opts, &embedder)
            .await
            .map_err(EngineError::from_internal)
    }

    /// [`Self::backfill_missing_vectors`] against an explicit embedder.
    ///
    /// Why (#9275): the shared embedder is a process-wide cell the lib tests
    /// seed with a non-chunking mock; this is the seam a chunking test drives.
    /// What: recomputes health with `embedder`, then re-embeds the missing
    /// drawers followed by the short ones (`limit` caps the union, missing
    /// first) through `PalaceHandle::repair_one` (#9735 Q5).
    /// Test: `backfill_limit_applies_to_missing_then_short`,
    /// `backfill_lock_timeout_returns_an_error_naming_the_palace`,
    /// `backfill_lock_timeout_clears_the_repaired_rows_first`,
    /// `backfill_dry_run_counts_short_drawers_and_writes_nothing`.
    pub(crate) async fn backfill_with(
        &self,
        opts: VectorBackfillOptions,
        embedder: &std::sync::Arc<dyn Embedder + Send + Sync>,
    ) -> Result<VectorBackfillReport> {
        let health = self.embed_health_with(Some(embedder.as_ref()));
        let mut report = VectorBackfillReport::from_health(&health, opts.dry_run);
        if opts.dry_run
            || (health.missing_vector_ids.is_empty() && health.short_chunk_ids.is_empty())
        {
            return Ok(report);
        }
        self.refuse_read_only_backfill()?;

        let queue = health
            .missing_vector_ids
            .iter()
            .chain(&health.short_chunk_ids);
        let targets: Vec<Uuid> = match opts.limit {
            Some(n) => queue.copied().take(n).collect(),
            None => queue.copied().collect(),
        };

        let mut repaired: HashSet<Uuid> = HashSet::new();
        let mut locked = None;
        for id in targets {
            // Re-read the content under the lock each time: a `forget` may have
            // landed since `embed_health` ran, in which case there is nothing to
            // embed and adding a vector would only create an orphan.
            let content = {
                let drawers = self.drawers.read();
                drawers
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.content().to_string())
            };
            let Some(content) = content else {
                continue;
            };
            // #9735 Q5: embed with no lock; recheck and upsert under write_mutex.
            let wait = crate::engine::timeouts::write_lock_timeout();
            match self
                .repair_one(embedder, id, &content, &opts.retry, wait)
                .await
            {
                Repair::Stale => {} // #9735: skipped, not failed; still listed
                Repair::Locked(e) => {
                    locked = Some(e);
                    break;
                }
                Repair::Repaired(attempts) => {
                    report.attempted += 1;
                    tracing::info!(
                        palace = %self.id, drawer = %id, attempts,
                        "#4906: vector backfill repaired a drawer"
                    );
                    repaired.insert(id);
                }
                Repair::Failed(loss) => {
                    report.attempted += 1;
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
        report.still_short_ids.retain(|id| !repaired.contains(id));
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
        // #9735 Q5: a busy palace ends the run, after repaired rows are cleared.
        match locked {
            Some(e) => Err(anyhow::Error::new(e)),
            None => Ok(report),
        }
    }

    /// Refuse a vector write on a read-only snapshot handle.
    fn refuse_read_only_backfill(&self) -> Result<()> {
        if self.is_read_only() {
            anyhow::bail!(
                "palace '{}' is read-only: the HTTP daemon holds the write lock — run the \
                 vector backfill through the daemon (`palace_reembed`) or stop it first",
                self.id
            );
        }
        Ok(())
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
