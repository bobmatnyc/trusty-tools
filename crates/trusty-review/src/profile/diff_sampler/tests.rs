//! Tests for the diff sampler.
//!
//! Why: the diff sampler touches real git repos and an in-memory SQLite DB;
//! isolated tests here keep `sampler.rs` free of test scaffolding.
//! What: exercises stratification, truncation, missing-repo skipping, real
//! diff fetching, max_diffs capping, and config path resolution.
//! Test: all tests are self-contained; they use tempfile repos and in-memory DBs.

#[cfg(test)]
mod diff_sampler_tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::path::PathBuf;

    use rusqlite::params;
    use tga::core::db::Database;

    use crate::profile::diff_sampler::MAX_DIFF_CHARS;
    use crate::profile::diff_sampler::config::{DEFAULT_MAX_DIFFS, DiffSamplerConfig};
    use crate::profile::diff_sampler::sampler::{
        CommitRecord, sample_diffs_for_batches, stratify_and_select, truncate_diff,
    };
    use crate::profile::types::period::PeriodBatch;

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
    /// when both categories are present.
    /// What: seeds 5 commits across categories, samples with max_diffs=3,
    /// asserts ≥1 of each priority category.
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
        assert!(
            result.contains("[... diff truncated"),
            "must contain truncation marker"
        );
        let content_chars = result.chars().count();
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
    /// What: seeds 5 commits, sets max_diffs=2, asserts at most 2 diffs per batch.
    /// Test: this test itself.
    #[test]
    fn diff_sampler_respects_max_diffs() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo_path = tmp.path().to_path_buf();

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
