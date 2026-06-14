//! Database-only path for the effort backfill.
//!
//! Why: `tga collect` stores per-file diff data in the `files` table, so
//! effort scores can be computed without opening the on-disk git repository.
//! Isolating this path makes it independently testable and keeps `effort.rs`
//! focused on orchestration and the git path.

use rusqlite::{params, Connection};
use tga::core::effort::{compute_effort, effort_tshirt_from_size, FORMULA_VERSION};

use super::types::{EffortBackfillArgs, EffortRow};

/// Process a single repository for the effort backfill using only the database.
///
/// Why: `tga collect` already stores `(path, insertions, deletions)` per file in
/// the `files` table for every collected commit. Reading from the database
/// avoids opening the on-disk git repo entirely, making `tga backfill effort`
/// self-sufficient on `tga.db` alone — no repository checkout required.
///
/// Commits outside the `tga collect` window are not present in the `files`
/// table and are silently skipped. Expand the collection `since`/`until`
/// window to score them.
///
/// What: queries `commits JOIN files` for the given repository, groups rows by
/// SHA, feeds each group to [`compute_effort`], and returns the accumulated
/// [`EffortRow`] records alongside the scored/skipped counts. Does NOT call
/// `persist_effort_rows`; the caller is responsible for persisting.
///
/// Returns `(scored, skipped, [XS, S, M, L, XL], rows)`.
///
/// Test: `tests::backfill_effort_db_path_*` in `tests.rs`.
pub(super) fn process_one_repo_db(
    conn: &Connection,
    repo_name: &str,
    args: &EffortBackfillArgs,
    dry_run: bool,
) -> anyhow::Result<(usize, usize, [usize; 5], Vec<EffortRow>)> {
    // Build the set of SHAs that already have an effort row (unless --force).
    let already_scored: std::collections::HashSet<String> = if args.force {
        std::collections::HashSet::new()
    } else {
        let mut stmt = conn.prepare("SELECT sha FROM fact_commit_effort WHERE repository = ?1")?;
        let rows = stmt.query_map(params![repo_name], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        set
    };

    // Count commits available in the database for this repo (for logging).
    let in_db: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT c.sha) FROM commits c WHERE c.repository = ?1",
            params![repo_name],
            |r| r.get(0),
        )
        .unwrap_or(0);

    tracing::info!(
        repo = %repo_name,
        in_db = in_db,
        already_scored = already_scored.len(),
        "effort backfill db path: starting"
    );

    // Pull all (sha, path, insertions, deletions) rows for this repo.
    // ORDER BY c.timestamp, c.sha ensures stable ordering; the sha secondary
    // sort handles ties so the grouping below is deterministic.
    let mut stmt = conn.prepare(
        "SELECT c.sha, f.path, f.insertions, f.deletions \
         FROM commits c \
         JOIN files f ON f.commit_id = c.id \
         WHERE c.repository = ?1 \
         ORDER BY c.timestamp ASC, c.sha ASC",
    )?;

    let limit = args.limit.unwrap_or(usize::MAX);
    let mut records: Vec<EffortRow> = Vec::new();
    let mut skipped: usize = 0;

    // Group consecutive rows by SHA (they arrive sorted by timestamp+sha).
    let mut current_sha: Option<String> = None;
    let mut current_files: Vec<(String, u32, u32)> = Vec::new();

    // Helper closure: flush the accumulated files for the current SHA.
    // Returns true if a record was pushed (i.e., not skipped, not over limit).
    let flush = |sha: &str,
                 files: &[(String, u32, u32)],
                 already_scored: &std::collections::HashSet<String>,
                 records: &mut Vec<EffortRow>,
                 skipped: &mut usize|
     -> bool {
        if records.len() >= limit {
            return false;
        }
        if already_scored.contains(sha) {
            *skipped += 1;
            return true; // keep iterating — may still reach the limit
        }
        if files.is_empty() {
            tracing::warn!(
                sha = %sha,
                "commit has no rows in the files table; skipping effort computation"
            );
            return true;
        }
        let file_refs: Vec<(&str, u32, u32)> =
            files.iter().map(|(p, i, d)| (p.as_str(), *i, *d)).collect();
        let effort = compute_effort(file_refs);
        let computed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        records.push(EffortRow {
            sha: sha.to_string(),
            repository: repo_name.to_string(),
            size: effort.size_label().to_string(),
            score: effort.score,
            loc: effort.loc,
            files: effort.files,
            test_loc: effort.test_loc,
            tests_factor: effort.tests_factor,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at,
            effort_tshirt: effort_tshirt_from_size(effort.size_label()),
        });
        if records.len().is_multiple_of(1000) {
            tracing::info!(
                repo = %repo_name,
                processed = records.len(),
                "effort backfill db path: progress"
            );
        }
        true
    };

    let rows = stmt.query_map(params![repo_name], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, u32>(3)?,
        ))
    })?;

    for row_res in rows {
        let (sha, path, ins, del) = row_res?;
        match &current_sha {
            None => {
                current_sha = Some(sha.clone());
                current_files.push((path, ins, del));
            }
            Some(cur) if cur == &sha => {
                current_files.push((path, ins, del));
            }
            Some(_) => {
                // New SHA — flush the previous one.
                let prev_sha = current_sha.take().expect("just checked Some");
                let should_continue = flush(
                    &prev_sha,
                    &current_files,
                    &already_scored,
                    &mut records,
                    &mut skipped,
                );
                current_files.clear();
                if !should_continue || records.len() >= limit {
                    break;
                }
                current_sha = Some(sha.clone());
                current_files.push((path, ins, del));
            }
        }
    }
    // Flush the last group.
    if let Some(last_sha) = current_sha.take() {
        if records.len() < limit {
            flush(
                &last_sha,
                &current_files,
                &already_scored,
                &mut records,
                &mut skipped,
            );
        }
    }

    let mut size_counts = [0usize; 5];
    for row in &records {
        let idx = match row.size.as_str() {
            "XS" => 0,
            "S" => 1,
            "M" => 2,
            "L" => 3,
            _ => 4, // XL
        };
        size_counts[idx] += 1;
    }

    tracing::info!(
        repo = %repo_name,
        in_db = in_db,
        scored = records.len(),
        skipped = skipped,
        dry_run = dry_run,
        "effort backfill db path: complete"
    );

    Ok((records.len(), skipped, size_counts, records))
}
