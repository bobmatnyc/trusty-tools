//! Commit selection and diff retrieval for the diff sampler.
//!
//! Why: diff text is expensive to fetch, so it is fetched in a separate pass
//! after batch assembly — which means a run that needs only statistics can skip
//! this stage entirely.
//! What: [`sample_diffs_for_batches`] is the entry point; `query_commits_in_period`
//! reads the candidate commits, [`stratify_and_select`] picks the sample, and
//! [`truncate_diff`] applies the profile-layer length limit.
//! Test: every test in `profile::diff_sampler::tests`.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::Path;

use rusqlite::params;
use tracing::{debug, warn};

use crate::collect::git::diff::diff_for_commit;
use crate::core::db::Database;
use crate::profile::error::{ProfileError, Result};
use crate::profile::types::period::{PeriodBatch, SampledDiff};

use super::config::{DiffSamplerConfig, MAX_DIFF_CHARS};

// ─── Commit record ────────────────────────────────────────────────────────────

/// One candidate commit for sampling.
#[derive(Debug, Clone)]
pub(super) struct CommitRecord {
    pub sha: String,
    pub repository: String,
    pub message: String,
    pub category: Option<String>,
    pub effort: Option<String>,
    /// Sort key derived from `effort`: XS=1 … XL=5, unscored=0.
    pub effort_rank: u8,
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Sample representative diffs for every batch and attach them in place.
///
/// Why: a missing local checkout is routine — an org-wide database covers
/// repositories the current machine has never cloned — so a repository that
/// cannot be opened is skipped with a warning rather than failing the run.
/// What: for each batch, queries that period's commits, selects up to
/// `config.max_diffs` via [`stratify_and_select`], fetches each diff, truncates
/// it, and pushes a [`SampledDiff`] onto `batch.sampled_diffs`.
///
/// # Errors
///
/// Returns early only on a database failure. A git or diff failure for one
/// commit is logged and skipped.
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

            match fetch_truncated_diff(&repo_path, &commit.sha) {
                Ok(diff_text) => batch.sampled_diffs.push(SampledDiff {
                    sha: commit.sha.clone(),
                    repository: commit.repository.clone(),
                    message: commit.message.clone(),
                    diff_text,
                    category: commit.category.clone(),
                    effort: commit.effort.clone(),
                }),
                Err(e) => warn!(
                    sha = %commit.sha,
                    repository = %commit.repository,
                    error = %e,
                    "diff sampler: diff_for_commit failed — skipping"
                ),
            }
        }
    }
    Ok(())
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Fetch one commit's diff and apply the profile-layer truncation.
fn fetch_truncated_diff(repo_path: &Path, sha: &str) -> Result<String> {
    let diff_text = diff_for_commit(repo_path, sha)?;
    let truncated = truncate_diff(&diff_text);
    debug!(
        sha,
        diff_len = diff_text.len(),
        truncated_len = truncated.len(),
        "sampled diff"
    );
    Ok(truncated)
}

/// Read the candidate commits for `email` within `[since, until]`.
///
/// Why: no existing tga query returns this exact shape (sha, repo, message,
/// category, effort in one row), and the sampler needs all five to stratify.
/// What: joins `commits` to `authors`, then left-joins `classifications` and
/// `fact_commit_effort` so an unclassified or unscored commit still appears.
/// Test: exercised through every `sample_diffs_for_batches` test.
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
        .map_err(|e| ProfileError::Db(crate::core::TgaError::from(e)))?;

    let rows = stmt
        .query_map(params![email, since, until], |row| {
            let sha: String = row.get(0)?;
            let repository: String = row.get(1)?;
            let message: String = row.get(2)?;
            let category: Option<String> = row.get(3)?;
            let effort: Option<String> = row.get(4)?;
            Ok((sha, repository, message, category, effort))
        })
        .map_err(|e| ProfileError::Db(crate::core::TgaError::from(e)))?;

    let mut commits = Vec::new();
    for r in rows {
        let (sha, repository, message, category, effort) =
            r.map_err(|e| ProfileError::Db(crate::core::TgaError::from(e)))?;
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

/// Map an effort-size label to a sort rank; higher means a larger commit.
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

/// Categories guaranteed at least one slot when present in the period.
const PRIORITY_CATEGORIES: &[&str] = &["bugfix", "feature", "refactor"];

/// Select up to `max_diffs` commits, stratified by category.
///
/// Why: taking the newest or largest commits would let a feature-heavy sprint
/// contribute zero bugfixes to the sample, and the resulting profile would say
/// nothing about how that contributor fixes things.
/// What: fills one slot per [`PRIORITY_CATEGORIES`] entry that is present, then
/// gives the remaining slots to the highest-effort commits not already chosen.
/// Test: `diff_sampler_stratification`, `diff_sampler_falls_back_to_effort_ordering`.
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

    // Pass 2: fill the rest with the highest-effort commits left.
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
/// A truncated result carries an explicit marker so a reader — human or model —
/// never mistakes the cut for the end of the change.
///
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
