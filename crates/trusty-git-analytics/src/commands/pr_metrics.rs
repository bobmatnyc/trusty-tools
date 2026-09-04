//! `tga pr-metrics` — aggregate pull-request metrics per engineer.
//!
//! Reads the `pull_requests` table (the on-disk PR cache populated by the
//! GitHub collector) and aggregates a small set of per-author metrics:
//!
//! | Metric                  | Source                                        |
//! |-------------------------|-----------------------------------------------|
//! | `prs_opened`            | count(*)                                      |
//! | `prs_merged`            | count(state = 'merged')                       |
//! | `pr_comments_given`     | unavailable in current schema, reported as 0  |
//! | `merge_rate`            | prs_merged / prs_opened                       |
//! | `avg_cycle_time_hours`  | mean(merged_at - created_at) over merged PRs  |
//! | `avg_revisions`         | unavailable in current schema, reported as 0  |
//!
//! Metrics flagged "unavailable" are zero-filled until a future migration
//! adds the underlying review-comment / commit-count columns. The CLI shape
//! is stable so adding those fields later is a non-breaking change.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use clap::Args;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use tga::core::config::Config;
use tga::core::db::Database;

/// Arguments for `tga pr-metrics`.
///
/// Note: the `pr_comments_given` and `avg_revisions` columns in the report
/// are reserved for future use. The underlying review-comment and
/// revision-count data is not yet tracked, so those columns currently
/// always report `0.0`. The CLI shape is stable, so populating them later
/// is a non-breaking change.
// #5217: `Default` is what lets `audit::run_full_sweep` build this without clap.
#[derive(Args, Debug, Default)]
#[command(
    about = "Aggregate pull-request metrics per engineer.",
    long_about = "Read the pull_requests table (populated during `tga collect`) and emit\n\
per-author metrics: PRs opened, merged, merge rate, and average cycle time.\n\n\
Note: pr_comments_given and avg_revisions are always 0 until a future schema\n\
migration adds the underlying review-comment and revision-count columns.\n\
The CSV shape is stable, so adding those columns later is non-breaking.",
    after_help = "EXAMPLES:\n\
  # Print an aligned text table for all time\n\
  tga pr-metrics\n\n\
  # Limit to PRs opened in the last 8 weeks\n\
  tga pr-metrics --weeks 8\n\n\
  # Emit CSV to a file for further analysis\n\
  tga pr-metrics --csv --output pr-metrics.csv\n\n\
TIPS:\n\
  - Run `tga collect` first to populate the pull_requests table.\n\
  - Combine with `tga report` for full author-level productivity data."
)]
pub struct PrMetricsArgs {
    /// Limit metrics to PRs created within the last N weeks.
    #[arg(long, value_name = "N")]
    pub weeks: Option<u32>,

    /// Emit CSV instead of an aligned text table.
    #[arg(long, default_value_t = false)]
    pub csv: bool,

    /// Output file path (CSV only). When `--csv` is set without `--output`,
    /// CSV is written to stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

/// One row of the aggregated per-engineer metrics report.
#[derive(Debug, Default, Clone)]
struct EngineerMetrics {
    author: String,
    prs_opened: u64,
    prs_merged: u64,
    pr_comments_given: u64,
    cycle_time_hours_total: f64,
    cycle_time_samples: u64,
    revisions_total: u64,
    revisions_samples: u64,
}

impl EngineerMetrics {
    fn merge_rate(&self) -> f64 {
        if self.prs_opened == 0 {
            0.0
        } else {
            (self.prs_merged as f64) / (self.prs_opened as f64)
        }
    }

    fn avg_cycle_time_hours(&self) -> f64 {
        if self.cycle_time_samples == 0 {
            0.0
        } else {
            self.cycle_time_hours_total / (self.cycle_time_samples as f64)
        }
    }

    fn avg_revisions(&self) -> f64 {
        if self.revisions_samples == 0 {
            0.0
        } else {
            (self.revisions_total as f64) / (self.revisions_samples as f64)
        }
    }
}

/// The author bucket a pull request with no recorded login is counted under.
///
/// Why: #6796. GitHub answers with a null `user` for a deleted account and the
/// collector stores that as an empty string, so the aggregation skipped those rows
/// and lost the pull requests from every total with nothing on the record saying
/// so. A named bucket keeps the PR in the counts; only attribution to a person was
/// ever recoverable, and that is what is lost.
/// What: a sentinel that cannot collide with a real login — GitHub logins are
/// alphanumeric plus hyphen, so no login contains a parenthesis.
/// Test: `tests::a_pr_with_no_recorded_author_is_counted_not_dropped`.
const UNKNOWN_AUTHOR: &str = "(unknown)";

/// What one aggregation pass read, not only what it produced.
///
/// Why: #6796. A bare `Vec<EngineerMetrics>` cannot tell "this repository has no
/// pull requests" from "rows were read and none of them aggregated", and both
/// render as the same header-only CSV. The row count is what separates them.
/// What: the per-author rows plus how many `pull_requests` rows the query read.
/// Test: `tests::aggregate_reports_how_many_rows_it_read`.
#[derive(Debug, Default)]
struct Aggregation {
    rows: Vec<EngineerMetrics>,
    prs_examined: u64,
}

/// Run the `tga pr-metrics` subcommand.
///
/// # Errors
///
/// Returns any error surfaced by the database query, CSV writer, or filesystem
/// (when writing to `--output`) and, since #6796, an error when the aggregation
/// produced no rows at all. The artifact is written FIRST in that case, so a
/// packaging step that expects the file still finds it; the error is what turns a
/// header-only CSV from a silent artifact into a recorded gap.
/// [`crate::audit::run_full_sweep`] records a stage failure rather than aborting,
/// so the gap reaches the report's Gaps & Caveats and the run continues.
///
/// Test: `tests::{zero_rows_is_an_error_not_a_silent_empty_csv,
/// an_empty_table_and_an_excluding_window_read_differently}`.
pub fn run(_config: Config, db: &Database, args: PrMetricsArgs) -> anyhow::Result<()> {
    let since_cutoff: Option<DateTime<Utc>> = args
        .weeks
        .map(|w| Utc::now() - Duration::weeks(i64::from(w)));

    let aggregation = aggregate(db, since_cutoff)?;
    let metrics = aggregation.rows.as_slice();

    if args.csv {
        write_csv(metrics, args.output.as_deref())?;
    } else if let Some(path) = args.output.as_deref() {
        // Non-CSV with --output: write the aligned table to a file as well.
        let rendered = render_table(metrics);
        std::fs::write(path, rendered)?;
        println!("Wrote PR metrics table to {}", path.display());
    } else {
        print!("{}", render_table(metrics));
    }
    // #6796: a header-only artifact was the loudest thing this stage said, and on
    // its own it says nothing — 59 repositories of one engagement shipped exactly
    // that and the run still reported success.
    if metrics.is_empty() {
        anyhow::bail!(empty_result_reason(&aggregation, since_cutoff));
    }
    Ok(())
}

/// Why this run produced no metric rows, in words an audit reader can act on.
///
/// Why: #6796. The ways to get here have different remedies. An empty
/// `pull_requests` table means collection never stored a PR — `fetch_prs` left at
/// its default (#211), or no non-interactive git credential (#6244). A lookback
/// window that excluded every stored PR is an operator choice. Naming which one
/// happened is the difference between an actionable gap line and a shrug.
/// What: one sentence per case, quoting the row count the query actually read.
/// Test: `tests::an_empty_table_and_an_excluding_window_read_differently`.
fn empty_result_reason(aggregation: &Aggregation, since_cutoff: Option<DateTime<Utc>>) -> String {
    match (aggregation.prs_examined, since_cutoff) {
        (0, Some(cutoff)) => format!(
            "pr-metrics produced no rows: no pull_requests row is dated on or after \
             {cutoff}, so no review-quality data covers the requested window. Widen \
             --weeks, or re-run `tga collect` (issue #6796)"
        ),
        (0, None) => "pr-metrics produced no rows: the pull_requests table is empty, so no \
             review-quality data was collected for this repository. Set \
             `github.fetch_prs: true` (issue #211) and make sure a non-interactive git \
             credential is available (issue #6244), then re-run `tga collect` (issue #6796)"
            .to_owned(),
        (examined, _) => format!(
            "pr-metrics read {examined} pull_requests row(s) and aggregated none of them \
             (issue #6796)"
        ),
    }
}

/// Query the database and aggregate per-author metrics.
///
/// #6796: reports how many rows it READ alongside the rows it produced, and counts
/// a pull request whose author login is empty under [`UNKNOWN_AUTHOR`] instead of
/// skipping it.
fn aggregate(db: &Database, since_cutoff: Option<DateTime<Utc>>) -> anyhow::Result<Aggregation> {
    let conn = db.connection();

    // Build the query and its bound parameters in one place. The only
    // difference between the cutoff and no-cutoff cases is a single `WHERE`
    // clause and one bound parameter, so the row-processing loop is shared.
    let (sql, sql_params): (&str, Vec<Value>) = match since_cutoff {
        Some(cutoff) => (
            "SELECT author, state, created_at, merged_at \
             FROM pull_requests WHERE created_at >= ?1",
            vec![Value::Text(cutoff.to_rfc3339())],
        ),
        None => (
            "SELECT author, state, created_at, merged_at FROM pull_requests",
            Vec::new(),
        ),
    };

    let mut stmt = conn.prepare(sql)?;
    let mut by_author: std::collections::BTreeMap<String, EngineerMetrics> =
        std::collections::BTreeMap::new();

    let rows = stmt.query_map(params_from_iter(sql_params.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut prs_examined = 0u64;
    for r in rows {
        let (author, state, created_at, merged_at) = r?;
        prs_examined += 1;
        // #6796: a deleted provider account arrives as an empty login; dropping the
        // row lost the pull request from every total, so it is bucketed instead.
        let author = if author.is_empty() {
            UNKNOWN_AUTHOR.to_owned()
        } else {
            author
        };
        let entry = by_author
            .entry(author.clone())
            .or_insert_with(|| EngineerMetrics {
                author,
                ..Default::default()
            });
        entry.prs_opened += 1;
        if state == "merged" {
            entry.prs_merged += 1;
        }
        if let (Ok(created), Some(merged)) = (
            DateTime::parse_from_rfc3339(&created_at),
            merged_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok()),
        ) {
            let dur = merged.signed_duration_since(created);
            let hours = dur.num_seconds() as f64 / 3600.0;
            if hours >= 0.0 {
                entry.cycle_time_hours_total += hours;
                entry.cycle_time_samples += 1;
            }
        }
    }

    let mut out: Vec<EngineerMetrics> = by_author.into_values().collect();
    // Sort by prs_opened descending for stable, useful output.
    out.sort_by(|a, b| {
        b.prs_opened
            .cmp(&a.prs_opened)
            .then_with(|| a.author.cmp(&b.author))
    });
    Ok(Aggregation {
        rows: out,
        prs_examined,
    })
}

/// Render the metrics as a plain aligned ASCII table.
fn render_table(metrics: &[EngineerMetrics]) -> String {
    let headers = [
        "author",
        "prs_opened",
        "prs_merged",
        "pr_comments_given",
        "merge_rate",
        "avg_cycle_time_hours",
        "avg_revisions",
    ];
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(metrics.len() + 1);
    rows.push(headers.iter().map(|s| (*s).to_string()).collect());
    for m in metrics {
        rows.push(vec![
            m.author.clone(),
            m.prs_opened.to_string(),
            m.prs_merged.to_string(),
            m.pr_comments_given.to_string(),
            format!("{:.2}", m.merge_rate()),
            format!("{:.1}", m.avg_cycle_time_hours()),
            format!("{:.1}", m.avg_revisions()),
        ]);
    }

    // Compute column widths.
    let ncols = headers.len();
    let mut widths = vec![0usize; ncols];
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    let mut out = String::new();
    for (idx, row) in rows.iter().enumerate() {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(&format!("{:width$}", cell, width = widths[i]));
        }
        out.push('\n');
        if idx == 0 {
            // Separator under header.
            for (i, w) in widths.iter().enumerate() {
                if i > 0 {
                    out.push_str("  ");
                }
                out.push_str(&"-".repeat(*w));
            }
            out.push('\n');
        }
    }
    if metrics.is_empty() {
        out.push_str("(no pull requests found)\n");
    }
    out
}

/// Write the metrics as CSV to either a file or stdout.
fn write_csv(metrics: &[EngineerMetrics], path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let mut wtr: csv::Writer<Box<dyn std::io::Write>> = match path {
        Some(p) => csv::Writer::from_writer(Box::new(std::fs::File::create(p)?)),
        None => csv::Writer::from_writer(Box::new(std::io::stdout())),
    };
    wtr.write_record([
        "author",
        "prs_opened",
        "prs_merged",
        "pr_comments_given",
        "merge_rate",
        "avg_cycle_time_hours",
        "avg_revisions",
    ])?;
    for m in metrics {
        wtr.write_record([
            m.author.as_str(),
            &m.prs_opened.to_string(),
            &m.prs_merged.to_string(),
            &m.pr_comments_given.to_string(),
            &format!("{:.4}", m.merge_rate()),
            &format!("{:.2}", m.avg_cycle_time_hours()),
            &format!("{:.2}", m.avg_revisions()),
        ])?;
    }
    wtr.flush()?;
    if let Some(p) = path {
        println!("Wrote PR metrics CSV to {}", p.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn seed_db() -> Database {
        let db = Database::open_in_memory().expect("open");
        let conn = db.connection();
        let now = Utc::now();
        let earlier = now - Duration::hours(24);
        let rows = [
            ("alice", "merged", earlier, Some(now)),
            ("alice", "open", earlier, None::<DateTime<Utc>>),
            ("bob", "closed", earlier, None),
            ("bob", "merged", earlier, Some(now)),
        ];
        for (i, (author, state, created, merged)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO pull_requests (pr_number, title, author, state, created_at, merged_at, commit_shas) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, '[]')",
                params![
                    i as i64 + 1,
                    "t",
                    *author,
                    *state,
                    created.to_rfc3339(),
                    merged.map(|t| t.to_rfc3339()),
                ],
            )
            .expect("insert");
        }
        db
    }

    /// Insert one PR with the given author login, so a test can seed a ghost row.
    fn insert_pr(db: &Database, author: &str, pr_number: i64) {
        let now = Utc::now();
        db.connection()
            .execute(
                "INSERT INTO pull_requests (pr_number, title, author, state, created_at, merged_at, commit_shas) \
                 VALUES (?1, 't', ?2, 'merged', ?3, ?4, '[]')",
                params![
                    pr_number,
                    author,
                    (now - Duration::hours(5)).to_rfc3339(),
                    now.to_rfc3339(),
                ],
            )
            .expect("insert");
    }

    #[test]
    fn aggregate_groups_by_author() {
        let db = seed_db();
        let metrics = aggregate(&db, None).expect("aggregate").rows;
        assert_eq!(metrics.len(), 2);
        let alice = metrics
            .iter()
            .find(|m| m.author == "alice")
            .expect("alice present");
        assert_eq!(alice.prs_opened, 2);
        assert_eq!(alice.prs_merged, 1);
        assert_eq!(alice.cycle_time_samples, 1);
        assert!(alice.avg_cycle_time_hours() > 0.0);

        let bob = metrics
            .iter()
            .find(|m| m.author == "bob")
            .expect("bob present");
        assert_eq!(bob.prs_opened, 2);
        assert_eq!(bob.prs_merged, 1);
    }

    #[test]
    fn render_table_includes_headers_and_rows() {
        let db = seed_db();
        let metrics = aggregate(&db, None).expect("aggregate").rows;
        let table = render_table(&metrics);
        assert!(table.contains("author"));
        assert!(table.contains("alice"));
        assert!(table.contains("bob"));
    }

    /// #6796: the aggregation reports the rows it read, so an empty table and a
    /// window that excluded everything are distinguishable.
    #[test]
    fn aggregate_reports_how_many_rows_it_read() {
        let db = seed_db();
        let agg = aggregate(&db, None).expect("aggregate");
        assert_eq!(agg.prs_examined, 4, "four seeded rows were read");

        let empty = Database::open_in_memory().expect("open");
        let agg = aggregate(&empty, None).expect("aggregate");
        assert_eq!(agg.prs_examined, 0);
        assert!(agg.rows.is_empty());
    }

    /// #6796: a pull request whose author login is empty — GitHub's answer for a
    /// deleted account — used to be skipped, so the PR vanished from every total.
    /// Before the fix this test sees an empty result set.
    #[test]
    fn a_pr_with_no_recorded_author_is_counted_not_dropped() {
        let db = Database::open_in_memory().expect("open");
        insert_pr(&db, "", 1);
        insert_pr(&db, "", 2);

        let agg = aggregate(&db, None).expect("aggregate");
        assert_eq!(agg.prs_examined, 2);
        assert_eq!(
            agg.rows.len(),
            1,
            "both ghost-author PRs belong to one bucket, got {:?}",
            agg.rows
        );
        let bucket = &agg.rows[0];
        assert_eq!(bucket.author, UNKNOWN_AUTHOR);
        assert_eq!(bucket.prs_opened, 2);
        assert_eq!(bucket.prs_merged, 2);
    }

    /// #6796: the reported shape of the bug — every repository of a 59-repo bundle
    /// shipped a header-only `pr-metrics.csv` and the run reported success. The file
    /// is still written, and the run no longer returns `Ok`.
    #[test]
    fn zero_rows_is_an_error_not_a_silent_empty_csv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("pr-metrics.csv");
        let db = Database::open_in_memory().expect("open");

        let err = run(
            Config::default(),
            &db,
            PrMetricsArgs {
                weeks: None,
                csv: true,
                output: Some(out.clone()),
            },
        )
        .expect_err("a header-only artifact must not report success");

        assert!(
            out.is_file(),
            "the artifact must still be written so packaging finds it"
        );
        let written = std::fs::read_to_string(&out).expect("read csv");
        assert!(
            written.starts_with("author,"),
            "header present: {written:?}"
        );
        assert_eq!(
            written.lines().count(),
            1,
            "header only, which is exactly what must now be loud"
        );
        let msg = err.to_string();
        assert!(msg.contains("pull_requests table is empty"), "{msg}");
        assert!(msg.contains("#211"), "names the fetch_prs cause: {msg}");
        assert!(msg.contains("#6244"), "names the credential cause: {msg}");
    }

    /// #6796: an empty table and a lookback window that excluded every stored PR
    /// have different remedies, so they get different messages.
    #[test]
    fn an_empty_table_and_an_excluding_window_read_differently() {
        let empty = Aggregation::default();
        let no_window = empty_result_reason(&empty, None);
        assert!(
            no_window.contains("pull_requests table is empty"),
            "{no_window}"
        );

        let windowed = empty_result_reason(&empty, Some(Utc::now()));
        assert!(windowed.contains("--weeks"), "{windowed}");

        let read_but_empty = Aggregation {
            rows: Vec::new(),
            prs_examined: 7,
        };
        let unusable = empty_result_reason(&read_but_empty, None);
        assert!(
            unusable.contains("read 7 pull_requests row(s)"),
            "{unusable}"
        );
    }

    /// #6796: a run that DOES produce rows still returns `Ok` — the new refusal is
    /// scoped to the empty case and does not fail every audit sweep.
    #[test]
    fn a_populated_run_still_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("pr-metrics.csv");
        let db = seed_db();

        run(
            Config::default(),
            &db,
            PrMetricsArgs {
                weeks: None,
                csv: true,
                output: Some(out.clone()),
            },
        )
        .expect("a populated run succeeds");

        let written = std::fs::read_to_string(&out).expect("read csv");
        assert_eq!(written.lines().count(), 3, "header plus two authors");
    }
}
