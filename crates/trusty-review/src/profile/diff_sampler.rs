//! Diff sampler for contributor-profile period batches.
//!
//! Why: the LLM profile pass needs concrete diff text — not just statistics —
//! to reason about code quality trends.  This module selects a representative
//! subset of commits per period, retrieves their unified diffs via tga's
//! `diff_for_commit`, truncates to a safe length, and attaches the results to
//! each `PeriodBatch`.
//! What: provides [`sample_diffs_for_batches`] which iterates the batches in
//! place, queries the tga DB for the author's commits in each period,
//! stratifies by category (≥1 bugfix/feature/refactor if present, else by
//! descending effort), calls `diff_for_commit`, truncates to
//! [`MAX_DIFF_CHARS`], and gracefully skips commits whose repository is not
//! available locally (log + continue).
//! Test: `diff_sampler::tests` uses a temp git repo (via git2) to exercise the
//! full path; missing-repo skipping is tested with a repo path that doesn't
//! exist; truncation is tested with content larger than `MAX_DIFF_CHARS`.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::params;
use tga::collect::git::diff::diff_for_commit;
use tga::core::db::Database;
use tracing::{debug, warn};

use super::error::Result;
use super::types::{PeriodBatch, SampledDiff};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum characters of diff text kept per sampled commit.
///
/// Why: LLM context windows are finite; truncating here prevents a single
/// large commit from consuming the entire profile budget.
/// What: 20,000 UTF-8 characters (~5–10K tokens).  The tga `DIFF_BYTE_CAP`
/// (200 KiB) is a separate, lower-level limit applied by `diff_for_commit`
/// itself; this constant is a further truncation applied at the profile layer.
/// Test: `tests::diff_sampler_truncates_long_diff`.
pub const MAX_DIFF_CHARS: usize = 20_000;

/// Maximum number of diffs to sample per period batch.
///
/// Why: the default max protects against periods with many commits all being
/// fed into the LLM in one shot.
/// What: 5 — enough for qualitative coverage without excessive token usage.
/// Callers may override this via `DiffSamplerConfig::max_diffs`.
/// Test: `tests::diff_sampler_respects_max_diffs`.
pub const DEFAULT_MAX_DIFFS: usize = 5;

// ─── Config ───────────────────────────────────────────────────────────────────

/// Configuration for the diff sampler.
///
/// Why: callers need to tune sampling parameters (number of diffs, repo paths)
/// without changing function signatures.
/// What: holds the maximum number of diffs per period and the map from
/// repository name to local filesystem path used by `diff_for_commit`.
/// Test: `DiffSamplerConfig::default()` is exercised by all sampler tests.
#[derive(Debug, Clone)]
pub struct DiffSamplerConfig {
    /// Maximum number of diffs to sample per period batch.
    /// Defaults to [`DEFAULT_MAX_DIFFS`].
    pub max_diffs: usize,

    /// Map from `commits.repository` (as stored in tga) to the local path
    /// of that repository.  Used to open the repo for `diff_for_commit`.
    ///
    /// When a repository is not present in this map, or when the mapped path
    /// does not exist, the commit is skipped with a warning.
    pub repo_paths: HashMap<String, PathBuf>,

    /// Optional root directory under which repos are located.
    ///
    /// When set, repo paths are resolved as `repos_root / repository_name`.
    /// `repo_paths` takes precedence over `repos_root` for individual repos.
    pub repos_root: Option<PathBuf>,
}

impl Default for DiffSamplerConfig {
    fn default() -> Self {
        Self {
            max_diffs: DEFAULT_MAX_DIFFS,
            repo_paths: HashMap::new(),
            repos_root: None,
        }
    }
}

impl DiffSamplerConfig {
    /// Resolve the local filesystem path for a given repository name.
    ///
    /// Why: callers may configure either an explicit `repo_paths` map or a
    /// `repos_root`; this method encapsulates the resolution precedence.
    /// What: returns the explicit `repo_paths` entry if present; otherwise
    /// tries `repos_root / repo_name`; returns `None` when neither is set.
    /// Test: `tests::config_repo_path_resolution`.
    pub fn repo_path(&self, repo_name: &str) -> Option<PathBuf> {
        if let Some(p) = self.repo_paths.get(repo_name) {
            return Some(p.clone());
        }
        self.repos_root.as_ref().map(|root| root.join(repo_name))
    }
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
/// # Parameters
///
/// - `batches` — mutable slice of assembled `PeriodBatch` values.
/// - `db` — open tga database handle (read-only queries).
/// - `canonical_email` — canonical email for the contributor.
/// - `config` — sampler configuration (repo paths, max_diffs).
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

// ─── Commit record ────────────────────────────────────────────────────────────

/// Lightweight commit record returned by the period query.
#[derive(Debug, Clone)]
struct CommitRecord {
    sha: String,
    repository: String,
    message: String,
    category: Option<String>,
    effort: Option<String>,
    /// Effort sort key: XS=1, S=2, M=3, L=4, XL=5, None=0.
    effort_rank: u8,
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Query the commits for `email` in the period `[since, until]`.
///
/// Why: the diff sampler needs the list of commits (sha + repo + message +
/// category + effort) for a given period; no existing tga function returns
/// this exact shape, so we query it inline.  This is a pure read-only query
/// against the existing schema — no schema changes needed.
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
        .map_err(|e| super::error::ProfileError::Db(tga::core::TgaError::from(e)))?;

    let rows = stmt
        .query_map(params![email, since, until], |row| {
            let sha: String = row.get(0)?;
            let repository: String = row.get(1)?;
            let message: String = row.get(2)?;
            let category: Option<String> = row.get(3)?;
            let effort: Option<String> = row.get(4)?;
            Ok((sha, repository, message, category, effort))
        })
        .map_err(|e| super::error::ProfileError::Db(tga::core::TgaError::from(e)))?;

    let mut commits = Vec::new();
    for r in rows {
        let (sha, repository, message, category, effort) =
            r.map_err(|e| super::error::ProfileError::Db(tga::core::TgaError::from(e)))?;
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
fn stratify_and_select(commits: &[CommitRecord], max_diffs: usize) -> Vec<&CommitRecord> {
    if max_diffs == 0 || commits.is_empty() {
        return Vec::new();
    }

    let mut selected: Vec<&CommitRecord> = Vec::with_capacity(max_diffs);
    let mut used_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();

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
        // Collect remaining commits sorted by descending effort rank.
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
fn truncate_diff(diff_text: &str) -> String {
    let char_count = diff_text.chars().count();
    if char_count <= MAX_DIFF_CHARS {
        return diff_text.to_string();
    }
    // Find the byte index of the char at MAX_DIFF_CHARS.
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::path::Path;
    use tga::core::db::Database;

    // ── DB seed helpers ───────────────────────────────────────────────────────

    fn seed_author(db: &Database, name: &str, email: &str) -> i64 {
        db.connection()
            .execute(
                "INSERT INTO authors (canonical_name, canonical_email, aliases) \
                 VALUES (?1, ?2, '[]')",
                params![name, email],
            )
            .expect("insert author");
        db.connection().last_insert_rowid()
    }

    fn seed_commit_with_category_effort(
        db: &Database,
        sha: &str,
        author_id: i64,
        repository: &str,
        timestamp: &str,
        category: Option<&str>,
        effort: Option<&str>,
    ) {
        let cls_id: Option<i64> = if let Some(cat) = category {
            db.connection()
                .execute(
                    "INSERT OR IGNORE INTO classifications (id, category, confidence, method) \
                     VALUES (NULL, ?1, 0.9, 'rule')",
                    params![cat],
                )
                .expect("insert classification");
            let id: i64 = db
                .connection()
                .query_row(
                    "SELECT id FROM classifications WHERE category = ?1 LIMIT 1",
                    params![cat],
                    |r| r.get(0),
                )
                .expect("get cls id");
            Some(id)
        } else {
            None
        };

        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_id, author_name, author_email, \
                 timestamp, message, repository, insertions, deletions, classification_id) \
                 VALUES (?1, ?2, 'n', 'e', ?3, ?1, ?4, 5, 2, ?5)",
                params![sha, author_id, timestamp, repository, cls_id],
            )
            .expect("insert commit");

        if let Some(sz) = effort {
            db.connection()
                .execute(
                    "INSERT INTO fact_commit_effort \
                     (sha, repository, size, score, loc, files, test_loc, tests_factor, computed_at) \
                     VALUES (?1, ?2, ?3, 1.0, 10, 1, 0, 1.0, 0)",
                    params![sha, repository, sz],
                )
                .expect("insert effort");
        }
    }

    // ── Git repo helpers ──────────────────────────────────────────────────────

    fn make_repo_with_initial_commit(dir: &Path, filename: &str, content: &str) -> String {
        let repo = git2::Repository::init(dir).expect("init repo");
        let mut config = repo.config().expect("config");
        config.set_str("user.name", "Test User").expect("set name");
        config
            .set_str("user.email", "test@example.com")
            .expect("set email");

        let file_path = dir.join(filename);
        std::fs::write(&file_path, content).expect("write file");

        let mut index = repo.index().expect("index");
        index.add_path(Path::new(filename)).expect("add path");
        index.write().expect("write index");

        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("Test User", "test@example.com").expect("sig");
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .expect("initial commit");
        oid.to_string()
    }

    fn add_commit(dir: &Path, filename: &str, new_content: &str) -> String {
        let repo = git2::Repository::open(dir).expect("open repo");

        let file_path = dir.join(filename);
        std::fs::write(&file_path, new_content).expect("write file");

        let mut index = repo.index().expect("index");
        index.add_path(Path::new(filename)).expect("add path");
        index.write().expect("write index");

        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("Test User", "test@example.com").expect("sig");
        let head = repo.head().expect("head").peel_to_commit().expect("peel");
        let oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "Follow-up commit",
                &tree,
                &[&head],
            )
            .expect("follow-up commit");
        oid.to_string()
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Why: the sampler must pick at least one bugfix and one feature commit
    /// when both categories are present, rather than taking commits in pure
    /// timestamp order.
    /// What: seeds 3 feature + 2 bugfix commits in one period, samples with
    /// max_diffs=3, asserts ≥1 of each category.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_stratification() {
        let commits = vec![
            CommitRecord {
                sha: "f1".to_string(),
                repository: "r".to_string(),
                message: "feat 1".to_string(),
                category: Some("feature".to_string()),
                effort: Some("S".to_string()),
                effort_rank: 2,
            },
            CommitRecord {
                sha: "f2".to_string(),
                repository: "r".to_string(),
                message: "feat 2".to_string(),
                category: Some("feature".to_string()),
                effort: Some("M".to_string()),
                effort_rank: 3,
            },
            CommitRecord {
                sha: "b1".to_string(),
                repository: "r".to_string(),
                message: "bugfix 1".to_string(),
                category: Some("bugfix".to_string()),
                effort: Some("XS".to_string()),
                effort_rank: 1,
            },
            CommitRecord {
                sha: "b2".to_string(),
                repository: "r".to_string(),
                message: "bugfix 2".to_string(),
                category: Some("bugfix".to_string()),
                effort: Some("S".to_string()),
                effort_rank: 2,
            },
            CommitRecord {
                sha: "r1".to_string(),
                repository: "r".to_string(),
                message: "refactor 1".to_string(),
                category: Some("refactor".to_string()),
                effort: Some("L".to_string()),
                effort_rank: 4,
            },
        ];

        let selected = stratify_and_select(&commits, 3);
        assert_eq!(selected.len(), 3);

        let cats: Vec<Option<&str>> = selected.iter().map(|c| c.category.as_deref()).collect();
        assert!(
            cats.contains(&Some("bugfix")),
            "should include a bugfix: {cats:?}"
        );
        assert!(
            cats.contains(&Some("feature")),
            "should include a feature: {cats:?}"
        );
    }

    /// Why: the sampler must fall back to descending-effort selection when
    /// no priority categories are present.
    /// What: seeds commits with only `chore` category, asserts highest-effort
    /// commit is first in the selection.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_falls_back_to_effort_ordering() {
        let commits = vec![
            CommitRecord {
                sha: "c1".to_string(),
                repository: "r".to_string(),
                message: "chore small".to_string(),
                category: Some("chore".to_string()),
                effort: Some("XS".to_string()),
                effort_rank: 1,
            },
            CommitRecord {
                sha: "c2".to_string(),
                repository: "r".to_string(),
                message: "chore large".to_string(),
                category: Some("chore".to_string()),
                effort: Some("XL".to_string()),
                effort_rank: 5,
            },
            CommitRecord {
                sha: "c3".to_string(),
                repository: "r".to_string(),
                message: "chore medium".to_string(),
                category: Some("chore".to_string()),
                effort: Some("M".to_string()),
                effort_rank: 3,
            },
        ];

        let selected = stratify_and_select(&commits, 2);
        assert_eq!(selected.len(), 2);
        // First slot should be the XL commit (highest effort).
        assert_eq!(selected[0].sha, "c2");
    }

    /// Why: `truncate_diff` must produce output no longer than MAX_DIFF_CHARS
    /// (plus the truncation marker) and append the marker string.
    /// What: creates a string longer than MAX_DIFF_CHARS, calls truncate_diff,
    /// asserts length constraint and marker presence.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_truncates_long_diff() {
        let big = "x".repeat(MAX_DIFF_CHARS + 5000);
        let result = truncate_diff(&big);
        // The result should contain at most MAX_DIFF_CHARS chars of content
        // plus the marker suffix.
        assert!(
            result.contains("[... diff truncated"),
            "must contain truncation marker"
        );
        let content_chars = result.chars().count();
        // Allow for marker overhead (~40 chars).
        assert!(
            content_chars <= MAX_DIFF_CHARS + 60,
            "truncated diff too long: {content_chars}"
        );
    }

    /// Why: a diff shorter than MAX_DIFF_CHARS must be returned unchanged.
    /// What: passes a short diff, asserts it equals the input.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_short_diff_unchanged() {
        let short = "+fn hello() { println!(\"hi\"); }";
        assert_eq!(truncate_diff(short), short);
    }

    /// Why: commits in repos that are not locally available must be silently
    /// skipped without aborting the profile run.
    /// What: seeds one commit in the DB with a repo path that doesn't exist,
    /// calls `sample_diffs_for_batches`, asserts no diffs were added and no
    /// error was returned.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_skips_missing_repo() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com");
        seed_commit_with_category_effort(
            &db,
            "sha_missing_repo",
            aid,
            "nonexistent-repo",
            "2024-01-08T00:00:00Z",
            Some("feature"),
            Some("M"),
        );

        let stats = tga::report::period_trends::query_author_period_trends(
            &db,
            "alice@example.com",
            4,
            None,
            None,
        )
        .expect("query trends");
        assert!(!stats.is_empty(), "should have at least one period");

        let mut batches: Vec<PeriodBatch> =
            stats.into_iter().map(PeriodBatch::from_stats).collect();

        let config = DiffSamplerConfig {
            max_diffs: 3,
            repo_paths: {
                let mut m = HashMap::new();
                m.insert(
                    "nonexistent-repo".to_string(),
                    PathBuf::from("/tmp/this-path-absolutely-does-not-exist-trusty-review"),
                );
                m
            },
            repos_root: None,
        };

        // Must not return an error.
        sample_diffs_for_batches(&mut batches, &db, "alice@example.com", &config)
            .expect("sample_diffs must not error on missing repo");

        let total_diffs: usize = batches.iter().map(|b| b.sampled_diffs.len()).sum();
        assert_eq!(
            total_diffs, 0,
            "no diffs should be collected for missing repos"
        );
    }

    /// Why: when the repo is available locally, diffs must be fetched and
    /// attached to the batch.
    /// What: creates a real temp git repo, seeds a commit in the DB with its
    /// SHA and path, calls `sample_diffs_for_batches`, asserts a diff was added.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_fetches_real_diff() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo_path = tmp.path().to_path_buf();

        // Create initial commit (needed to have a parent for the follow-up).
        make_repo_with_initial_commit(tmp.path(), "hello.txt", "hello world\n");
        // Second commit — this is the one we'll sample.
        let sha = add_commit(tmp.path(), "hello.txt", "hello universe\n");

        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com");

        let repo_name = "test-repo";
        seed_commit_with_category_effort(
            &db,
            &sha,
            aid,
            repo_name,
            "2024-01-08T00:00:00Z",
            Some("feature"),
            Some("S"),
        );

        let stats = tga::report::period_trends::query_author_period_trends(
            &db,
            "alice@example.com",
            4,
            None,
            None,
        )
        .expect("query trends");
        assert!(!stats.is_empty());

        let mut batches: Vec<PeriodBatch> =
            stats.into_iter().map(PeriodBatch::from_stats).collect();

        let config = DiffSamplerConfig {
            max_diffs: 3,
            repo_paths: {
                let mut m = HashMap::new();
                m.insert(repo_name.to_string(), repo_path);
                m
            },
            repos_root: None,
        };

        sample_diffs_for_batches(&mut batches, &db, "alice@example.com", &config)
            .expect("sample_diffs");

        let total_diffs: usize = batches.iter().map(|b| b.sampled_diffs.len()).sum();
        assert_eq!(total_diffs, 1, "one diff should have been sampled");

        let diff = &batches[0].sampled_diffs[0];
        assert_eq!(diff.sha, sha);
        assert!(
            diff.diff_text.contains("+hello universe"),
            "diff text must contain the added line"
        );
        assert_eq!(diff.category, Some("feature".to_string()));
    }

    /// Why: max_diffs must cap the number of sampled diffs per period.
    /// What: seeds 10 commits, sets max_diffs=2, asserts at most 2 diffs per batch.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_respects_max_diffs() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo_path = tmp.path().to_path_buf();

        // Create a repo with multiple commits.
        make_repo_with_initial_commit(tmp.path(), "f.txt", "v0\n");
        let mut shas = Vec::new();
        for i in 1..=5 {
            let sha = add_commit(tmp.path(), "f.txt", &format!("v{i}\n"));
            shas.push(sha);
        }

        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com");
        let repo_name = "myrepo";

        for (i, sha) in shas.iter().enumerate() {
            seed_commit_with_category_effort(
                &db,
                sha,
                aid,
                repo_name,
                &format!("2024-01-{:02}T00:00:00Z", i + 8),
                Some("feature"),
                None,
            );
        }

        let stats = tga::report::period_trends::query_author_period_trends(
            &db,
            "alice@example.com",
            4,
            None,
            None,
        )
        .expect("query trends");
        let mut batches: Vec<PeriodBatch> =
            stats.into_iter().map(PeriodBatch::from_stats).collect();

        let config = DiffSamplerConfig {
            max_diffs: 2,
            repo_paths: {
                let mut m = HashMap::new();
                m.insert(repo_name.to_string(), repo_path);
                m
            },
            repos_root: None,
        };

        sample_diffs_for_batches(&mut batches, &db, "alice@example.com", &config)
            .expect("sample_diffs");

        for batch in &batches {
            assert!(
                batch.sampled_diffs.len() <= 2,
                "max_diffs=2 must cap sampled diffs per period, got {}",
                batch.sampled_diffs.len()
            );
        }
    }

    /// Why: `DiffSamplerConfig::repo_path` must prefer the explicit map entry
    /// over the repos_root.
    /// What: sets both repo_paths and repos_root, queries a repo in repo_paths,
    /// asserts the explicit path is returned.
    /// Test: this test itself.
    #[test]
    fn config_repo_path_resolution() {
        let config = DiffSamplerConfig {
            repos_root: Some(PathBuf::from("/repos")),
            repo_paths: {
                let mut m = HashMap::new();
                m.insert("acme".to_string(), PathBuf::from("/explicit/acme"));
                m
            },
            max_diffs: DEFAULT_MAX_DIFFS,
        };

        assert_eq!(
            config.repo_path("acme"),
            Some(PathBuf::from("/explicit/acme")),
            "explicit entry must win over repos_root"
        );
        assert_eq!(
            config.repo_path("other"),
            Some(PathBuf::from("/repos/other")),
            "repos_root fallback must work"
        );
        assert_eq!(
            DiffSamplerConfig::default().repo_path("anything"),
            None,
            "no config → None"
        );
    }
}
