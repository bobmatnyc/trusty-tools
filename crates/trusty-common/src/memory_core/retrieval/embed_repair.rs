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
        /// nowhere.
        aliased_drawer_ids: Vec<Uuid>,
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
    pub fn from_scan(scan: anyhow::Result<(usize, usize, Vec<Uuid>)>) -> Self {
        match scan {
            Ok((key_rows, distinct_vector_ids, aliased_drawer_ids)) => Self::Measured {
                key_rows,
                distinct_vector_ids,
                aliased_drawer_ids,
            },
            Err(e) => Self::Unavailable {
                reason: format!("{e:#}"),
            },
        }
    }

    /// Whether the audit ran AND found no drawer sharing an id.
    ///
    /// `Unavailable` is false: an unread palace is not a clean one.
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Measured { aliased_drawer_ids, .. } if aliased_drawer_ids.is_empty())
    }

    /// Drawers caught in a collision; empty when the audit did not run.
    ///
    /// Callers that branch on emptiness MUST check [`Self::is_clean`] instead —
    /// empty here means "none found OR none looked for".
    pub fn aliased_drawer_ids(&self) -> &[Uuid] {
        match self {
            Self::Measured {
                aliased_drawer_ids, ..
            } => aliased_drawer_ids,
            Self::Unavailable { .. } => &[],
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

impl PalaceHandle {
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
