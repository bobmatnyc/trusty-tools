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
#[derive(Args, Debug)]
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

/// Run the `tga pr-metrics` subcommand.
///
/// # Errors
///
/// Returns any error surfaced by the database query, CSV writer, or
/// filesystem (when writing to `--output`).
pub fn run(_config: Config, db: &Database, args: PrMetricsArgs) -> anyhow::Result<()> {
    let since_cutoff: Option<DateTime<Utc>> = args
        .weeks
        .map(|w| Utc::now() - Duration::weeks(i64::from(w)));

    let metrics = aggregate(db, since_cutoff)?;

    if args.csv {
        write_csv(&metrics, args.output.as_deref())?;
    } else if let Some(path) = args.output.as_deref() {
        // Non-CSV with --output: write the aligned table to a file as well.
        let rendered = render_table(&metrics);
        std::fs::write(path, rendered)?;
        println!("Wrote PR metrics table to {}", path.display());
    } else {
        print!("{}", render_table(&metrics));
    }
    Ok(())
}

/// Query the database and aggregate per-author metrics.
fn aggregate(
    db: &Database,
    since_cutoff: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<EngineerMetrics>> {
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
    for r in rows {
        let (author, state, created_at, merged_at) = r?;
        if author.is_empty() {
            continue;
        }
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
    Ok(out)
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

    #[test]
    fn aggregate_groups_by_author() {
        let db = seed_db();
        let metrics = aggregate(&db, None).expect("aggregate");
        assert_eq!(metrics.len(), 2);
        let alice = metrics.iter().find(|m| m.author == "alice").unwrap();
        assert_eq!(alice.prs_opened, 2);
        assert_eq!(alice.prs_merged, 1);
        assert_eq!(alice.cycle_time_samples, 1);
        assert!(alice.avg_cycle_time_hours() > 0.0);

        let bob = metrics.iter().find(|m| m.author == "bob").unwrap();
        assert_eq!(bob.prs_opened, 2);
        assert_eq!(bob.prs_merged, 1);
    }

    #[test]
    fn render_table_includes_headers_and_rows() {
        let db = seed_db();
        let metrics = aggregate(&db, None).expect("aggregate");
        let table = render_table(&metrics);
        assert!(table.contains("author"));
        assert!(table.contains("alice"));
        assert!(table.contains("bob"));
    }
}
