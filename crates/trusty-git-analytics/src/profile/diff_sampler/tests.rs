//! Tests for the diff sampler.
//!
//! Why: the sampler touches real git repositories and a live SQLite schema, so
//! its scaffolding is bulky enough to keep out of `sampler.rs`.
//! What: covers stratification, effort-ordering fallback, truncation,
//! missing-repo skipping, a real diff fetch, the `max_diffs` cap, and config
//! path resolution.
//! Test: every test here is self-contained — temp git repos and in-memory DBs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::params;

use super::config::{DiffSamplerConfig, DEFAULT_MAX_DIFFS};
use super::sampler::{sample_diffs_for_batches, stratify_and_select, truncate_diff, CommitRecord};
use super::MAX_DIFF_CHARS;
use crate::core::db::Database;
use crate::profile::types::period::PeriodBatch;

// ── DB seed helpers ──────────────────────────────────────────────────────────

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
    let cls_id: Option<i64> = category.map(|cat| {
        db.connection()
            .execute(
                "INSERT OR IGNORE INTO classifications (id, category, confidence, method) \
                 VALUES (NULL, ?1, 0.9, 'rule')",
                params![cat],
            )
            .expect("insert classification");
        db.connection()
            .query_row(
                "SELECT id FROM classifications WHERE category = ?1 LIMIT 1",
                params![cat],
                |r| r.get(0),
            )
            .expect("get classification id")
    });

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

fn batches_for(db: &Database, email: &str) -> Vec<PeriodBatch> {
    let stats = crate::report::period_trends::query_author_period_trends(db, email, 4, None, None)
        .expect("query trends");
    stats.into_iter().map(PeriodBatch::from_stats).collect()
}

fn record(sha: &str, category: &str, effort: &str, effort_rank: u8) -> CommitRecord {
    CommitRecord {
        sha: sha.to_string(),
        repository: "r".to_string(),
        message: format!("{category} commit"),
        category: Some(category.to_string()),
        effort: Some(effort.to_string()),
        effort_rank,
    }
}

// ── Git repo helpers ─────────────────────────────────────────────────────────

fn make_repo_with_initial_commit(dir: &Path, filename: &str, content: &str) -> String {
    let repo = git2::Repository::init(dir).expect("init repo");
    let mut config = repo.config().expect("config");
    config.set_str("user.name", "Test User").expect("set name");
    config
        .set_str("user.email", "test@example.com")
        .expect("set email");

    std::fs::write(dir.join(filename), content).expect("write file");

    let mut index = repo.index().expect("index");
    index.add_path(Path::new(filename)).expect("add path");
    index.write().expect("write index");

    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig = git2::Signature::now("Test User", "test@example.com").expect("sig");
    repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
        .expect("initial commit")
        .to_string()
}

fn add_commit(dir: &Path, filename: &str, new_content: &str) -> String {
    let repo = git2::Repository::open(dir).expect("open repo");

    std::fs::write(dir.join(filename), new_content).expect("write file");

    let mut index = repo.index().expect("index");
    index.add_path(Path::new(filename)).expect("add path");
    index.write().expect("write index");

    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig = git2::Signature::now("Test User", "test@example.com").expect("sig");
    let head = repo.head().expect("head").peel_to_commit().expect("peel");
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "Follow-up commit",
        &tree,
        &[&head],
    )
    .expect("follow-up commit")
    .to_string()
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Why: a sample that shows only features would let bugfix quality — the thing
/// a profile is most often asked about — go unexamined.
/// What: offers five commits across three categories with `max_diffs = 3` and
/// asserts both a bugfix and a feature are chosen.
/// Test: this test itself.
#[test]
fn diff_sampler_stratification() {
    let commits = vec![
        record("f1", "feature", "S", 2),
        record("f2", "feature", "M", 3),
        record("b1", "bugfix", "XS", 1),
        record("b2", "bugfix", "S", 2),
        record("r1", "refactor", "L", 4),
    ];

    let selected = stratify_and_select(&commits, 3);
    assert_eq!(selected.len(), 3);

    let cats: Vec<Option<&str>> = selected.iter().map(|c| c.category.as_deref()).collect();
    assert!(
        cats.contains(&Some("bugfix")),
        "includes a bugfix: {cats:?}"
    );
    assert!(
        cats.contains(&Some("feature")),
        "includes a feature: {cats:?}"
    );
}

/// Why: with no priority category present the sampler still has to pick
/// something informative, and the largest commits carry the most signal.
/// What: offers only `chore` commits and asserts the XL one is picked first.
/// Test: this test itself.
#[test]
fn diff_sampler_falls_back_to_effort_ordering() {
    let commits = vec![
        record("c1", "chore", "XS", 1),
        record("c2", "chore", "XL", 5),
        record("c3", "chore", "M", 3),
    ];

    let selected = stratify_and_select(&commits, 2);
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].sha, "c2");
}

/// Why: an untruncated giant diff would crowd out every other period's
/// evidence, and a reader must be able to tell a diff was cut.
/// What: truncates a string well over the cap and asserts both the marker and
/// the length bound.
/// Test: this test itself.
#[test]
fn diff_sampler_truncates_long_diff() {
    let big = "x".repeat(MAX_DIFF_CHARS + 5000);
    let result = truncate_diff(&big);
    assert!(
        result.contains("[... diff truncated"),
        "must carry the truncation marker"
    );
    let content_chars = result.chars().count();
    assert!(
        content_chars <= MAX_DIFF_CHARS + 60,
        "truncated diff too long: {content_chars}"
    );
}

/// Why: the common case is a diff under the cap, and appending a marker to an
/// intact diff would be a lie.
/// What: passes a short diff and asserts it comes back byte-identical.
/// Test: this test itself.
#[test]
fn diff_sampler_short_diff_unchanged() {
    let short = "+fn hello() { println!(\"hi\"); }";
    assert_eq!(truncate_diff(short), short);
}

/// Why: a profile spanning ten repositories must not fail because one is not
/// checked out on this machine.
/// What: points the config at a path that does not exist and asserts the run
/// succeeds with zero diffs collected.
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

    let mut batches = batches_for(&db, "alice@example.com");
    assert!(!batches.is_empty(), "should have at least one period");

    let config = DiffSamplerConfig {
        max_diffs: 3,
        repo_paths: HashMap::from([(
            "nonexistent-repo".to_string(),
            PathBuf::from("/tmp/this-path-absolutely-does-not-exist-tga-profile"),
        )]),
        repos_root: None,
    };

    sample_diffs_for_batches(&mut batches, &db, "alice@example.com", &config)
        .expect("a missing repo must not fail the run");

    let total_diffs: usize = batches.iter().map(|b| b.sampled_diffs.len()).sum();
    assert_eq!(total_diffs, 0, "no diffs collected for a missing repo");
}

/// Why: the whole pass exists to attach real diff text; a green unit test over
/// mocks would not prove libgit2 is wired to the right commit.
/// What: builds a real temp repository, records its second commit in the
/// database, samples, and asserts the added line appears in the diff text.
/// Test: this test itself.
#[test]
fn diff_sampler_fetches_real_diff() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_path = tmp.path().to_path_buf();

    make_repo_with_initial_commit(tmp.path(), "hello.txt", "hello world\n");
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

    let mut batches = batches_for(&db, "alice@example.com");
    assert!(!batches.is_empty());

    let config = DiffSamplerConfig {
        max_diffs: 3,
        repo_paths: HashMap::from([(repo_name.to_string(), repo_path)]),
        repos_root: None,
    };

    sample_diffs_for_batches(&mut batches, &db, "alice@example.com", &config)
        .expect("sample_diffs");

    let total_diffs: usize = batches.iter().map(|b| b.sampled_diffs.len()).sum();
    assert_eq!(total_diffs, 1, "one diff sampled");

    let diff = &batches[0].sampled_diffs[0];
    assert_eq!(diff.sha, sha);
    assert!(
        diff.diff_text.contains("+hello universe"),
        "diff text must contain the added line"
    );
    assert_eq!(diff.category, Some("feature".to_string()));
}

/// Why: the cap is the sampler's only defence against a period with hundreds of
/// commits, so it has to hold per period, not per run.
/// What: seeds five commits in one period with `max_diffs = 2` and asserts no
/// batch exceeds two diffs.
/// Test: this test itself.
#[test]
fn diff_sampler_respects_max_diffs() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_path = tmp.path().to_path_buf();

    make_repo_with_initial_commit(tmp.path(), "f.txt", "v0\n");
    let shas: Vec<String> = (1..=5)
        .map(|i| add_commit(tmp.path(), "f.txt", &format!("v{i}\n")))
        .collect();

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

    let mut batches = batches_for(&db, "alice@example.com");
    let config = DiffSamplerConfig {
        max_diffs: 2,
        repo_paths: HashMap::from([(repo_name.to_string(), repo_path)]),
        repos_root: None,
    };

    sample_diffs_for_batches(&mut batches, &db, "alice@example.com", &config)
        .expect("sample_diffs");

    for batch in &batches {
        assert!(
            batch.sampled_diffs.len() <= 2,
            "max_diffs=2 must cap each period, got {}",
            batch.sampled_diffs.len()
        );
    }
}

/// Why: a caller that names an explicit checkout for one repository expects it
/// to win over the generic root, otherwise the explicit map is decorative.
/// What: sets both, then asserts the explicit path, the root fallback, and the
/// `None` result when neither is configured.
/// Test: this test itself.
#[test]
fn config_repo_path_resolution() {
    let config = DiffSamplerConfig {
        repos_root: Some(PathBuf::from("/repos")),
        repo_paths: HashMap::from([("acme".to_string(), PathBuf::from("/explicit/acme"))]),
        max_diffs: DEFAULT_MAX_DIFFS,
    };

    assert_eq!(
        config.repo_path("acme"),
        Some(PathBuf::from("/explicit/acme")),
        "explicit entry wins over repos_root"
    );
    assert_eq!(
        config.repo_path("other"),
        Some(PathBuf::from("/repos/other")),
        "repos_root fallback applies"
    );
    assert_eq!(
        DiffSamplerConfig::default().repo_path("anything"),
        None,
        "nothing configured → None"
    );
}
