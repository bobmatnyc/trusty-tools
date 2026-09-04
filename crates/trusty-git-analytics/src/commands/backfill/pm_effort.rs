//! `tga backfill pm-effort` — populate `fact_pm_effort` (issue #3915).
//!
//! Why: the PM-side sibling of `tga backfill effort`, and the tier above
//! `tga backfill pm-work`. Running it after a sync materializes the
//! complexity scores the cto-reports warehouse joins against, with the same
//! `--dry-run` and idempotency contract as every other backfill.
//!
//! What: reads the tickets `fact_pm_work` marked meaningful, recovers
//! reporter / description / creation date with the work tier's extractor and
//! story points / parent key with the effort tier's, scores each with
//! [`tga::core::pm_effort::score`], and upserts one `fact_pm_effort` row.
//!
//! Ordering: `pm-work` must run first. It is the gate — a database with no
//! `fact_pm_work` rows yields no effort candidates, and this command says so
//! rather than reporting a silent zero.

use chrono::{DateTime, Utc};

use tga::core::db::{
    load_effort_candidates, prune_non_meaningful_effort, summarize_effort, upsert_pm_effort,
    CheckpointMode, Database, PmEffortRow,
};
use tga::core::pm_effort::{self, EffortCounts, PmEffortInput};
use tga::core::pm_work::extract as work_extract;

/// Score every meaningful ticket's PM effort and persist the scores.
///
/// Why: see the module docs — the EFFORT tier of epic #3914, materialized so
/// downstream dashboards join against a stored score instead of re-deriving
/// one per query.
/// What: delegates to [`backfill_pm_effort_at`] with the current time as the
/// scoring instant, which is what the recency floor measures ticket age
/// against.
/// Test: `tests::backfill_pm_effort_scores_each_bucket`.
///
/// # Errors
///
/// Propagates database errors from the candidate read, the prune, or any
/// upsert.
pub(super) fn backfill_pm_effort(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    backfill_pm_effort_at(db, dry_run, Utc::now())
}

/// [`backfill_pm_effort`] with an explicit scoring instant.
///
/// Why: the recency floor is the one part of the formula that depends on
/// wall-clock time, and a test that has to seed "four days ago" relative to
/// a hidden `Utc::now()` proves less than one that states both the ticket's
/// creation date and the instant it is scored at.
/// What: identical to [`backfill_pm_effort`] except that `now` is supplied.
/// Idempotent: the upsert is keyed on `(work_item_id, work_item_source)`, so
/// a second run at the same instant rewrites the same rows and creates none.
/// `--dry-run` reports the candidate count without writing.
/// Test: `tests::a_recent_epic_is_recorded_as_deferred_not_scored_zero`,
/// `tests::backfill_pm_effort_is_idempotent`,
/// `tests::backfill_pm_effort_dry_run_writes_nothing`.
///
/// # Errors
///
/// Propagates database errors from the candidate read, the prune, or any
/// upsert.
pub(super) fn backfill_pm_effort_at(
    db: &mut Database,
    dry_run: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let candidates = load_effort_candidates(db.connection())
        .map_err(|e| anyhow::anyhow!("pm-effort read failed: {e}"))?;

    if dry_run {
        println!(
            "Dry run — would score {} meaningful work item(s) into fact_pm_effort \
             (formula_version={}). No changes written.",
            candidates.len(),
            pm_effort::FORMULA_VERSION
        );
        return Ok(());
    }

    // #3915: re-apply the meaningfulness gate before writing, so a ticket
    // that `pm-work` has since reclassified as boilerplate stops contributing.
    let pruned = prune_non_meaningful_effort(db.connection())
        .map_err(|e| anyhow::anyhow!("pm-effort prune failed: {e}"))?;

    let mut written = 0usize;
    for candidate in &candidates {
        let row = score_candidate(candidate, now);
        upsert_pm_effort(db.connection(), &row)
            .map_err(|e| anyhow::anyhow!("pm-effort upsert failed for {}: {e}", candidate.id))?;
        written += 1;
    }

    report(db, written, pruned, candidates.is_empty())?;
    Ok(())
}

/// Build one `fact_pm_effort` row from one candidate.
///
/// Why: keeps the loop above a loop. The two extractors are called once
/// each per ticket, and the scorer sees only already-extracted values.
/// What: the work tier's extractor supplies reporter, description and
/// creation date; the effort tier's supplies story points; the loader
/// already supplied the three counts. Age is whole days from creation to
/// `now`, and is `None` when the payload carried no parseable creation
/// date — in which case the recency floor cannot fire and the ticket is
/// scored on its other inputs.
/// Test: `tests::backfill_pm_effort_scores_each_bucket`,
/// `tests::a_ticket_with_no_creation_date_is_scored_not_deferred`.
fn score_candidate(
    candidate: &tga::core::db::PmEffortCandidate,
    now: DateTime<Utc>,
) -> PmEffortRow {
    let work_fields = work_extract::extract_fields(candidate.raw_json.as_deref());
    let effort_fields = pm_effort::extract::extract_fields(candidate.raw_json.as_deref());
    let age_days = work_fields
        .created
        .map(|created| (now - created).num_days());

    let counts = EffortCounts {
        epic_children: candidate.epic_children_count,
        description_words: description_word_count(work_fields.description.as_deref()),
        comments: candidate.comment_count,
        transitions: candidate.transition_count,
        story_points: effort_fields.story_points,
    };
    let score = pm_effort::score(&PmEffortInput {
        item_type: &candidate.item_type,
        age_days,
        counts,
    });

    PmEffortRow {
        work_item_id: candidate.id.clone(),
        work_item_source: candidate.source.clone(),
        pm_name: work_fields.reporter.clone(),
        week_key: work_fields.created.map(work_extract::week_key),
        age_days,
        counts,
        score,
    }
}

/// Words of description, reusing the work tier's counter so both tiers
/// measure prose the same way.
fn description_word_count(description: Option<&str>) -> u32 {
    let words = description.map_or(0, tga::core::pm_work::word_count);
    u32::try_from(words).unwrap_or(u32::MAX)
}

/// Print the run summary and flush the WAL.
fn report(db: &mut Database, written: usize, pruned: usize, empty: bool) -> anyhow::Result<()> {
    let (deferred, buckets) = summarize_effort(db.connection())
        .map_err(|e| anyhow::anyhow!("pm-effort summary failed: {e}"))?;
    println!(
        "Backfilled fact_pm_effort: {written} row(s) written (UPSERT semantics), \
         formula_version={}.",
        pm_effort::FORMULA_VERSION
    );
    if pruned > 0 {
        println!("  pruned {pruned} row(s) whose ticket is no longer meaningful");
    }
    for (bucket, count) in buckets {
        println!("  {bucket}: {count}");
    }
    println!(
        "  deferred (inside the {}-day recency floor): {deferred}",
        pm_effort::thresholds::RECENCY_FLOOR_DAYS
    );
    if empty {
        println!(
            "  no meaningful tickets found — run `tga backfill pm-work` first: \
             fact_pm_work is the gate this tier scores through."
        );
    }

    // Flush the WAL so the new rows are durable in the main DB file.
    if let Err(e) = db.wal_checkpoint(CheckpointMode::Truncate) {
        tracing::warn!(error = %e, "WAL TRUNCATE checkpoint failed after pm-effort backfill");
    }
    Ok(())
}
