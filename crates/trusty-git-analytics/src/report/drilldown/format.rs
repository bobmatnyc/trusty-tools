//! Formatters for per-engineer drill-down reports.
//!
//! Why: separating Markdown and JSON rendering from the data model lets the
//! formatters be tested with synthetic data without a live database.
//! What: [`format_markdown`] renders an [`AuthorDrilldownData`] as human-readable
//! Markdown; [`format_json`] serialises it to pretty-printed JSON.
//! Test: see `tests::format_markdown_contains_headers` and `tests::format_json_parses`.

use crate::report::errors::Result;

use super::model::AuthorDrilldownData;

/// Render an [`AuthorDrilldownData`] as a Markdown report string.
///
/// Why: `tga author --format markdown` (the default) targets human readers;
/// the output mirrors the structure in spec §6 so automated doc generation
/// can consume it too.
/// What: produces the exact table structure from spec §6, including section
/// headers, the effort histogram with coverage fraction, and the PR metrics
/// table. Cycle-time fields show `—` when `None`; p95 shows `(< 20 PRs)`
/// when omitted due to insufficient sample.
/// Test: see `tests::format_markdown_contains_headers`.
pub fn format_markdown(data: &AuthorDrilldownData) -> String {
    let mut out = String::new();

    // Header.
    let period_str = match (&data.period.since, &data.period.until) {
        (Some(s), Some(u)) => format!("{s} – {u}"),
        (Some(s), None) => format!("{s} – present"),
        (None, Some(u)) => format!("all history – {u}"),
        (None, None) => "all history".to_string(),
    };
    let generated_date = data.generated_at.get(..10).unwrap_or(&data.generated_at);
    out.push_str(&format!(
        "# Engineer Report: {} <{}>\n",
        data.name, data.email
    ));
    out.push_str(&format!(
        "Generated: {generated_date} | Period: {period_str}\n\n"
    ));

    // Summary table.
    out.push_str("## Summary\n");
    out.push_str("| Metric          | Value                     |\n");
    out.push_str("|-----------------|---------------------------|\n");
    out.push_str(&format!(
        "| Total commits   | {:<25} |\n",
        data.commits.total
    ));
    let repos_str = data.commits.repositories.join(", ");
    out.push_str(&format!(
        "| Repositories    | {:<25} |\n",
        if repos_str.is_empty() {
            "—".to_string()
        } else {
            repos_str
        }
    ));
    out.push_str(&format!(
        "| First commit    | {:<25} |\n",
        data.commits
            .first_commit
            .as_deref()
            .and_then(|s| s.get(..10))
            .unwrap_or("—")
    ));
    out.push_str(&format!(
        "| Last commit     | {:<25} |\n",
        data.commits
            .last_commit
            .as_deref()
            .and_then(|s| s.get(..10))
            .unwrap_or("—")
    ));
    let coverage_str = match (data.commits.total, data.commits.ticket_coverage) {
        (0, _) => "no commits in scope".to_string(),
        (total, Some(cov)) => {
            format!(
                "{} / {} ({:.0}%)",
                data.commits.ticketed,
                total,
                cov * 100.0
            )
        }
        (total, None) => format!("{} / {} (0%)", data.commits.ticketed, total),
    };
    out.push_str(&format!("| Ticket coverage | {:<25} |\n", coverage_str));
    out.push('\n');

    // Effort histogram.
    out.push_str(&format!(
        "## Effort Histogram ({} / {} commits scored)\n",
        data.effort.scored_commits, data.effort.total_commits
    ));
    out.push_str("| Size | Count | % scored |\n");
    out.push_str("|------|-------|----------|\n");
    let scored = data.effort.scored_commits as f64;
    for size in &["XS", "S", "M", "L", "XL"] {
        let count = data.effort.histogram.get(*size).copied().unwrap_or(0);
        let pct = if scored > 0.0 {
            format!("{:.0}%", f64::from(count) / scored * 100.0)
        } else {
            "—".to_string()
        };
        out.push_str(&format!("| {:<4} | {:>5} | {:>8} |\n", size, count, pct));
    }
    out.push('\n');

    // PR metrics.
    out.push_str("## Pull Request Metrics\n");
    out.push_str("| Metric             | Value     |\n");
    out.push_str("|--------------------|-----------||\n");
    out.push_str(&format!(
        "| Total PRs          | {:<9} |\n",
        data.pull_requests.total
    ));
    out.push_str(&format!(
        "| Merged PRs         | {:<9} |\n",
        data.pull_requests.merged
    ));
    let fmt_ct = |v: Option<f64>| -> String {
        v.map(|h| format!("{h:.1} h"))
            .unwrap_or_else(|| "—".to_string())
    };
    out.push_str(&format!(
        "| Avg cycle time     | {:<9} |\n",
        fmt_ct(data.pull_requests.avg_cycle_time_hours)
    ));
    out.push_str(&format!(
        "| Median cycle time  | {:<9} |\n",
        fmt_ct(data.pull_requests.median_cycle_time_hours)
    ));
    let p95_str = match data.pull_requests.p95_cycle_time_hours {
        Some(h) => format!("{h:.1} h"),
        None if data.pull_requests.merged < 20 => "(< 20 PRs)".to_string(),
        None => "—".to_string(),
    };
    out.push_str(&format!("| p95 cycle time     | {:<9} |\n", p95_str));

    if data.pull_requests.total == 0 {
        out.push_str(
            "\n> No pull requests found. Ensure provider logins are mapped via \
                      `tga aliases add-login`.\n",
        );
    }
    out.push('\n');

    // Category breakdown.
    if !data.categories.is_empty() {
        out.push_str("## Category Breakdown\n");
        out.push_str("| Category    | Commits | % total |\n");
        out.push_str("|-------------|---------|--------|\n");
        let total_cats: usize = data.categories.values().sum();
        let mut cats: Vec<(&String, &usize)> = data.categories.iter().collect();
        cats.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (cat, count) in cats {
            let pct = if total_cats > 0 {
                format!("{:.0}%", *count as f64 / total_cats as f64 * 100.0)
            } else {
                "—".to_string()
            };
            out.push_str(&format!("| {:<11} | {:>7} | {:>7} |\n", cat, count, pct));
        }
        out.push('\n');
    }

    out
}

/// Render an [`AuthorDrilldownData`] as a JSON string.
///
/// Why: `tga author --format json` targets programmatic consumers (CI
/// dashboards, team tooling) that need structured, machine-readable output.
/// What: serialises the struct to pretty-printed JSON via serde. All
/// `Option<f64>` fields render as JSON `null` when absent.
/// Test: see `tests::format_json_parses`.
pub fn format_json(data: &AuthorDrilldownData) -> Result<String> {
    serde_json::to_string_pretty(data).map_err(crate::report::errors::ReportError::Json)
}
