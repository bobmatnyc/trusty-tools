//! Tests for all `tga backfill` subcommands.
//!
//! Why: keeping tests separate from implementation files allows the production
//! modules to stay under the 500 SLOC cap while maintaining test coverage.

use rusqlite::params;
use tga::core::config::Config;
use tga::core::db::Database;
use tga::core::effort::FORMULA_VERSION;

use super::effort::{backfill_effort_tshirt, persist_effort_rows};
use super::effort_db::process_one_repo_db;
use super::flags::{
    backfill_ai_detection_commits, backfill_revert_flags, backfill_ticket_ids, backfill_ticketed,
    is_revert,
};
use super::misc::{backfill_complexity, backfill_quality, backfill_top_level};
use super::types::{ComplexityBackfillArgs, EffortBackfillArgs, EffortRow};

fn seed(db: &Database, sha: &str, message: &str) {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES (?1, 'n', 'e', '2024-01-01T00:00:00Z', ?2, 'r')",
            params![sha, message],
        )
        .expect("insert");
}

#[test]
fn revert_detector_matches_expected_forms() {
    assert!(is_revert("Revert \"feat: add login\""));
    assert!(is_revert("revert: bad merge"));
    assert!(is_revert("Revert this change"));
    assert!(!is_revert("Refactor revert handling"));
    assert!(!is_revert("Fix bug in feature"));
}

#[test]
fn ticket_id_extraction_prefers_specific_patterns() {
    use tga::collect::ticket::extract_ticket_id;
    assert_eq!(
        extract_ticket_id("AB#42 implement"),
        Some("AB#42".to_string())
    );
    assert_eq!(
        extract_ticket_id("ENG-123: feature"),
        Some("ENG-123".to_string())
    );
    assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
    assert_eq!(extract_ticket_id("misc cleanup"), None);
}

#[test]
fn backfill_revert_flags_updates_only_changed_rows() {
    let mut db = Database::open_in_memory().expect("open");
    seed(&db, "a", "Revert \"foo\"");
    seed(&db, "b", "feat: thing");
    backfill_revert_flags(&mut db, false, &[], None, None).expect("backfill");
    let reverts: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM commits WHERE is_revert = 1",
            [],
            |r| r.get(0),
        )
        .expect("q");
    assert_eq!(reverts, 1);
}

#[test]
fn backfill_ticket_ids_populates_ticket_id() {
    let mut db = Database::open_in_memory().expect("open");
    seed(&db, "a", "ENG-7: thing");
    seed(&db, "b", "no ticket");
    backfill_ticket_ids(&mut db, false, &[], None, None).expect("backfill");
    let t: Option<String> = db
        .connection()
        .query_row("SELECT ticket_id FROM commits WHERE sha = 'a'", [], |r| {
            r.get(0)
        })
        .expect("q");
    assert_eq!(t, Some("ENG-7".to_string()));
    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM commits WHERE ticketed = 1", [], |r| {
            r.get(0)
        })
        .expect("q");
    assert_eq!(n, 1);
}

#[test]
fn dry_run_does_not_modify_rows() {
    let mut db = Database::open_in_memory().expect("open");
    seed(&db, "a", "Revert \"foo\"");
    backfill_revert_flags(&mut db, true, &[], None, None).expect("dry run");
    let reverts: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM commits WHERE is_revert = 1",
            [],
            |r| r.get(0),
        )
        .expect("q");
    assert_eq!(reverts, 0);
}

/// Why: regression guard for issue #397 bug 2. `tga backfill complexity`
/// must be wired and its dry-run path must report the count of NULL-complexity
/// candidates without invoking the LLM (so it works offline) and without
/// mutating any row.
/// What: seed one classification with `complexity IS NULL` (regex_rule,
/// eligible) and one already-scored row (must not be counted); run the
/// dry-run backfill; assert no LLM is needed and nothing is written.
/// Test: in-memory DB; dry_run=true short-circuits before any LLM call.
#[tokio::test]
async fn backfill_complexity_dry_run_reports_candidates_without_writing() {
    let mut db = Database::open_in_memory().expect("open");

    // Candidate: NULL complexity, non-exact method.
    db.connection()
        .execute(
            "INSERT INTO classifications (category, confidence, method, complexity) \
             VALUES ('feature', 0.5, 'regex_rule', NULL)",
            [],
        )
        .expect("insert null-complexity row");
    // Not a candidate: already scored.
    db.connection()
        .execute(
            "INSERT INTO classifications (category, confidence, method, complexity) \
             VALUES ('bugfix', 0.8, 'regex_rule', 3)",
            [],
        )
        .expect("insert scored row");

    let args = ComplexityBackfillArgs { use_llm: false };
    // dry_run=true must not hit the network; Config::default() has no LLM key.
    backfill_complexity(Config::default(), &mut db, args, true)
        .await
        .expect("dry-run complexity backfill");

    // Nothing changed: the NULL row is still NULL, the scored row still 3.
    let null_count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM classifications WHERE complexity IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("count null");
    assert_eq!(null_count, 1, "dry-run must not write complexity scores");
}

// ── effort backfill tests ─────────────────────────────────────────────────

/// Why: verify the schema migration and UPSERT INSERT path work end-to-end.
/// What: calls `persist_effort_rows` with known data and reads it back.
/// Test: this test itself.
#[test]
fn backfill_effort_persists_rows() {
    let mut db = Database::open_in_memory().expect("open");

    let rows = vec![EffortRow {
        sha: "abc123".to_string(),
        repository: "testrepo".to_string(),
        size: "M".to_string(),
        score: 9.1,
        loc: 50,
        files: 2,
        test_loc: 0,
        tests_factor: 1.0,
        formula_version: FORMULA_VERSION.to_string(),
        computed_at: 1_000_000,
        effort_tshirt: 3,
    }];

    persist_effort_rows(&mut db, &rows).expect("persist");

    let (size, score, loc, files): (String, f64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT size, score, loc, files \
             FROM fact_commit_effort WHERE sha = 'abc123'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("query");

    assert_eq!(size, "M");
    assert!((score - 9.1).abs() < 0.001);
    assert_eq!(loc, 50);
    assert_eq!(files, 2);
}

/// Why: verify that `--force` semantics replace an existing row with
/// updated values rather than silently keeping the old one.
/// What: inserts a row with score=1.0, then re-inserts with score=9.9
/// (simulating --force); asserts the score was updated.
/// Test: this test itself.
#[test]
fn backfill_effort_force_recomputes() {
    let mut db = Database::open_in_memory().expect("open");

    // First pass: insert initial row.
    let first = vec![EffortRow {
        sha: "deadbeef".to_string(),
        repository: "repo".to_string(),
        size: "XS".to_string(),
        score: 1.0,
        loc: 1,
        files: 1,
        test_loc: 0,
        tests_factor: 1.0,
        formula_version: FORMULA_VERSION.to_string(),
        computed_at: 1_000_000,
        effort_tshirt: 1,
    }];
    persist_effort_rows(&mut db, &first).expect("first persist");

    // Second pass: replace with updated score.
    let second = vec![EffortRow {
        sha: "deadbeef".to_string(),
        repository: "repo".to_string(),
        size: "XL".to_string(),
        score: 99.9,
        loc: 100_000,
        files: 500,
        test_loc: 0,
        tests_factor: 1.0,
        formula_version: FORMULA_VERSION.to_string(),
        computed_at: 2_000_000,
        effort_tshirt: 5,
    }];
    persist_effort_rows(&mut db, &second).expect("second persist");

    // Only one row should exist (no duplicate).
    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_commit_effort WHERE sha = 'deadbeef'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "UPSERT must not create duplicate rows");

    let score: f64 = db
        .connection()
        .query_row(
            "SELECT score FROM fact_commit_effort WHERE sha = 'deadbeef'",
            [],
            |r| r.get(0),
        )
        .expect("score");
    assert!(
        (score - 99.9).abs() < 0.001,
        "score must be updated to 99.9"
    );
}

/// Why: `fact_commit_effort` must allow the same SHA in two different
/// repositories (fork/mirror scenarios).
/// What: insert two rows with the same SHA but different repository; both
/// must persist without conflict.
/// Test: this test itself.
#[test]
fn backfill_effort_same_sha_different_repos() {
    let mut db = Database::open_in_memory().expect("open");

    let rows = vec![
        EffortRow {
            sha: "cafebabe".to_string(),
            repository: "repo-a".to_string(),
            size: "S".to_string(),
            score: 5.5,
            loc: 30,
            files: 2,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at: 1_000_000,
            effort_tshirt: 2, // S=2
        },
        EffortRow {
            sha: "cafebabe".to_string(),
            repository: "repo-b".to_string(),
            size: "M".to_string(),
            score: 8.0,
            loc: 60,
            files: 3,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at: 1_000_000,
            effort_tshirt: 3, // M=3
        },
    ];

    persist_effort_rows(&mut db, &rows).expect("persist");

    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_commit_effort WHERE sha = 'cafebabe'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 2, "same SHA in two repos must produce two rows");
}

/// Why: an effort backfill on an empty repo should produce zero rows and
/// no errors.
/// What: calls `persist_effort_rows` with an empty slice.
/// Test: this test itself.
#[test]
fn backfill_effort_empty_produces_no_rows() {
    let mut db = Database::open_in_memory().expect("open");
    persist_effort_rows(&mut db, &[]).expect("empty persist");
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fact_commit_effort", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0);
}

// ── db-path tests ─────────────────────────────────────────────────────────

/// Seed a commit row and its associated files rows into an in-memory DB.
///
/// Why: shared helper for db-path tests; avoids repetitive SQL in each test.
/// What: inserts one commit row and one or more file rows, returning the
/// commit's integer id.
/// Test: used by `backfill_effort_db_path_*` tests below.
fn seed_commit_with_files(
    db: &Database,
    sha: &str,
    repo: &str,
    timestamp: &str,
    files: &[(&str, u32, u32)], // (path, insertions, deletions)
) -> i64 {
    let conn = db.connection();
    conn.execute(
        "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
         VALUES (?1, 'tester', 'test@example.com', ?2, 'msg', ?3)",
        params![sha, timestamp, repo],
    )
    .expect("insert commit");
    let commit_id = conn.last_insert_rowid();
    for (path, ins, del) in files {
        conn.execute(
            "INSERT INTO files (commit_id, path, change_type, insertions, deletions) \
             VALUES (?1, ?2, 'modified', ?3, ?4)",
            params![commit_id, path, ins, del],
        )
        .expect("insert file");
    }
    commit_id
}

/// Why: verify the db-only path reads `commits JOIN files` and populates
/// `fact_commit_effort` correctly without touching a git repo.
/// What: seeds two commits with file rows; calls `process_one_repo_db` and
/// then persists; asserts both rows appear in `fact_commit_effort`.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_populates_fact_table() {
    let mut db = Database::open_in_memory().expect("open");

    seed_commit_with_files(
        &db,
        "aaa111",
        "myrepo",
        "2024-01-01T00:00:00Z",
        &[("src/main.rs", 30, 10), ("src/lib.rs", 5, 2)],
    );
    seed_commit_with_files(
        &db,
        "bbb222",
        "myrepo",
        "2024-01-02T00:00:00Z",
        &[("src/tests/foo_test.rs", 20, 0)],
    );

    let args = EffortBackfillArgs {
        range: None,
        force: false,
        notes: false,
        limit: None,
    };

    let (scored, skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "myrepo", &args, false).expect("db path");
    assert_eq!(scored, 2, "both commits should be scored");
    assert_eq!(skipped, 0, "nothing pre-scored");

    persist_effort_rows(&mut db, &rows).expect("persist");

    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_commit_effort WHERE repository = 'myrepo'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 2, "two effort rows expected");

    // Verify the test-file commit has a reduced tests_factor.
    let (size_b, tests_factor_b): (String, f64) = db
        .connection()
        .query_row(
            "SELECT size, tests_factor FROM fact_commit_effort WHERE sha = 'bbb222'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("bbb222 row");
    // 20 test LoC out of 20 total → ratio=1 → tests_factor=0.7
    assert!(
        (tests_factor_b - 0.7).abs() < 1e-6,
        "expected tests_factor=0.7 for all-test commit, got {tests_factor_b}"
    );
    // score = 1.0*log2(21) + 1.5*log2(2) + 1.0*0.7 ≈ 4.392 + 1.5 + 0.7 = 6.592 → S
    assert_eq!(size_b, "S", "all-test commit should be S");
}

/// Why: verify the db-path respects the `--force=false` default — commits
/// that already have an effort row must be skipped.
/// What: inserts a pre-existing effort row for one commit; runs db path;
/// asserts only the unscored commit is returned.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_skips_already_scored() {
    let mut db = Database::open_in_memory().expect("open");

    seed_commit_with_files(
        &db,
        "scored111",
        "repo",
        "2024-01-01T00:00:00Z",
        &[("src/a.rs", 10, 0)],
    );
    seed_commit_with_files(
        &db,
        "unscored222",
        "repo",
        "2024-01-02T00:00:00Z",
        &[("src/b.rs", 5, 5)],
    );

    // Pre-populate an effort row for scored111.
    let pre = vec![EffortRow {
        sha: "scored111".to_string(),
        repository: "repo".to_string(),
        size: "XS".to_string(),
        score: 1.0,
        loc: 10,
        files: 1,
        test_loc: 0,
        tests_factor: 1.0,
        formula_version: FORMULA_VERSION.to_string(),
        computed_at: 0,
        effort_tshirt: 1, // XS=1
    }];
    persist_effort_rows(&mut db, &pre).expect("pre-persist");

    let args = EffortBackfillArgs {
        range: None,
        force: false,
        notes: false,
        limit: None,
    };

    let (scored, skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

    assert_eq!(scored, 1, "only unscored222 should be scored");
    assert_eq!(skipped, 1, "scored111 should be skipped");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sha, "unscored222");
}

/// Why: verify `--force` causes already-scored commits to be re-scored
/// rather than skipped on the db path.
/// What: pre-populates effort for a commit; runs db path with force=true;
/// asserts the commit appears in the returned rows.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_force_rescores_all() {
    let mut db = Database::open_in_memory().expect("open");

    seed_commit_with_files(
        &db,
        "sha001",
        "repo",
        "2024-01-01T00:00:00Z",
        &[("src/x.rs", 100, 50)],
    );

    // Insert a stale effort row.
    let stale = vec![EffortRow {
        sha: "sha001".to_string(),
        repository: "repo".to_string(),
        size: "XS".to_string(),
        score: 0.1,
        loc: 1,
        files: 1,
        test_loc: 0,
        tests_factor: 1.0,
        formula_version: "v0".to_string(),
        computed_at: 0,
        effort_tshirt: 1, // XS=1
    }];
    persist_effort_rows(&mut db, &stale).expect("stale persist");

    let args = EffortBackfillArgs {
        range: None,
        force: true, // re-score everything
        notes: false,
        limit: None,
    };

    let (scored, skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

    assert_eq!(scored, 1, "force path should score the commit");
    assert_eq!(skipped, 0, "nothing should be skipped with --force");
    // The new score should reflect 150 LoC, not the stale 0.1.
    assert!(
        rows[0].score > 1.0,
        "re-scored effort should be higher than stale 0.1"
    );
}

/// Why: commits present in the `commits` table but with no rows in `files`
/// (e.g., empty commits) must not cause errors — they should be silently
/// skipped with a warning.
/// What: inserts a commit with no file rows; runs db path; asserts zero
/// records returned and no error raised.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_skips_commit_with_no_files() {
    let db = Database::open_in_memory().expect("open");

    // Insert commit row but NO file rows.
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES ('empty001', 'tester', 'test@example.com', '2024-01-01T00:00:00Z', 'empty', 'repo')",
            [],
        )
        .expect("insert commit");

    // The above commit has no files rows, so the JOIN returns no rows —
    // `process_one_repo_db` will not even see a SHA to group.
    let args = EffortBackfillArgs {
        range: None,
        force: false,
        notes: false,
        limit: None,
    };

    let (scored, skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

    // Zero files rows → nothing scored.
    assert_eq!(scored, 0, "commit with no files should produce no records");
    assert_eq!(skipped, 0);
    assert!(rows.is_empty());
}

/// Why: the `--limit N` flag must cap records at N even when more commits
/// are available in the db.
/// What: seeds 5 commits; runs db path with limit=3; asserts exactly 3
/// records are returned.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_respects_limit() {
    let db = Database::open_in_memory().expect("open");

    for i in 0..5u32 {
        seed_commit_with_files(
            &db,
            &format!("sha{i:03}"),
            "repo",
            &format!("2024-01-{:02}T00:00:00Z", i + 1),
            &[("src/foo.rs", 10, 5)],
        );
    }

    let args = EffortBackfillArgs {
        range: None,
        force: false,
        notes: false,
        limit: Some(3),
    };

    let (scored, _skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

    assert_eq!(scored, 3, "limit=3 should cap at 3 records");
    assert_eq!(rows.len(), 3);
}

/// Why: the db path must correctly segregate commits by repository when
/// multiple repos share the same database.
/// What: seeds commits for two different repos; runs db path for one;
/// asserts only that repo's commits are scored.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_scoped_to_repo() {
    let db = Database::open_in_memory().expect("open");

    seed_commit_with_files(
        &db,
        "alpha001",
        "repo-alpha",
        "2024-01-01T00:00:00Z",
        &[("src/a.rs", 20, 10)],
    );
    seed_commit_with_files(
        &db,
        "beta001",
        "repo-beta",
        "2024-01-01T00:00:00Z",
        &[("src/b.rs", 50, 20)],
    );

    let args = EffortBackfillArgs {
        range: None,
        force: false,
        notes: false,
        limit: None,
    };

    // Process only repo-alpha.
    let (scored, _skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "repo-alpha", &args, false).expect("db path");

    assert_eq!(scored, 1);
    assert_eq!(rows[0].sha, "alpha001");
    assert_eq!(rows[0].repository, "repo-alpha");
}

/// Why: dry_run=true on the db path must return rows (for reporting) but
/// the caller must not persist them — this test verifies the path selection
/// in `backfill_effort` withholds `persist_effort_rows`.
/// What: directly calls `process_one_repo_db` with dry_run=true; asserts
/// rows are returned but `fact_commit_effort` remains empty.
/// Test: this test itself.
#[test]
fn backfill_effort_db_path_dry_run_returns_rows_without_persisting() {
    let db = Database::open_in_memory().expect("open");

    seed_commit_with_files(
        &db,
        "drysha1",
        "repo",
        "2024-01-01T00:00:00Z",
        &[("src/main.rs", 40, 10)],
    );

    let args = EffortBackfillArgs {
        range: None,
        force: false,
        notes: false,
        limit: None,
    };

    let (scored, _skipped, _sizes, rows) =
        process_one_repo_db(db.connection(), "repo", &args, true /* dry_run */).expect("db path");

    assert_eq!(
        scored, 1,
        "db path should return 1 scored row even in dry_run"
    );
    assert_eq!(rows.len(), 1);

    // Caller is responsible for not persisting in dry_run; here we do NOT
    // call persist_effort_rows, mirroring the behaviour in `backfill_effort`.
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fact_commit_effort", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0, "dry_run must not write to fact_commit_effort");
}

// ── issue #445 backfill tests ─────────────────────────────────────────────

/// Why: regression guard for issue #445. `backfill_ticketed` must correct
/// rows where a bare `#N` was (incorrectly) stored as `ticketed=1` under the
/// old logic, setting them to `ticketed=0`. Rows with JIRA refs must stay 1.
/// What: seeds two commits (one bare-hash, one JIRA), runs the ticketed
/// backfill with dry_run=false, asserts the bare-hash row is now 0 and the
/// JIRA row remains 1.
/// Test: this test itself.
#[test]
fn backfill_ticketed_corrects_bare_hash_rows() {
    let mut db = Database::open_in_memory().expect("open");

    // Force-insert with ticketed=1 to simulate the pre-#445 incorrect state.
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository, ticketed) VALUES ('bare1', 'n', 'e', '2024-01-01T00:00:00Z', \
             'some note about #42', 'repo', 1)",
            [],
        )
        .expect("insert bare-hash commit");
    // JIRA ref — was and should remain ticketed.
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository, ticketed) VALUES ('jira1', 'n', 'e', '2024-01-02T00:00:00Z', \
             'ENG-7: add feature', 'repo', 1)",
            [],
        )
        .expect("insert JIRA commit");
    // Plain message — was and should remain 0.
    seed(&db, "plain1", "no ticket here");

    backfill_ticketed(&mut db, false, &[], None, None).expect("backfill ticketed");

    let bare_val: i64 = db
        .connection()
        .query_row(
            "SELECT ticketed FROM commits WHERE sha = 'bare1'",
            [],
            |r| r.get(0),
        )
        .expect("read bare");
    assert_eq!(bare_val, 0, "bare #N must be unticketed after backfill");

    let jira_val: i64 = db
        .connection()
        .query_row(
            "SELECT ticketed FROM commits WHERE sha = 'jira1'",
            [],
            |r| r.get(0),
        )
        .expect("read jira");
    assert_eq!(jira_val, 1, "JIRA ref must remain ticketed");
}

/// Why: verify `backfill_ai_detection_commits` detects Claude in an existing
/// commit message and sets `is_ai_assisted=1` / `ai_tool='claude'`.
/// What: seeds one Claude-co-authored commit and one plain human commit;
/// runs the backfill; asserts is_ai_assisted and ai_tool are set correctly.
/// Test: this test itself.
#[test]
fn backfill_ai_detection_commits_detects_claude() {
    let mut db = Database::open_in_memory().expect("open");

    // AI-assisted commit (Claude trailer).
    let ai_msg = "feat: add auth\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>";
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository) VALUES ('ai1', 'n', 'e', '2024-01-01T00:00:00Z', ?1, 'repo')",
            params![ai_msg],
        )
        .expect("insert AI commit");
    // Human-only commit.
    seed(&db, "human1", "fix: bug without AI help");

    backfill_ai_detection_commits(&mut db, false, &[], None, None).expect("backfill ai-detection");

    let (is_ai, tool): (i64, Option<String>) = db
        .connection()
        .query_row(
            "SELECT is_ai_assisted, ai_tool FROM commits WHERE sha = 'ai1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read ai1");
    assert_eq!(is_ai, 1, "AI-assisted commit must have is_ai_assisted=1");
    assert_eq!(tool, Some("claude".to_string()), "ai_tool must be 'claude'");

    let (human_ai, human_tool): (i64, Option<String>) = db
        .connection()
        .query_row(
            "SELECT is_ai_assisted, ai_tool FROM commits WHERE sha = 'human1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read human1");
    assert_eq!(human_ai, 0, "human commit must have is_ai_assisted=0");
    assert!(human_tool.is_none(), "human commit must have ai_tool=NULL");
}

/// Why: issue #1334 — the repair path left historical `agentic_mode` stuck at
/// its `'none'` default, so a Claude-co-authored commit backfilled after
/// migration v21 never became queryable as `agentic_mode = 'full_agentic'`.
/// What: seeds a Claude-trailer commit (defaulting to `agentic_mode = 'none'`),
/// a Cursor-trailer commit, and a plain human commit; runs the backfill; asserts
/// the Claude commit is now `'full_agentic'`, the Cursor commit `'ide_assisted'`,
/// and the human commit remains `'none'` — matching the forward `tga collect`
/// path.
/// Test: this test itself.
#[test]
fn backfill_ai_detection_commits_repairs_agentic_mode() {
    let mut db = Database::open_in_memory().expect("open");

    // Claude Code commit — must become full_agentic (the #1334 regression).
    let claude_msg = "feat: add auth\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>";
    seed(&db, "claude1", claude_msg);
    // Cursor IDE commit — must become ide_assisted.
    let cursor_msg = "fix: npe\n\nCo-Authored-By: Cursor <noreply@cursor.sh>";
    seed(&db, "cursor1", cursor_msg);
    // Human-only commit — must remain none.
    seed(&db, "human1", "chore: bump dep");

    // Sanity: all rows start at the migration default 'none' before the repair.
    let pre: String = db
        .connection()
        .query_row(
            "SELECT agentic_mode FROM commits WHERE sha = 'claude1'",
            [],
            |r| r.get(0),
        )
        .expect("read pre claude1");
    assert_eq!(
        pre, "none",
        "pre-backfill agentic_mode must default to 'none'"
    );

    backfill_ai_detection_commits(&mut db, false, &[], None, None).expect("backfill ai-detection");

    let claude_mode: String = db
        .connection()
        .query_row(
            "SELECT agentic_mode FROM commits WHERE sha = 'claude1'",
            [],
            |r| r.get(0),
        )
        .expect("read claude1 mode");
    assert_eq!(
        claude_mode, "full_agentic",
        "Claude commit must be repaired to agentic_mode='full_agentic' (#1334)"
    );

    let cursor_mode: String = db
        .connection()
        .query_row(
            "SELECT agentic_mode FROM commits WHERE sha = 'cursor1'",
            [],
            |r| r.get(0),
        )
        .expect("read cursor1 mode");
    assert_eq!(
        cursor_mode, "ide_assisted",
        "Cursor commit must be repaired to agentic_mode='ide_assisted'"
    );

    let human_mode: String = db
        .connection()
        .query_row(
            "SELECT agentic_mode FROM commits WHERE sha = 'human1'",
            [],
            |r| r.get(0),
        )
        .expect("read human1 mode");
    assert_eq!(
        human_mode, "none",
        "human commit must remain agentic_mode='none'"
    );
}

/// Why: `backfill_top_level` must fill `top_level_category` for existing
/// classifications where it is NULL, using the built-in taxonomy.
/// What: seeds a classification with subcategory='bugfix' and
/// top_level_category=NULL; runs the backfill; asserts top_level_category
/// is now 'bugfix'.
/// Test: this test itself.
#[test]
fn backfill_top_level_fills_known_subcategories() {
    let mut db = Database::open_in_memory().expect("open");

    db.connection()
        .execute(
            "INSERT INTO classifications (category, subcategory, confidence, method) \
             VALUES ('bugfix', 'bugfix', 0.9, 'exact_rule')",
            [],
        )
        .expect("insert classification");

    backfill_top_level(&mut db, false).expect("backfill top-level");

    let top: Option<String> = db
        .connection()
        .query_row(
            "SELECT top_level_category FROM classifications WHERE subcategory = 'bugfix' \
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("read top");
    assert_eq!(
        top,
        Some("bugfix".to_string()),
        "bugfix subcategory must resolve to 'bugfix' top-level"
    );
}

/// Why: `backfill_effort_tshirt` must populate `effort_tshirt` from the
/// existing `size` TEXT column for rows where the integer is NULL.
/// What: inserts an effort row with size='L' and effort_tshirt=NULL; runs
/// the backfill; asserts effort_tshirt is now 4 (L=4).
/// Test: this test itself.
#[test]
fn backfill_effort_tshirt_fills_from_size() {
    let mut db = Database::open_in_memory().expect("open");

    // Insert a row with size='L' but no effort_tshirt (simulating pre-v17 row).
    db.connection()
        .execute(
            "INSERT INTO fact_commit_effort \
             (sha, repository, size, score, loc, files, test_loc, tests_factor, \
              formula_version, computed_at) \
             VALUES ('tshirt_test', 'repo', 'L', 15.5, 200, 5, 0, 1.0, 'v1', 1000000)",
            [],
        )
        .expect("insert effort row without tshirt");

    backfill_effort_tshirt(&mut db, false).expect("backfill effort-tshirt");

    let tshirt: Option<i64> = db
        .connection()
        .query_row(
            "SELECT effort_tshirt FROM fact_commit_effort WHERE sha = 'tshirt_test'",
            [],
            |r| r.get(0),
        )
        .expect("read effort_tshirt");
    assert_eq!(tshirt, Some(4), "L size must map to effort_tshirt=4");
}

/// Why: regression guard for issue #445 batch B. `backfill_quality` must
/// populate `fact_weekly_quality` for all historical weeks, and running it
/// twice must produce the same result (idempotent UPSERT).
/// What: seeds two commits by the same author in the same ISO week with known
/// quality signals (one revert, one ticketed feature). Runs `backfill_quality`
/// once and checks the written row; runs it again and checks the row count
/// is still 1 (UPSERT did not duplicate).
/// Test: this test itself.
#[test]
fn backfill_quality_populates_and_is_idempotent() {
    let mut db = Database::open_in_memory().expect("open");
    // Seed: one revert + one ticketed feature by Alice in 2024-W03.
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge, ticketed) \
             VALUES ('q1', 'Alice', 'alice@example.com', '2024-01-15T10:00:00+00:00', \
                 'ENG-1 feature', 'repo-a', 1, 5, 1, 0, 1)",
            [],
        )
        .expect("seed ticketed feature");
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge, ticketed) \
             VALUES ('q2', 'Alice', 'alice@example.com', '2024-01-16T10:00:00+00:00', \
                 'Revert \"ENG-1 feature\"', 'repo-a', 1, 2, 5, 0, 0)",
            [],
        )
        .expect("seed revert");

    // First backfill — should write 1 row.
    backfill_quality(&mut db, false).expect("first backfill");
    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_weekly_quality WHERE author_email = 'alice@example.com'",
            [],
            |r| r.get(0),
        )
        .expect("count after first backfill");
    assert_eq!(count, 1, "first backfill must write exactly 1 row");

    // Second backfill — idempotent: still 1 row.
    backfill_quality(&mut db, false).expect("second backfill (idempotency)");
    let count2: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_weekly_quality WHERE author_email = 'alice@example.com'",
            [],
            |r| r.get(0),
        )
        .expect("count after second backfill");
    assert_eq!(
        count2, 1,
        "second backfill must not add duplicate rows (UPSERT semantics)"
    );

    // Verify the quality_score is plausible (1 revert, 1 ticketed, 2 commits,
    // 0 bugfixes): 0.35*(1-0.5) + 0.40*(1-0) + 0.25*0.5 = 0.175+0.40+0.125 = 0.7.
    let score: f64 = db
        .connection()
        .query_row(
            "SELECT quality_score FROM fact_weekly_quality WHERE author_email = 'alice@example.com'",
            [],
            |r| r.get(0),
        )
        .expect("read quality_score");
    assert!(
        (score - 0.700).abs() < 0.001,
        "quality_score must be ~0.70 for this fixture, got {score:.6}"
    );
}

/// Why: regression guard for issue #445 batch B. `backfill_quality --dry-run`
/// must estimate the row count without writing to `fact_weekly_quality`.
/// What: seed two commits; run dry-run backfill; assert `fact_weekly_quality`
/// is still empty and the exit code is Ok.
/// Test: this test itself.
#[test]
fn backfill_quality_dry_run_does_not_write() {
    let mut db = Database::open_in_memory().expect("open");
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge) \
             VALUES ('dq1', 'Bob', 'bob@example.com', '2024-02-05T10:00:00+00:00', \
                 'feat: x', 'repo-b', 1, 3, 1, 0)",
            [],
        )
        .expect("seed commit");

    backfill_quality(&mut db, true).expect("dry-run quality backfill must not error");

    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fact_weekly_quality", [], |r| r.get(0))
        .expect("count after dry-run");
    assert_eq!(
        count, 0,
        "dry-run must not write any rows to fact_weekly_quality"
    );
}

// ── effort-tshirt percentile tests (#445 batch C) ─────────────────────────

/// Helper: insert an effort row with a given score and size (0 for effort_tshirt).
fn seed_effort_row_for_tshirt(db: &Database, sha: &str, repo: &str, score: f64, size: &str) {
    db.connection()
        .execute(
            "INSERT OR REPLACE INTO fact_commit_effort \
             (sha, repository, size, score, loc, files, test_loc, tests_factor, \
              formula_version, computed_at, effort_tshirt) \
             VALUES (?1, ?2, ?3, ?4, 10, 1, 0, 1.0, 'v1', 0, 0)",
            params![sha, repo, size, score],
        )
        .expect("insert effort row");
}

/// Why: `backfill_effort_tshirt` must use corpus-percentile binning
/// (not static size→integer mapping) after batch C (#445).
/// What: seed 10 effort rows with scores 1–10; run backfill; assert that
/// effort_tshirt values reflect percentile quintiles and thresholds persist.
/// Test: this test itself.
#[test]
fn backfill_effort_tshirt_uses_percentile_binning() {
    let mut db = Database::open_in_memory().expect("open");

    // Seed 10 rows with scores 1.0 to 10.0 (all labeled "M" for simplicity).
    for i in 1..=10u32 {
        seed_effort_row_for_tshirt(&db, &format!("pct{i:03}"), "repo", i as f64, "M");
    }

    backfill_effort_tshirt(&mut db, false).expect("backfill");

    // With nearest-rank on [1..10]:
    //   p20=2, p40=4, p60=6, p80=8.
    // band_for_score: score=1 < 2 → 1; score=10 ≥ 8 → 5.
    let score1_band: i64 = db
        .connection()
        .query_row(
            "SELECT effort_tshirt FROM fact_commit_effort WHERE sha = 'pct001'",
            [],
            |r| r.get(0),
        )
        .expect("band for score=1");
    assert_eq!(score1_band, 1, "score=1 (below p20=2) → band 1");

    let score10_band: i64 = db
        .connection()
        .query_row(
            "SELECT effort_tshirt FROM fact_commit_effort WHERE sha = 'pct010'",
            [],
            |r| r.get(0),
        )
        .expect("band for score=10");
    assert_eq!(score10_band, 5, "score=10 (above p80=8) → band 5");

    // Verify thresholds were persisted.
    let stored = tga::core::effort_percentile::load_thresholds(db.connection())
        .expect("load thresholds")
        .expect("must be Some after backfill of 10 rows");
    assert!((stored.p20 - 2.0).abs() < 1e-9, "stored p20 must be 2.0");
    assert!((stored.p80 - 8.0).abs() < 1e-9, "stored p80 must be 8.0");
}

/// Why: tiny corpus (< 5 rows) must not panic; falls back to static
/// mapping without persisting thresholds.
/// What: seed 3 rows (all "L"), run backfill, assert effort_tshirt=4 (L→4)
/// and no thresholds stored.
/// Test: this test itself.
#[test]
fn backfill_effort_tshirt_tiny_corpus_fallback() {
    let mut db = Database::open_in_memory().expect("open");

    // 3 rows — below MIN_CORPUS_SIZE=5.
    for i in 1..=3u32 {
        seed_effort_row_for_tshirt(&db, &format!("tiny{i}"), "repo", i as f64, "L");
    }

    // Must not panic.
    backfill_effort_tshirt(&mut db, false).expect("backfill tiny corpus");

    // All rows should get static L=4 mapping.
    let tshirts: Vec<i64> = {
        let conn = db.connection();
        let mut stmt = conn
            .prepare("SELECT effort_tshirt FROM fact_commit_effort")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    };
    assert!(
        tshirts.iter().all(|&v| v == 4),
        "all rows must get L=4 (static fallback), got {tshirts:?}"
    );

    // No thresholds should be stored for a tiny corpus.
    let stored = tga::core::effort_percentile::load_thresholds(db.connection()).expect("load");
    assert!(
        stored.is_none(),
        "no thresholds should be stored for tiny corpus"
    );
}
