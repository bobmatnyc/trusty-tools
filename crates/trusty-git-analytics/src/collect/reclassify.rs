//! Re-classify stored commits when the detector generation changes (#6748).
//!
//! Why: `tga collect` runs [`detect`] once per commit and persists the verdict.
//! Nothing re-reads a stored row when the marker set gains an entry, so a
//! commit ingested before a detector fix keeps the old verdict forever. The
//! discriminator on the affected corpus was ingest date, not message content:
//! ~700 commits carrying a literal AI trailer sit at `is_ai_assisted = 0`
//! because they were walked before #1334 landed, while byte-identical messages
//! walked afterwards are correct. The downstream warehouse cannot repair them —
//! `fact_commits` has no message column, so the text exists only in `tga.db`.
//! What: [`reclassify_stale`] scans `commits` for rows whose stored
//! `ai_detector_version` is below [`DETECTOR_VERSION`], re-runs the detector
//! over the stored message and author email, and writes the verdict and the
//! current generation together. Rows already at the current generation are
//! never read, so a settled corpus costs one indexed lookup. Work proceeds in
//! batches, one transaction each, so an interrupted run leaves every completed
//! batch stamped and the remainder still selectable — the next run finishes it
//! rather than starting over.
//! Test: `tests` below.

use rusqlite::params;

use crate::collect::ai_markers::{detect, CommitSignals, Detection, DETECTOR_VERSION};
use crate::collect::errors::Result;
use crate::core::db::Database;

/// Rows re-classified per transaction.
///
/// Large enough that a hundred-thousand-row corpus is a few hundred
/// transactions, small enough that an interrupted run loses little work.
pub const RECLASSIFY_BATCH: usize = 1_000;

/// What one re-classification pass did.
///
/// Why: the operator notice needs both numbers — how much was stale, and how
/// much of it the current detector actually reads differently.
/// What: `stamped` counts rows advanced to [`DETECTOR_VERSION`]; `changed`
/// counts the subset whose `ai_tool` or `agentic_mode` moved.
/// Test: `tests::stale_rows_are_reclassified_and_current_rows_are_not`.
///
/// `#[non_exhaustive]` because a later counter — rows skipped, batches
/// committed — would otherwise be a SemVer-major break on a published crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReclassifyStats {
    /// Rows advanced to the current detector generation.
    pub stamped: usize,
    /// Rows whose stored verdict differed from what the current detector says.
    pub changed: usize,
}

/// Re-classify every commit stored by an older detector generation.
///
/// Why: the entry point `tga collect` calls before walking, so a database
/// carried across a detector change repairs itself without the operator
/// knowing a fix shipped.
/// What: loops [`reclassify_batch_with`] over [`RECLASSIFY_BATCH`]-row batches
/// until no stale row remains, using the shipped [`detect`].
/// Test: `tests::stale_rows_are_reclassified_and_current_rows_are_not`.
///
/// # Errors
///
/// Propagates database errors from the scan or the batch transactions.
pub fn reclassify_stale(db: &mut Database) -> Result<ReclassifyStats> {
    reclassify_stale_with(db, RECLASSIFY_BATCH, detect)
}

/// [`reclassify_stale`] against an explicit detector and batch size.
///
/// Why: the seam that lets a test count detector invocations, which is the only
/// way to prove a row already at the current generation is not re-processed —
/// the written row is identical either way, so no assertion on the data can
/// distinguish "skipped" from "recomputed to the same value".
/// What: repeatedly drains one batch until a batch stamps nothing. Each batch
/// commits on its own, so the loop is a resumption point, not a rollback scope.
/// Test: `tests::an_interrupted_pass_resumes_from_where_it_stopped`.
///
/// # Errors
///
/// Propagates database errors from the scan or the batch transactions.
pub fn reclassify_stale_with<F>(
    db: &mut Database,
    batch: usize,
    mut detector: F,
) -> Result<ReclassifyStats>
where
    F: FnMut(&CommitSignals<'_>) -> Detection,
{
    let batch = batch.max(1);
    let mut total = ReclassifyStats::default();
    loop {
        let pass = reclassify_batch_with(db, batch, &mut detector)?;
        if pass.stamped == 0 {
            return Ok(total);
        }
        total.stamped += pass.stamped;
        total.changed += pass.changed;
    }
}

/// Re-classify at most `batch` stale rows inside one transaction.
///
/// Why: the unit of resumability. Verdict columns and `ai_detector_version` are
/// written by the same statement in the same transaction, so a row is never
/// left carrying a new verdict with an old generation (or the reverse), and an
/// interrupt between batches loses at most the batch in flight.
/// What: selects the lowest-id rows still below [`DETECTOR_VERSION`], runs
/// `detector` over each, then writes every scanned row — changed or not — with
/// the current generation. Stamping the unchanged rows is what terminates the
/// loop; without it the same batch would be selected forever.
/// Test: `tests::an_interrupted_pass_resumes_from_where_it_stopped`.
///
/// # Errors
///
/// Propagates database errors from the scan or the transaction.
pub fn reclassify_batch_with<F>(
    db: &mut Database,
    batch: usize,
    detector: &mut F,
) -> Result<ReclassifyStats>
where
    F: FnMut(&CommitSignals<'_>) -> Detection,
{
    // (id, is_ai_assisted, ai_tool, agentic_mode, verdict_changed)
    let mut pending: Vec<(i64, i64, Option<String>, &'static str, bool)> = Vec::new();
    {
        let conn = db.connection();
        let mut stmt = conn.prepare(
            "SELECT id, message, ai_tool, agentic_mode, author_email FROM commits \
             WHERE ai_detector_version < ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows: Vec<(i64, String, Option<String>, String, String)> = stmt
            .query_map(params![DETECTOR_VERSION, batch as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    // Pre-v21 rows default to 'none'; COALESCE guards any NULLs.
                    row.get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "none".to_string()),
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;

        for (id, message, stored_tool, stored_mode, author_email) in rows {
            // #6748: `commits` has no committer_email column, so the email
            // family sees the author address only on this path — the same
            // limitation `tga backfill ai-detection-commits` carries.
            let detection = detector(&CommitSignals {
                message: &message,
                author_email: &author_email,
                committer_email: "",
            });
            let tool = detection.tool;
            let mode = detection.mode.as_str();
            let changed = tool != stored_tool.as_deref() || mode != stored_mode;
            let is_ai = i64::from(tool.is_some());
            pending.push((id, is_ai, tool.map(str::to_string), mode, changed));
        }
    }

    if pending.is_empty() {
        return Ok(ReclassifyStats::default());
    }
    let stats = ReclassifyStats {
        stamped: pending.len(),
        changed: pending.iter().filter(|(.., changed)| *changed).count(),
    };

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare(
            "UPDATE commits SET is_ai_assisted = ?1, ai_tool = ?2, agentic_mode = ?3, \
             ai_detector_version = ?4 WHERE id = ?5",
        )?;
        for (id, is_ai, tool, mode, _) in &pending {
            up.execute(params![is_ai, tool, mode, DETECTOR_VERSION, id])?;
        }
    }
    tx.commit()?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Insert one commit row the way a pre-#6748 collector wrote it.
    fn insert_commit(db: &Database, sha: &str, message: &str, detector_version: i64) {
        db.connection()
            .execute(
                "INSERT INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository, \
                  is_ai_assisted, ai_tool, agentic_mode, ai_detector_version) \
                 VALUES (?1, 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', ?2, \
                         'testrepo', 0, NULL, 'none', ?3)",
                params![sha, message, detector_version],
            )
            .expect("insert commit");
    }

    fn verdict(db: &Database, sha: &str) -> (i64, Option<String>, String, i64) {
        db.connection()
            .query_row(
                "SELECT is_ai_assisted, ai_tool, agentic_mode, ai_detector_version \
                 FROM commits WHERE sha = ?1",
                params![sha],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("read verdict")
    }

    const TRAILER: &str = "feat: a thing\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n";

    /// Why: the reported failure — a commit carrying a literal AI trailer,
    /// ingested before the detector learned to read it, stays at
    /// `is_ai_assisted = 0` forever because nothing re-runs detection over
    /// stored rows (duettoresearch/cto-reports#140).
    /// What: stores that exact row at generation 0 and asserts the pass
    /// repairs every verdict column and stamps the current generation.
    /// Test: this test itself.
    #[test]
    fn a_stale_trailer_commit_is_repaired() {
        let mut db = Database::open_in_memory().expect("open db");
        insert_commit(&db, "stale_trailer", TRAILER, 0);

        let stats = reclassify_stale(&mut db).expect("reclassify");

        assert_eq!(stats.stamped, 1);
        assert_eq!(stats.changed, 1, "the stored verdict was wrong");
        let (is_ai, tool, mode, version) = verdict(&db, "stale_trailer");
        assert_eq!(is_ai, 1);
        assert_eq!(tool.as_deref(), Some("claude"));
        assert_eq!(mode, "full_agentic");
        assert_eq!(version, DETECTOR_VERSION);
    }

    /// Why: a row already at the current generation must not be re-detected.
    /// The written row is identical either way, so only the invocation count
    /// distinguishes "skipped" from "recomputed to the same value" — and the
    /// cost this issue is about is per-row detection over hundreds of
    /// thousands of commits on every collect.
    /// What: two stale rows and one current row; asserts the detector fires
    /// exactly twice, then that a second pass fires it zero times.
    /// Test: this test itself.
    #[test]
    fn stale_rows_are_reclassified_and_current_rows_are_not() {
        let mut db = Database::open_in_memory().expect("open db");
        insert_commit(&db, "stale_a", TRAILER, 0);
        insert_commit(&db, "stale_b", "fix: human work\n", 0);
        insert_commit(&db, "current", TRAILER, DETECTOR_VERSION);

        let calls = Cell::new(0_usize);
        let stats = reclassify_stale_with(&mut db, RECLASSIFY_BATCH, |s| {
            calls.set(calls.get() + 1);
            detect(s)
        })
        .expect("reclassify");

        assert_eq!(calls.get(), 2, "only the two stale rows may be detected");
        assert_eq!(stats.stamped, 2);
        assert_eq!(stats.changed, 1, "only `stale_a` carries a marker");
        assert_eq!(
            verdict(&db, "current").0,
            0,
            "a row at the current generation is left exactly as stored"
        );

        let again = Cell::new(0_usize);
        let second = reclassify_stale_with(&mut db, RECLASSIFY_BATCH, |s| {
            again.set(again.get() + 1);
            detect(s)
        })
        .expect("reclassify twice");
        assert_eq!(again.get(), 0, "a settled corpus detects nothing");
        assert_eq!(second, ReclassifyStats::default());
    }

    /// Why: the corpus this issue names holds hundreds of thousands of
    /// commits, so an interrupted pass is the expected case, not the edge one.
    /// The requirement is that the next run finishes the remainder instead of
    /// restarting, and that no row carries a new verdict with an old
    /// generation.
    /// What: runs ONE batch of two rows out of five — the shape of an
    /// interrupt after the first transaction — then asserts the committed rows
    /// are wholly updated, three remain selectable as stale, and the resuming
    /// pass detects exactly those three.
    /// Test: this test itself.
    #[test]
    fn an_interrupted_pass_resumes_from_where_it_stopped() {
        let mut db = Database::open_in_memory().expect("open db");
        for i in 0..5 {
            insert_commit(&db, &format!("sha{i}"), TRAILER, 0);
        }

        let mut detector = detect;
        let first = reclassify_batch_with(&mut db, 2, &mut detector).expect("one batch");
        assert_eq!(first.stamped, 2, "the interrupt lands after one batch");

        for sha in ["sha0", "sha1"] {
            let (is_ai, tool, mode, version) = verdict(&db, sha);
            assert_eq!(
                (is_ai, tool.as_deref(), mode.as_str(), version),
                (1, Some("claude"), "full_agentic", DETECTOR_VERSION),
                "{sha}: verdict and generation are written together or not at all"
            );
        }
        let stale: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE ai_detector_version < ?1",
                params![DETECTOR_VERSION],
                |r| r.get(0),
            )
            .expect("count stale");
        assert_eq!(stale, 3, "the remainder is still selectable");

        let calls = Cell::new(0_usize);
        let resumed = reclassify_stale_with(&mut db, 2, |s| {
            calls.set(calls.get() + 1);
            detect(s)
        })
        .expect("resume");
        assert_eq!(calls.get(), 3, "the resuming pass redoes nothing");
        assert_eq!(resumed.stamped, 3);
    }
}
