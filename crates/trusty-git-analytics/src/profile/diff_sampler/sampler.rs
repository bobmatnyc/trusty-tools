//! Sampling logic for the diff sampler.
//!
//! Why: see [`crate::profile::diff_sampler`] — this file holds the selection
//! and truncation rules that decide which commits represent a period.
//! What: [`sample_diffs_for_batches`] (the entry point), `query_commits_in_period`
//! (the DB read), [`stratify_and_select`] (category-stratified selection), and
//! [`truncate_diff`] (the length limiter).
//! Test: every test in the sibling `tests` module.

use std::cmp::Reverse;
use std::collections::HashSet;

use rusqlite::params;
use tracing::{debug, warn};

use crate::collect::git::diff::diff_for_commit;
use crate::core::db::Database;
use crate::profile::error::{ProfileError, Result};
use crate::profile::types::period::{PeriodBatch, SampledDiff};

use super::config::{DiffSamplerConfig, MAX_DIFF_CHARS};

// ─── Commit record ───────────────────────────────────────────────────────────

/// One commit row as the sampler needs it, with the effort label pre-ranked.
#[derive(Debug, Clone)]
pub(super) struct CommitRecord {
    pub sha: String,
    pub repository: String,
    pub message: String,
    pub category: Option<String>,
    pub effort: Option<String>,
    /// Effort sort key: XS=1, S=2, M=3, L=4, XL=5, unscored=0.
    pub effort_rank: u8,
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Sample representative diffs for each period batch, attaching them in place.
///
/// Why: see [`crate::profile::diff_sampler`].
/// What: for each batch, reads the author's commits in that period, selects up
/// to `config.max_diffs` of them via [`stratify_and_select`], fetches and
/// truncates each diff, and pushes the result onto `batch.sampled_diffs`.
///
/// A commit whose repository is unmapped, whose mapped path does not exist, or
/// whose diff cannot be read is logged and skipped: a missing checkout degrades
/// the profile's evidence, it does not invalidate the run.
///
/// # Errors
///
/// [`ProfileError::Db`] when the commit query fails. Per-commit git failures
/// are skipped, not propagated.
///
/// Test: `diff_sampler_fetches_real_diff`, `diff_sampler_skips_missing_repo`,
/// `diff_sampler_respects_max_diffs`.
pub fn sample_diffs_for_batches(
    batches: &mut [PeriodBatch],
    db: &Database,
    canonical_email: &str,
    config: &DiffSamplerConfig,
) -> Result<()> {
    for batch in batches.iter_mut() {
        let since = batch.stats.since.clone();
        let until = batch.stats.until.clone();

        let commits = query_commits_in_period(db, canonical_email, &since, &until)?;
        let selected = stratify_and_select(&commits, config.max_diffs);

        for commit in selected {
            let Some(repo_path) = config.repo_path(&commit.repository) else {
                warn!(
                    sha = %commit.sha,
                    repository = %commit.repository,
                    "diff sampler: repository not configured locally — skipping"
                );
                continue;
            };

            if !repo_path.exists() {
                warn!(
                    sha = %commit.sha,
                    repository = %commit.repository,
                    path = %repo_path.display(),
                    "diff sampler: repository path does not exist — skipping"
                );
                continue;
            }

            match diff_for_commit(&repo_path, &commit.sha) {
                Ok(diff_text) => {
                    let truncated = truncate_diff(&diff_text);
                    debug!(
                        sha = %commit.sha,
                        diff_len = diff_text.len(),
                        truncated_len = truncated.len(),
                        "sampled diff"
                    );
                    batch.sampled_diffs.push(SampledDiff {
                        sha: commit.sha.clone(),
                        repository: commit.repository.clone(),
                        message: commit.message.clone(),
                        diff_text: truncated,
                        category: commit.category.clone(),
                        effort: commit.effort.clone(),
                    });
                }
                Err(e) => {
                    warn!(
                        sha = %commit.sha,
                        repository = %commit.repository,
                        error = %e,
                        "diff sampler: diff_for_commit failed — skipping"
                    );
                }
            }
        }
    }
    Ok(())
}

// ─── Private helpers ─────────────────────────────────────────────────────────

/// Read the commits authored by `email` within `[since, until]`.
///
/// Why: the sampler needs sha, repository, message, category, and effort
/// together; no existing report query returns that shape.
/// What: joins `commits` to `authors`, left-joins `classifications` and
/// `fact_commit_effort`, and orders newest first. The `until` bound is widened
/// to end-of-day so a date-only bound includes that day's commits.
/// Test: exercised by every `sample_diffs_for_batches` test.
fn query_commits_in_period(
    db: &Database,
    email: &str,
    since: &str,
    until: &str,
) -> Result<Vec<CommitRecord>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT c.sha, c.repository, c.message, \
                    cl.category, fce.size \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             LEFT JOIN classifications cl ON cl.id = c.classification_id \
             LEFT JOIN fact_commit_effort fce ON fce.sha = c.sha \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND c.timestamp >= ?2 \
               AND c.timestamp <= ?3 || 'T23:59:59Z' \
             ORDER BY c.timestamp DESC",
        )
        .map_err(db_err)?;

    let rows = stmt
        .query_map(params![email, since, until], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(db_err)?;

    let mut commits = Vec::new();
    for r in rows {
        let (sha, repository, message, category, effort) = r.map_err(db_err)?;
        let effort_rank = effort_to_rank(effort.as_deref());
        commits.push(CommitRecord {
            sha,
            repository,
            message,
            category,
            effort,
            effort_rank,
        });
    }
    Ok(commits)
}

/// Wrap a rusqlite error as a [`ProfileError::Db`].
fn db_err(e: rusqlite::Error) -> ProfileError {
    ProfileError::Db(crate::core::TgaError::from(e))
}

/// Map an effort-size label to a sort rank (higher = larger commit).
fn effort_to_rank(size: Option<&str>) -> u8 {
    match size {
        Some("XS") => 1,
        Some("S") => 2,
        Some("M") => 3,
        Some("L") => 4,
        Some("XL") => 5,
        _ => 0,
    }
}

/// Categories guaranteed a slot in the sample when present.
const PRIORITY_CATEGORIES: &[&str] = &["bugfix", "feature", "refactor"];

/// Select up to `max_diffs` commits using category-stratified sampling.
///
/// Why: taking the newest or largest commits would systematically miss whole
/// categories — a sprint heavy in features would show no bugfixes at all, and
/// bugfix quality is exactly what a profile is asked about.
/// What: fills one slot per priority category first, in
/// [`PRIORITY_CATEGORIES`] order, then fills the remainder with the
/// highest-effort commits not yet chosen.
/// Test: `diff_sampler_stratification`,
/// `diff_sampler_falls_back_to_effort_ordering`.
pub(super) fn stratify_and_select(
    commits: &[CommitRecord],
    max_diffs: usize,
) -> Vec<&CommitRecord> {
    if max_diffs == 0 || commits.is_empty() {
        return Vec::new();
    }

    let mut selected: Vec<&CommitRecord> = Vec::with_capacity(max_diffs);
    let mut used_indices: HashSet<usize> = HashSet::new();

    // Pass 1: one commit per priority category.
    for cat in PRIORITY_CATEGORIES {
        if selected.len() >= max_diffs {
            break;
        }
        if let Some((idx, commit)) = commits
            .iter()
            .enumerate()
            .find(|(i, c)| !used_indices.contains(i) && c.category.as_deref() == Some(cat))
        {
            selected.push(commit);
            used_indices.insert(idx);
        }
    }

    // Pass 2: fill the remaining slots with the highest-effort commits left.
    if selected.len() < max_diffs {
        let mut remaining: Vec<(usize, &CommitRecord)> = commits
            .iter()
            .enumerate()
            .filter(|(i, _)| !used_indices.contains(i))
            .collect();
        remaining.sort_by_key(|b| Reverse(b.1.effort_rank));

        for (_, commit) in remaining {
            if selected.len() >= max_diffs {
                break;
            }
            selected.push(commit);
        }
    }

    selected
}

/// Truncate diff text to [`MAX_DIFF_CHARS`] on a UTF-8 character boundary.
///
/// Why: see [`MAX_DIFF_CHARS`]. Cutting on a byte index would panic on
/// multi-byte content, which real diffs contain.
/// What: returns the input unchanged when it is within the cap; otherwise cuts
/// at the character boundary and appends a marker so a reader knows the diff
/// continues.
/// Test: `diff_sampler_truncates_long_diff`, `diff_sampler_short_diff_unchanged`.
pub(super) fn truncate_diff(diff_text: &str) -> String {
    let char_count = diff_text.chars().count();
    if char_count <= MAX_DIFF_CHARS {
        return diff_text.to_string();
    }
    let byte_end = diff_text
        .char_indices()
        .nth(MAX_DIFF_CHARS)
        .map(|(i, _)| i)
        .unwrap_or(diff_text.len());
    format!(
        "{}\n[... diff truncated at {} chars ...]",
        &diff_text[..byte_end],
        MAX_DIFF_CHARS
    )
}
