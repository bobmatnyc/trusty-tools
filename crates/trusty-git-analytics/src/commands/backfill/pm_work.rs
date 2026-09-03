//! `tga backfill pm-work` — populate `fact_pm_work` (issue #3916).
//!
//! Why: this is the PM-side sibling of `tga backfill effort`, which is the
//! only thing that (re)computes `fact_commit_effort` today. Putting the work
//! tier on the same command means one `tga backfill pm-work` after a sync
//! materializes the meaningfulness verdicts the cto-reports warehouse joins
//! against, with the same `--dry-run` and idempotency contract as every
//! other backfill.
//!
//! What: reads every `work_items` row, extracts reporter / description /
//! creation date from its preserved payload, classifies it with
//! [`tga::core::pm_work::classify`], and upserts one `fact_pm_work` row.

use tga::core::db::{
    load_candidates, summarize, upsert_pm_work, CheckpointMode, Database, PmWorkRow,
};
use tga::core::pm_work::{self, extract, PmWorkInput};

/// Classify every `work_items` row's meaningfulness and persist the verdicts.
///
/// Why: see the module docs — the WORK tier of epic #3914, materialized so
/// downstream dashboards join against a stored verdict instead of
/// re-deriving one per query.
/// What: one pass over the candidates from
/// [`tga::core::db::load_candidates`]. Idempotent: the upsert is keyed on
/// `(work_item_id, work_item_source)`, so a second run rewrites the same
/// rows and creates none. `--dry-run` reports the candidate count without
/// writing.
/// Test: `tests::backfill_pm_work_classifies_each_exclusion_reason`,
/// `tests::backfill_pm_work_is_idempotent`,
/// `tests::backfill_pm_work_dry_run_writes_nothing`.
///
/// # Errors
///
/// Propagates database errors from the candidate read or any upsert.
pub(super) fn backfill_pm_work(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    let candidates = load_candidates(db.connection())
        .map_err(|e| anyhow::anyhow!("pm-work read failed: {e}"))?;

    if dry_run {
        println!(
            "Dry run — would classify {} work item(s) into fact_pm_work \
             (formula_version={}). No changes written.",
            candidates.len(),
            pm_work::FORMULA_VERSION
        );
        return Ok(());
    }

    let mut written = 0usize;
    for candidate in &candidates {
        let fields = extract::extract_fields(candidate.raw_json.as_deref());
        let verdict = pm_work::classify(&PmWorkInput {
            title: &candidate.title,
            description: fields.description.as_deref(),
            reporter: fields.reporter.as_deref(),
            human_transitioned: candidate.human_transitioned,
        });
        let row = PmWorkRow {
            work_item_id: candidate.id.clone(),
            work_item_source: candidate.source.clone(),
            pm_name: fields.reporter.clone(),
            week_key: fields.created.map(extract::week_key),
            verdict,
        };
        upsert_pm_work(db.connection(), &row)
            .map_err(|e| anyhow::anyhow!("pm-work upsert failed for {}: {e}", candidate.id))?;
        written += 1;
    }

    let (meaningful, excluded) =
        summarize(db.connection()).map_err(|e| anyhow::anyhow!("pm-work summary failed: {e}"))?;
    println!(
        "Backfilled fact_pm_work: {written} row(s) written (UPSERT semantics), \
         formula_version={}.",
        pm_work::FORMULA_VERSION
    );
    println!("  meaningful: {meaningful}");
    for (reason, count) in excluded {
        println!("  excluded {reason}: {count}");
    }

    // Flush the WAL so the new rows are durable in the main DB file.
    if let Err(e) = db.wal_checkpoint(CheckpointMode::Truncate) {
        tracing::warn!(error = %e, "WAL TRUNCATE checkpoint failed after pm-work backfill");
    }
    Ok(())
}
