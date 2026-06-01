//! Core sampling logic for the diff sampler.
//!
//! Why: the diff text is too expensive to fetch speculatively; fetching it in
//! a dedicated pass (after batch assembly) allows the pipeline to skip the
//! pass entirely when no LLM narrative is needed.
//! What: implements `sample_diffs_for_batches` (the public entry point),
//! `query_commits_in_period` (DB helper), `stratify_and_select` (category-
//! stratified sampling), and `truncate_diff` (length limiter).
//! Test: all diff-sampler unit tests in `diff_sampler::tests`.

use std::cmp::Reverse;
use std::collections::HashSet;

use rusqlite::params;
use tga::collect::git::diff::diff_for_commit;
use tga::core::db::Database;
use tracing::{debug, warn};

use super::config::{DiffSamplerConfig, MAX_DIFF_CHARS};
use crate::profile::error::Result;
use crate::profile::types::period::{PeriodBatch, SampledDiff};

// ─── Commit record ────────────────────────────────────────────────────────────

/// Lightweight commit record returned by the period query.
#[derive(Debug, Clone)]
pub(super) struct CommitRecord {
    pub sha: String,
    pub repository: String,
    pub message: String,
    pub category: Option<String>,
    pub effort: Option<String>,
    /// Effort sort key: XS=1, S=2, M=3, L=4, XL=5, None=0.
    pub effort_rank: u8,
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Sample representative diffs for each period batch and attach them in-place.
///
/// Why: the diff text is too expensive to fetch speculatively; fetching it in
/// a dedicated pass (after batch assembly) allows the pipeline to skip the
/// pass entirely when no LLM narrative is needed.
/// What: for each `PeriodBatch`, queries the tga DB for the author's commits in
/// that period, stratifies them by category (≥1 bugfix/feature/refactor first,
/// then fills remaining slots by descending effort size), calls
/// `diff_for_commit` for each selected commit, truncates to [`MAX_DIFF_CHARS`],
/// and appends the result to `batch.sampled_diffs`.  Repos not available
/// locally are skipped silently (warn + continue) so a missing checkout does
/// not abort the entire profile run.
///
/// # Errors
///
/// Returns early only on DB errors.  Git / diff errors for individual commits
/// are logged and skipped.
///
/// Test: see `diff_sampler::tests::*`.
pub fn sample_diffs_for_batches(
    batches: &mut [PeriodBatch],
    db: &Database,
    canonical_email: &str,
    config: &DiffSamplerConfig,
) -> Result<()> {
    for batch in batches.iter_mut() {
        let since = batch.stats.since.as_str();
        let until = batch.stats.until.as_str();

        let commits = query_commits_in_period(db, canonical_email, since, until)?;
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

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Query the commits for `email` in the period `[since, until]`.
///
/// Why: the diff sampler needs the list of commits (sha + repo + message +
/// category + effort) for a given period; no existing tga function returns
/// this exact shape, so we query it inline.
/// What: joins `commits`, `authors`, `classifications` (LEFT), and
/// `fact_commit_effort` (LEFT) to collect the needed fields.
/// Test: exercised indirectly by all `sample_diffs_for_batches` tests.
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
        .map_err(|e| crate::profile::error::ProfileError::Db(tga::core::TgaError::from(e)))?;

    let rows = stmt
        .query_map(params![email, since, until], |row| {
            let sha: String = row.get(0)?;
            let repository: String = row.get(1)?;
            let message: String = row.get(2)?;
            let category: Option<String> = row.get(3)?;
            let effort: Option<String> = row.get(4)?;
            Ok((sha, repository, message, category, effort))
        })
        .map_err(|e| crate::profile::error::ProfileError::Db(tga::core::TgaError::from(e)))?;

    let mut commits = Vec::new();
    for r in rows {
        let (sha, repository, message, category, effort) =
            r.map_err(|e| crate::profile::error::ProfileError::Db(tga::core::TgaError::from(e)))?;
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

/// Preferred commit categories for stratified sampling.
const PRIORITY_CATEGORIES: &[&str] = &["bugfix", "feature", "refactor"];

/// Select up to `max_diffs` commits using category-stratified sampling.
///
/// Why: purely random sampling would miss important categories (e.g. no
/// bugfixes sampled from a sprint heavy in features).  Stratification ensures
/// ≥1 sample from each priority category when available.
/// What: fills slots greedily — first one commit per priority category in
/// order, then fills remaining slots with the highest-effort commits not yet
/// selected.  Returns a `Vec` of up to `max_diffs` references.
/// Test: `tests::diff_sampler_stratification`.
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

    // Pass 2: fill remaining slots with highest-effort commits.
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

/// Truncate diff text to [`MAX_DIFF_CHARS`] at a UTF-8 character boundary.
///
/// Why: some diffs produced by `diff_for_commit` may be large even after tga's
/// byte cap; `MAX_DIFF_CHARS` provides a second, profile-layer limit.
/// What: if `diff_text.chars().count()` exceeds the cap, the string is cut at
/// the char boundary and a truncation marker appended.
/// Test: `tests::diff_sampler_truncates_long_diff`.
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
