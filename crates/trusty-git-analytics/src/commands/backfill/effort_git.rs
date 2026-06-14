//! Git (libgit2) path for the effort backfill.
//!
//! Why: two cases require a live git repository — `--range` (revwalk) and
//! `--notes` (writing git notes). Isolating this from the db-only path keeps
//! each path independently testable and avoids pulling git2 into tests that
//! do not need it.

use git2::{Repository, Sort};
use rusqlite::params;
use tga::core::db::Database;
use tga::core::effort::{compute_effort, effort_tshirt_from_size, FORMULA_VERSION};

use super::types::{EffortBackfillArgs, EffortRow};

/// Process a single repository for the effort backfill using libgit2 (git path).
///
/// Why: required for two cases that cannot use the db-only path —
/// (1) `--range`: revwalk is needed to interpret git range syntax such as
/// `HEAD~10..HEAD`; (2) `--notes`: writing `refs/notes/effort` requires a live
/// `Repository`.
///
/// What: opens the on-disk git repo via libgit2, walks commits (optionally
/// filtered by `--range`), computes [`compute_effort`] per diff, and returns
/// the accumulated [`EffortRow`] records alongside scored/skipped counts.
/// Does NOT call `persist_effort_rows`; the caller is responsible for persisting
/// (matching the pattern of `process_one_repo_db`).
/// Skips already-scored commits unless `--force`.
/// Supports `--limit N` and `--dry-run`.
///
/// Test: existing `tests::backfill_effort_persists_rows` and related tests
/// exercise `persist_effort_rows`; end-to-end git path tested via `--notes`
/// and `--range` integration paths.
///
/// Returns `(scored, skipped, [XS, S, M, L, XL], rows)`.
pub(super) fn process_one_repo_git(
    repo_path: &std::path::Path,
    repo_name: &str,
    db: &mut Database,
    args: &EffortBackfillArgs,
    dry_run: bool,
) -> anyhow::Result<(usize, usize, [usize; 5], Vec<EffortRow>)> {
    let repo = Repository::open(repo_path)
        .map_err(|e| anyhow::anyhow!("cannot open git repo {}: {e}", repo_path.display()))?;

    // Build the set of SHAs that already have an effort row (unless --force).
    let already_scored: std::collections::HashSet<String> = if args.force {
        std::collections::HashSet::new()
    } else {
        let conn = db.connection();
        let mut stmt = conn.prepare("SELECT sha FROM fact_commit_effort WHERE repository = ?1")?;
        let rows = stmt.query_map(params![repo_name], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        set
    };

    // Set up the revwalk.
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TIME)?;

    if let Some(ref range) = args.range {
        // Parse the range: "A..B" → push B, hide A.
        if let Some((base, tip)) = range.split_once("..") {
            let tip_oid = repo
                .revparse_single(tip.trim())
                .map_err(|e| anyhow::anyhow!("cannot resolve git ref '{tip}': {e}"))?
                .id();
            revwalk.push(tip_oid)?;
            if !base.trim().is_empty() {
                let base_oid = repo
                    .revparse_single(base.trim())
                    .map_err(|e| anyhow::anyhow!("cannot resolve git ref '{base}': {e}"))?
                    .id();
                revwalk.hide(base_oid)?;
            }
        } else {
            // Single ref — walk from there.
            let oid = repo
                .revparse_single(range.trim())
                .map_err(|e| anyhow::anyhow!("cannot resolve git ref '{range}': {e}"))?
                .id();
            revwalk.push(oid)?;
        }
    } else {
        // HEAD may not exist on an empty repo — silently skip.
        let _ = revwalk.push_head();
    }

    // Collect records for this repo.
    let mut records: Vec<EffortRow> = Vec::new();
    let mut skipped: usize = 0;
    let limit = args.limit.unwrap_or(usize::MAX);

    for oid_res in revwalk {
        if records.len() >= limit {
            break;
        }

        let oid = match oid_res {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(repo = %repo_name, error = %e, "revwalk error; stopping");
                break;
            }
        };

        let sha_str = oid.to_string();

        // Skip already-scored commits unless --force.
        if already_scored.contains(&sha_str) {
            skipped += 1;
            continue;
        }

        // Compute the diff.
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(sha = %sha_str, error = %e, "cannot find commit; skipping");
                continue;
            }
        };

        let tree = match commit.tree() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(sha = %sha_str, error = %e, "cannot get tree; skipping");
                continue;
            }
        };

        let parent_tree = if commit.parent_count() > 0 {
            match commit.parent(0).and_then(|p| p.tree()) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(sha = %sha_str, error = %e, "cannot get parent tree; skipping");
                    continue;
                }
            }
        } else {
            None
        };

        let diff = match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(sha = %sha_str, error = %e, "diff failed; skipping");
                continue;
            }
        };

        // Extract per-file stats for the effort formula.
        // We walk the diff to collect (path, insertions, deletions) tuples.
        let file_stats: std::cell::RefCell<Vec<(String, u32, u32)>> =
            std::cell::RefCell::new(Vec::new());

        let _ = diff.foreach(
            &mut |delta, _progress| {
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                file_stats.borrow_mut().push((path, 0, 0));
                true
            },
            None,
            None,
            Some(&mut |delta, _hunk, line| {
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut files = file_stats.borrow_mut();
                if let Some(entry) = files.iter_mut().find(|e| e.0 == path) {
                    match line.origin() {
                        '+' => entry.1 = entry.1.saturating_add(1),
                        '-' => entry.2 = entry.2.saturating_add(1),
                        _ => {}
                    }
                }
                true
            }),
        );

        // Extend the lifetime of the borrow by binding to a named variable.
        let stats_snapshot = file_stats.into_inner();
        let file_refs: Vec<(&str, u32, u32)> = stats_snapshot
            .iter()
            .map(|(p, ins, del)| (p.as_str(), *ins, *del))
            .collect();

        let effort = compute_effort(file_refs);
        let computed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        records.push(EffortRow {
            sha: sha_str,
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

        // Log progress every 1000 commits.
        if records.len().is_multiple_of(1000) {
            tracing::info!(
                repo = %repo_name,
                processed = records.len(),
                "effort backfill progress"
            );
        }
    }

    // Write git notes if requested (--notes).
    if args.notes && !dry_run {
        write_effort_notes(&repo, &records);
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

    Ok((records.len(), skipped, size_counts, records))
}

/// Write `Effort: <size>` git notes to `refs/notes/effort`.
///
/// Why: optional git-native visibility for effort scores — lets users run
/// `git log --show-notes=effort` to see effort annotations inline.
/// What: for each row, appends a note to `refs/notes/effort` on the commit.
/// Soft-fails per commit (notes API errors are logged but do not abort).
/// Test: exercised by the `--notes` integration path; not unit-tested since
/// it requires a real on-disk git repo and mutates git state.
fn write_effort_notes(repo: &Repository, rows: &[EffortRow]) {
    // Resolve the notes ref signature (falls back to repo config or a
    // placeholder — notes are informational only).
    let sig = match repo.signature() {
        Ok(s) => s,
        Err(_) => match git2::Signature::now("tga", "tga@localhost") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "cannot create git signature for notes; skipping");
                return;
            }
        },
    };

    for row in rows {
        let oid = match git2::Oid::from_str(&row.sha) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let note_body = format!("Effort: {}", row.size);
        if let Err(e) = repo.note(
            &sig,
            &sig,
            Some("refs/notes/effort"),
            oid,
            &note_body,
            true, // force-overwrite
        ) {
            tracing::warn!(sha = %row.sha, error = %e, "failed to write git note; skipping");
        }
    }
}
