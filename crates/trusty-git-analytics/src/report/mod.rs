//! Stage 3 of the pipeline: read classified commits from a SQLite database
//! and generate CSV, JSON, and Markdown reports.
//!
//! ## Submodules
//!
//! - [`aggregator`] — DB → in-memory [`ReportData`]
//! - [`formatters`] — CSV / JSON / Markdown output
//! - [`templates`] — embedded Tera template strings
//! - [`pipeline`] — [`ReportPipeline`] orchestrator
//! - [`errors`] — [`ReportError`] / [`Result`]
//! - [`models`] — aggregated data structures
//! - [`period_trends`] — N-week period roll-up for contributor profiles (#558)

pub mod aggregator;
pub mod drilldown;
pub mod errors;
pub mod formatters;
pub mod models;
pub mod period_trends;
pub mod pipeline;
pub mod templates;
pub mod ticketed_stats;

pub use errors::{ReportError, Result};
pub use models::ReportData;
pub use period_trends::{query_author_period_trends, AuthorPeriodSummary};
pub use pipeline::{ReportPipeline, ReportStats};
pub use ticketed_stats::{compute_ticketed_stats, TicketedStats};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::core::config::{Config, OutputConfig, RepositoryConfig};
    use crate::core::db::Database;

    use super::aggregator::Aggregator;
    use super::formatters::{csv as csv_fmt, json as json_fmt, markdown as md_fmt};
    use super::pipeline::ReportPipeline;

    /// Seed an in-memory DB with two authors, two commits, and one classification.
    fn seed_db() -> Database {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        conn.execute(
            "INSERT INTO classifications (id, category, subcategory, ticket_id, confidence, method) \
             VALUES (1, 'feature', NULL, NULL, 0.9, 'exact_rule')",
            [],
        )
        .expect("insert classification");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, classification_id, confidence, is_merge) \
             VALUES ('aaa111', 'Alice', 'alice@example.com', '2024-01-15T10:00:00+00:00', \
                 'feat: add login', 'repo-a', 3, 50, 5, 1, 0.9, 0)",
            [],
        )
        .expect("insert commit 1");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, classification_id, confidence, is_merge) \
             VALUES ('bbb222', 'Bob', 'bob@example.com', '2024-01-22T11:00:00+00:00', \
                 'fix: edge case', 'repo-a', 1, 10, 2, NULL, NULL, 0)",
            [],
        )
        .expect("insert commit 2");

        db
    }

    fn baseline_config() -> Config {
        Config {
            repositories: vec![RepositoryConfig {
                path: PathBuf::from("/tmp/repo-a"),
                name: Some("repo-a".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn aggregator_builds_report_data() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        assert_eq!(data.total_commits, 2);
        assert_eq!(data.total_authors, 2);
        assert!(data.period_start.is_some());
        assert!(data.period_end.is_some());
        assert_eq!(data.repositories.len(), 1);
        assert_eq!(data.repositories[0].name, "repo-a");
        assert_eq!(data.repositories[0].author_count, 2);
        assert_eq!(data.category_breakdown.get("feature").copied(), Some(1));
        // Two weekly buckets — one per author (different weeks).
        assert_eq!(data.weekly_activity.len(), 2);
    }

    #[test]
    fn aggregator_handles_empty_db() {
        let db = Database::open_in_memory().expect("open db");
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");
        assert_eq!(data.total_commits, 0);
        assert_eq!(data.total_authors, 0);
        assert!(data.period_start.is_none());
    }

    fn tmp_dir(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "tga-report-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("mkdir");
        path
    }

    #[test]
    fn csv_formatter_writes_files_with_headers() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("csv");
        let authors_path = csv_fmt::write_author_csv(&data, &dir).expect("write authors");
        let weekly_path = csv_fmt::write_weekly_csv(&data, &dir).expect("write weekly");

        let authors_text = std::fs::read_to_string(&authors_path).expect("read");
        assert!(authors_text.starts_with("name,email,commit_count"));
        assert!(authors_text.contains("Alice"));
        assert!(authors_text.contains("Bob"));

        let weekly_text = std::fs::read_to_string(&weekly_path).expect("read");
        assert!(weekly_text.starts_with("week,author,repository"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_formatter_writes_valid_json() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("json");
        let path = json_fmt::write_json(&data, &dir).expect("write json");
        let text = std::fs::read_to_string(&path).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(parsed["total_commits"], 2);
        assert_eq!(parsed["total_authors"], 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn markdown_formatter_emits_report_header() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("md");
        let path = md_fmt::write_markdown(&data, &dir).expect("write md");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("# Git Activity Report"));
        assert!(text.contains("Alice"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pipeline_constructs_without_panic() {
        let cfg = baseline_config();
        let _pipeline = ReportPipeline::new(cfg);
    }

    #[test]
    fn aggregator_computes_summary_and_dora_and_quality() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let summary = data.summary.as_ref().expect("summary present");
        assert_eq!(summary.total_commits, 2);
        assert_eq!(summary.total_developers, 2);
        assert!(summary.total_weeks >= 1);
        // One of two commits is classified → 50% coverage.
        assert!((summary.classification_coverage_pct - 50.0).abs() < 1e-6);

        let dora = data.dora.as_ref().expect("dora present");
        let lvl = dora.performance_level.as_str();
        assert!(
            matches!(lvl, "elite" | "high" | "medium" | "low"),
            "unexpected performance_level: {lvl}"
        );

        let quality = data.quality.as_ref().expect("quality present");
        assert!(quality.quality_score >= 0.0 && quality.quality_score <= 1.0);

        let velocity = data.velocity.as_ref().expect("velocity present");
        assert_eq!(velocity.pr_count, 0);
    }

    #[test]
    fn aggregator_produces_developer_activity_with_score_ordering() {
        // Seed two authors with different commit counts so we can assert the
        // higher-commit author also gets the higher activity score.
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        for i in 0..5 {
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                     files_changed, insertions, deletions, is_merge) \
                 VALUES (?1, 'Alice', 'alice@example.com', '2024-01-15T10:00:00+00:00', \
                     'feat: change', 'repo-a', 1, 10, 1, 0)",
                [format!("a{i}")],
            )
            .expect("seed alice");
        }
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, is_merge) \
             VALUES ('b1', 'Bob', 'bob@example.com', '2024-01-22T10:00:00+00:00', \
                 'feat: y', 'repo-a', 1, 1, 1, 0)",
            [],
        )
        .expect("seed bob");

        let data = Aggregator::build(&db, &baseline_config()).expect("aggregate");
        let alice = data
            .developer_activity
            .iter()
            .find(|d| d.developer_id == "alice@example.com")
            .expect("alice present");
        let bob = data
            .developer_activity
            .iter()
            .find(|d| d.developer_id == "bob@example.com")
            .expect("bob present");
        assert_eq!(alice.total_commits, 5);
        assert_eq!(bob.total_commits, 1);
        assert!(
            alice.activity_score > bob.activity_score,
            "alice ({:.4}) should outrank bob ({:.4})",
            alice.activity_score,
            bob.activity_score
        );
    }

    #[test]
    fn csv_formatter_writes_new_report_files() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("csv-new");
        let summary = csv_fmt::write_summary_csv(&data, &dir).expect("write summary");
        let weekly_metrics =
            csv_fmt::write_weekly_metrics_csv(&data, &dir).expect("write weekly metrics");
        let dev_activity =
            csv_fmt::write_developer_activity_csv(&data, &dir).expect("write dev activity");
        let untracked = csv_fmt::write_untracked_csv(&data, &dir).expect("write untracked");
        let weekly_cat = csv_fmt::write_weekly_categorization_csv(&data, &dir)
            .expect("write weekly categorization");
        let weekly_vel =
            csv_fmt::write_weekly_velocity_csv(&data, &dir).expect("write weekly velocity");
        let dora_csv = csv_fmt::write_weekly_dora_csv(&data, &dir).expect("write dora csv");

        for p in [
            &summary,
            &weekly_metrics,
            &dev_activity,
            &untracked,
            &weekly_cat,
            &weekly_vel,
            &dora_csv,
        ] {
            assert!(p.exists(), "{} should exist", p.display());
        }

        let summary_text = std::fs::read_to_string(&summary).expect("read summary");
        assert!(summary_text.starts_with("date_range,total_commits"));

        let dev_text = std::fs::read_to_string(&dev_activity).expect("read dev activity");
        assert!(dev_text.contains("activity_score"));
        assert!(dev_text.contains("Alice"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_formatter_writes_velocity_quality_dora() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("json-new");
        let velocity = json_fmt::write_velocity_json(&data, &dir).expect("write velocity");
        let quality = json_fmt::write_quality_json(&data, &dir).expect("write quality");
        let dora = json_fmt::write_dora_json(&data, &dir).expect("write dora");

        let velocity_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&velocity).expect("read"))
                .expect("velocity json");
        assert!(velocity_v.is_object());
        assert!(velocity_v["pr_count"].is_number());

        let quality_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&quality).expect("read"))
                .expect("quality json");
        assert!(quality_v["quality_score"].as_f64().unwrap() >= 0.0);

        let dora_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dora).expect("read"))
                .expect("dora json");
        assert!(dora_v["performance_level"].is_string());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pipeline_runs_all_formats_when_unspecified() {
        let db = seed_db();
        let dir = tmp_dir("pipeline");
        let cfg = Config {
            repositories: vec![RepositoryConfig {
                path: PathBuf::from("/tmp/repo-a"),
                name: Some("repo-a".into()),
                ..Default::default()
            }],
            output: Some(OutputConfig {
                directory: Some(dir.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pipeline = ReportPipeline::new(cfg);
        let stats = pipeline.run(&db).expect("run");
        assert_eq!(stats.total_commits, 2);
        assert_eq!(stats.total_authors, 2);
        // CSV (9 files) + JSON (4 files) + Markdown (1 file) = 14 files.
        assert_eq!(stats.files_written.len(), 14);
        for f in &stats.files_written {
            assert!(f.exists(), "{} should exist", f.display());
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // =========================================================================
    // Quality metric tests (issue #377)
    // =========================================================================

    /// Seed one engineer in a single week with a known mix of commit kinds so
    /// the per-week quality score is deterministic.
    fn seed_quality_db() -> Database {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        // bugfix classification (id=2) so a categorized bugfix exists.
        conn.execute(
            "INSERT INTO classifications (id, category, subcategory, ticket_id, confidence, method) \
             VALUES (2, 'bugfix', NULL, NULL, 0.9, 'exact_rule')",
            [],
        )
        .expect("insert bugfix classification");

        // Four commits by Carol in the same ISO week (2024-W03):
        //  1. plain ticketed feature  (ticketed=1)
        //  2. ticketed feature        (ticketed=1)
        //  3. a revert                (revert detected from message)
        //  4. a classified bugfix     (category=bugfix, not ticketed)
        let rows = [
            ("c1", "ENG-1 add feature", 1_i64, None::<i64>),
            ("c2", "ENG-2 more feature", 1, None),
            ("c3", "Revert \"ENG-3 bad change\"", 0, None),
            ("c4", "patch up edge case", 0, Some(2)),
        ];
        for (sha, msg, ticketed, cls) in rows {
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                     repository, files_changed, insertions, deletions, is_merge, ticketed, \
                     classification_id) \
                 VALUES (?1, 'Carol', 'carol@example.com', '2024-01-15T10:00:00+00:00', ?2, \
                     'repo-a', 1, 5, 1, 0, ?3, ?4)",
                rusqlite::params![sha, msg, ticketed, cls],
            )
            .expect("insert quality commit");
        }
        db
    }

    #[test]
    fn weekly_activity_carries_quality_columns() {
        let db = seed_quality_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        // One weekly bucket: (2024-W03, Carol, repo-a).
        assert_eq!(data.weekly_activity.len(), 1);
        let wa = &data.weekly_activity[0];
        assert_eq!(wa.commit_count, 4);
        assert_eq!(wa.revert_count, 1, "one revert commit");
        assert_eq!(wa.bugfix_count, 1, "one classified bugfix");
        assert_eq!(wa.ticketed_count, 2, "two ticketed commits");

        // Expected score with the v1 orientation:
        //   revert_rate = 1/4 = 0.25, bugfix_rate = 1/4 = 0.25, ticket = 2/4 = 0.5
        //   0.35*(1-0.25) + 0.40*(1-0.25) + 0.25*0.5
        //   = 0.2625 + 0.30 + 0.125 = 0.6875 ⇒ band 4.
        assert!(
            (wa.quality_score - 0.6875).abs() < 1e-9,
            "quality_score = {}",
            wa.quality_score
        );
        assert_eq!(wa.quality_tshirt, "4");
    }

    #[test]
    fn weekly_quality_perfect_when_clean_and_ticketed() {
        // All commits ticketed, no reverts, no bugfixes ⇒ score 1.0, tshirt 5.
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        for i in 0..3 {
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                     repository, files_changed, insertions, deletions, is_merge, ticketed) \
                 VALUES (?1, 'Dave', 'dave@example.com', '2024-02-05T10:00:00+00:00', \
                     'ENG-9 clean work', 'repo-a', 1, 3, 1, 0, 1)",
                [format!("d{i}")],
            )
            .expect("seed clean commit");
        }
        let data = Aggregator::build(&db, &baseline_config()).expect("aggregate");
        assert_eq!(data.weekly_activity.len(), 1);
        let wa = &data.weekly_activity[0];
        assert!(
            (wa.quality_score - 1.0).abs() < 1e-9,
            "{}",
            wa.quality_score
        );
        assert_eq!(wa.quality_tshirt, "5");
        assert_eq!(wa.revert_count, 0);
        assert_eq!(wa.bugfix_count, 0);
        assert_eq!(wa.ticketed_count, 3);
        assert_eq!(wa.abandoned_pr_count, 0);
    }

    #[test]
    fn aggregator_counts_abandoned_prs() {
        // Seed a commit so a weekly-activity row exists for the engineer, then
        // a closed-unmerged PR authored by the same identity in the same week.
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, is_merge, ticketed) \
             VALUES ('e1', 'eve', 'eve@example.com', '2024-01-15T10:00:00+00:00', \
                 'ENG-1 work', 'repo-a', 1, 5, 1, 0, 1)",
            [],
        )
        .expect("seed commit");
        // Abandoned PR: state=closed, merged_at NULL, author login = display name.
        conn.execute(
            "INSERT INTO pull_requests (pr_number, title, author, state, created_at, merged_at) \
             VALUES (10, 'wip', 'eve', 'closed', '2024-01-15T09:00:00+00:00', NULL)",
            [],
        )
        .expect("seed abandoned pr");
        // A merged PR by the same author must NOT count as abandoned.
        conn.execute(
            "INSERT INTO pull_requests (pr_number, title, author, state, created_at, merged_at) \
             VALUES (11, 'done', 'eve', 'merged', '2024-01-15T08:00:00+00:00', \
                 '2024-01-15T12:00:00+00:00')",
            [],
        )
        .expect("seed merged pr");

        let data = Aggregator::build(&db, &baseline_config()).expect("aggregate");
        assert_eq!(data.weekly_activity.len(), 1);
        assert_eq!(
            data.weekly_activity[0].abandoned_pr_count, 1,
            "exactly one closed-unmerged PR attributed to eve"
        );
    }

    #[test]
    fn weekly_csv_includes_quality_columns() {
        let db = seed_quality_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("weekly-quality-csv");
        let path = csv_fmt::write_weekly_csv(&data, &dir).expect("write weekly");
        let text = std::fs::read_to_string(&path).expect("read");
        let header = text.lines().next().expect("header line");
        for col in [
            "revert_count",
            "bugfix_count",
            "ticketed_count",
            "quality_score",
            "quality_tshirt",
            "abandoned_pr_count",
        ] {
            assert!(header.contains(col), "header missing {col}: {header}");
        }
        // The single data row should carry tshirt "4" (see the formula test).
        assert!(
            text.contains(",4,"),
            "expected quality_tshirt 4 in row: {text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_report_exposes_weekly_quality_fields() {
        // Why: the Duetto warehouse ingests the JSON report; the new quality
        // fields must round-trip through the full `ReportData` serialization,
        // not just the dedicated CSV.
        let db = seed_quality_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("json-quality");
        let path = json_fmt::write_json(&data, &dir).expect("write json");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
                .expect("valid json");
        let wa = &parsed["weekly_activity"][0];
        assert_eq!(wa["revert_count"], 1);
        assert_eq!(wa["bugfix_count"], 1);
        assert_eq!(wa["ticketed_count"], 2);
        assert_eq!(wa["quality_tshirt"], "4");
        assert!(wa["quality_score"].as_f64().expect("score is f64") > 0.68);
        assert_eq!(wa["abandoned_pr_count"], 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    // =========================================================================
    // Author filter tests (issue #324)
    // =========================================================================

    /// Seed a DB with two authors linked to the `authors` table (canonical
    /// identity rows) so that `resolve_canonical_email` / `load_rows_filtered`
    /// paths are fully exercised.
    fn seed_db_with_authors() -> Database {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // Insert canonical author rows.
        conn.execute(
            "INSERT INTO authors (id, canonical_name, canonical_email, aliases) \
             VALUES (1, 'Alice Smith', 'alice@example.com', '[]')",
            [],
        )
        .expect("insert author alice");
        conn.execute(
            "INSERT INTO authors (id, canonical_name, canonical_email, aliases) \
             VALUES (2, 'Bob Jones', 'bob@example.com', '[]')",
            [],
        )
        .expect("insert author bob");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, is_merge, author_id) \
             VALUES ('aaa111', 'Alice Smith', 'alice@example.com', '2024-01-15T10:00:00+00:00', \
                 'feat: add login', 'repo-a', 3, 50, 5, 0, 1)",
            [],
        )
        .expect("insert alice commit 1");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, is_merge, author_id) \
             VALUES ('aaa222', 'Alice Smith', 'alice@example.com', '2024-01-16T10:00:00+00:00', \
                 'feat: add logout', 'repo-a', 2, 20, 3, 0, 1)",
            [],
        )
        .expect("insert alice commit 2");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, is_merge, author_id) \
             VALUES ('bbb111', 'Bob Jones', 'bob@example.com', '2024-01-22T11:00:00+00:00', \
                 'fix: edge case', 'repo-a', 1, 10, 2, 0, 2)",
            [],
        )
        .expect("insert bob commit");

        db
    }

    #[test]
    fn aggregator_author_filter_returns_single_author() {
        // Why: validates that `build_filtered` with a known email returns only
        // that author's commits and excludes all others.
        let db = seed_db_with_authors();
        let cfg = baseline_config();

        let data = Aggregator::build_filtered(&db, &cfg, Some("alice@example.com"))
            .expect("build filtered");

        assert_eq!(
            data.total_commits, 2,
            "should contain only alice's 2 commits, got {}",
            data.total_commits
        );
        assert_eq!(
            data.total_authors, 1,
            "should contain only 1 author (alice), got {}",
            data.total_authors
        );
        assert_eq!(
            data.authors[0].email, "alice@example.com",
            "the sole author should be alice"
        );
    }

    #[test]
    fn aggregator_author_filter_case_insensitive() {
        // Why: git emails in the wild mix case; the filter must be
        // case-insensitive so `ALICE@EXAMPLE.COM` resolves the same as
        // `alice@example.com`.
        let db = seed_db_with_authors();
        let cfg = baseline_config();

        let data = Aggregator::build_filtered(&db, &cfg, Some("ALICE@EXAMPLE.COM"))
            .expect("case-insensitive filter");

        assert_eq!(data.total_commits, 2);
        assert_eq!(data.total_authors, 1);
    }

    #[test]
    fn aggregator_author_filter_unknown_email_errors() {
        // Why: a typo in the email should fail loudly with a suggestion, not
        // silently produce an empty report.
        let db = seed_db_with_authors();
        let cfg = baseline_config();

        let result = Aggregator::build_filtered(&db, &cfg, Some("nobody@example.com"));

        assert!(
            result.is_err(),
            "expected an error for unknown email, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("nobody@example.com"),
            "error should contain the supplied email; got: {msg}"
        );
        assert!(
            msg.contains("tga aliases list"),
            "error should suggest `tga aliases list`; got: {msg}"
        );
    }

    // =========================================================================
    // fact_weekly_quality persistence tests (issue #445 batch B, task 1)
    // =========================================================================

    /// Why: `persist_weekly_quality` must UPSERT the same values the aggregator
    /// computed into `fact_weekly_quality`, and running it twice must not
    /// duplicate rows (idempotent).
    /// What: seed the quality DB, build a report (which calls persist internally),
    /// then call `persist_weekly_quality` again; assert the row count is still 1
    /// and the values match the aggregator's weekly_activity output.
    /// Test: this test itself.
    #[test]
    fn persist_weekly_quality_upserts_rows_and_is_idempotent() {
        let db = seed_quality_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate (first — writes quality rows)");

        // The aggregator's build path already called persist internally.
        let wa = &data.weekly_activity[0];

        // Read back the persisted row.
        let (score, tshirt, rc, bc, tc, cc): (f64, i64, i64, i64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT quality_score, quality_tshirt, revert_count, bugfix_count, \
                 ticketed_count, commit_count FROM fact_weekly_quality \
                 WHERE author_email = 'carol@example.com'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .expect("read persisted quality row");

        // Values must match the aggregator's weekly_activity output.
        assert!(
            (score - wa.quality_score).abs() < 1e-9,
            "persisted quality_score {score} must match aggregator score {}",
            wa.quality_score
        );
        assert_eq!(tshirt, wa.quality_tshirt.parse::<i64>().unwrap_or(0));
        assert_eq!(rc, wa.revert_count as i64);
        assert_eq!(bc, wa.bugfix_count as i64);
        assert_eq!(tc, wa.ticketed_count as i64);
        assert_eq!(cc, wa.commit_count as i64);

        // Idempotency: call persist again; row count must still be 1.
        Aggregator::persist_weekly_quality(&db, &data).expect("second persist");
        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_weekly_quality WHERE author_email = 'carol@example.com'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "UPSERT must not duplicate the grain row");
    }

    // =========================================================================
    // avg_complexity tests (issue #445 batch B, task 2)
    // =========================================================================

    /// Why: `avg_complexity` must be `None` when no commit in the bucket has a
    /// non-null `classifications.complexity` value, and `Some(mean)` when at
    /// least one commit does. This validates both the all-null and mixed cases.
    /// What: seeds a classification with `complexity = 3`, links it to one
    /// commit; seeds a second commit with no complexity. Asserts avg = 3.0 (only
    /// non-null values average) and the all-null baseline returns None.
    /// Test: this test itself.
    #[test]
    fn avg_complexity_is_mean_of_non_null_values() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // Insert a classification with complexity = 3.
        conn.execute(
            "INSERT INTO classifications (id, category, subcategory, confidence, method, complexity) \
             VALUES (10, 'feature', NULL, 0.9, 'llm', 3)",
            [],
        )
        .expect("insert classification with complexity");
        // Insert a classification with complexity = 5.
        conn.execute(
            "INSERT INTO classifications (id, category, subcategory, confidence, method, complexity) \
             VALUES (11, 'feature', NULL, 0.9, 'llm', 5)",
            [],
        )
        .expect("insert classification complexity=5");

        // Three commits by Eve in 2024-W03: two with complexity, one without.
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge, classification_id) \
             VALUES ('e1', 'Eve', 'eve@complexity.example', '2024-01-15T10:00:00+00:00', \
                 'feat: x', 'repo-c', 1, 5, 1, 0, 10)",
            [],
        )
        .expect("commit with complexity=3");
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge, classification_id) \
             VALUES ('e2', 'Eve', 'eve@complexity.example', '2024-01-16T10:00:00+00:00', \
                 'feat: y', 'repo-c', 1, 3, 1, 0, 11)",
            [],
        )
        .expect("commit with complexity=5");
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge) \
             VALUES ('e3', 'Eve', 'eve@complexity.example', '2024-01-17T10:00:00+00:00', \
                 'chore: no complexity', 'repo-c', 1, 1, 1, 0)",
            [],
        )
        .expect("commit without complexity");

        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        // Find Eve's weekly activity row.
        let wa = data
            .weekly_activity
            .iter()
            .find(|r| r.author == "Eve" || r.author.contains("eve@complexity"))
            .expect("Eve's weekly row must exist");

        // Expected: avg of [3, 5] = 4.0 (null-complexity commit is excluded).
        assert_eq!(
            wa.avg_complexity,
            Some(4.0),
            "avg_complexity must be 4.0 (mean of 3 and 5)"
        );
    }

    /// Why: when ALL commits in a bucket have null complexity (e.g. all resolved
    /// by exact_rule), `avg_complexity` must be `None` — not `Some(0.0)`.
    /// What: uses `seed_db()` which has no complexity values; asserts None.
    /// Test: this test itself.
    #[test]
    fn avg_complexity_is_none_when_all_null() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        for wa in &data.weekly_activity {
            assert_eq!(
                wa.avg_complexity, None,
                "avg_complexity must be None when no classifications carry a complexity score"
            );
        }
    }

    /// Why: the weekly CSV must include the `avg_complexity` column so warehouse
    /// ingestion picks it up without schema changes.
    /// What: seeds a classification with complexity, builds report, writes CSV,
    /// asserts the header contains `avg_complexity` and a data row has a value.
    /// Test: this test itself.
    #[test]
    fn weekly_csv_includes_avg_complexity_column() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        conn.execute(
            "INSERT INTO classifications (id, category, subcategory, confidence, method, complexity) \
             VALUES (20, 'feature', NULL, 0.9, 'llm', 4)",
            [],
        )
        .expect("insert classification");
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, files_changed, insertions, deletions, is_merge, classification_id) \
             VALUES ('cx1', 'Frank', 'frank@example.com', '2024-01-15T10:00:00+00:00', \
                 'feat: z', 'repo-a', 1, 5, 1, 0, 20)",
            [],
        )
        .expect("insert commit");

        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("csv-complexity");
        let path = csv_fmt::write_weekly_csv(&data, &dir).expect("write weekly csv");
        let text = std::fs::read_to_string(&path).expect("read csv");
        let header = text.lines().next().expect("header");

        assert!(
            header.contains("avg_complexity"),
            "weekly CSV header must contain avg_complexity; got: {header}"
        );
        // The data row should have a non-empty avg_complexity (4.0000).
        assert!(
            text.contains("4.0000"),
            "CSV should contain avg_complexity = 4.0000 for Frank's row; got:\n{text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn aggregator_author_filter_none_returns_all() {
        // Why: omitting `--author` must behave identically to the pre-existing
        // `Aggregator::build` path — backwards compatibility is not regressed.
        let db = seed_db_with_authors();
        let cfg = baseline_config();

        let unfiltered = Aggregator::build(&db, &cfg).expect("unfiltered");
        let filtered_none = Aggregator::build_filtered(&db, &cfg, None).expect("filtered none");

        assert_eq!(unfiltered.total_commits, filtered_none.total_commits);
        assert_eq!(unfiltered.total_authors, filtered_none.total_authors);
    }

    #[test]
    fn pipeline_author_filter_single_author() {
        // Why: validates the full pipeline path (`run_with_filter`) so that
        // all formatters receive the scoped data without crashing.
        let db = seed_db_with_authors();
        let dir = tmp_dir("pipeline-author");
        let cfg = Config {
            repositories: vec![RepositoryConfig {
                path: PathBuf::from("/tmp/repo-a"),
                name: Some("repo-a".into()),
                ..Default::default()
            }],
            output: Some(OutputConfig {
                directory: Some(dir.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pipeline = ReportPipeline::new(cfg);
        let stats = pipeline
            .run_with_filter(&db, Some("alice@example.com"))
            .expect("run with filter");

        assert_eq!(stats.total_commits, 2, "filtered report: 2 alice commits");
        assert_eq!(stats.total_authors, 1, "filtered report: 1 author");
        // 14 files still written (formatters are unchanged).
        assert_eq!(stats.files_written.len(), 14);

        std::fs::remove_dir_all(&dir).ok();
    }
}
