//! Profile report rendering and file output.
//!
//! Why: a profile has two audiences — a program reading JSON and a person
//! reading Markdown — and rendering both from the same in-memory artefact keeps
//! them from drifting.
//! What: [`ReportFormat`] selects which files to write, [`Reporter`] writes
//! them, and [`render_markdown`] is the renderer, exposed on its own so a
//! caller can render without touching the filesystem.
//!
//! The GitHub issue upsert from the trusty-review original is deliberately
//! absent — it lands in #5465, which owns the GitHub write path.
//!
//! Test: `reporter_tests.rs`.

use std::path::PathBuf;

use tracing::info;

use super::types::{ContributorProfile, Trajectory, TrendTag};

// ─── Output format ────────────────────────────────────────────────────────────

/// Which report files [`Reporter::write_profile`] emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// Only the JSON artefact.
    Json,
    /// Only the Markdown report.
    Markdown,
    /// Both.
    Both,
}

impl std::str::FromStr for ReportFormat {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            "both" => Ok(Self::Both),
            other => Err(format!("unknown format: {other}")),
        }
    }
}

// ─── Reporter ─────────────────────────────────────────────────────────────────

/// Writes a [`ContributorProfile`] to disk in the configured format.
///
/// Why: keeping the I/O behind a small type lets [`render_markdown`] stay a
/// pure function that tests can call without a temporary directory.
/// What: holds the output directory and the format; `write_profile` creates the
/// directory if needed and returns the paths it wrote.
/// Test: `reporter_json_output`, `reporter_both_format_writes_two_files`.
pub struct Reporter {
    output_dir: PathBuf,
    format: ReportFormat,
}

impl Reporter {
    /// Create a reporter writing into `output_dir`.
    pub fn new(output_dir: impl Into<PathBuf>, format: ReportFormat) -> Self {
        Self {
            output_dir: output_dir.into(),
            format,
        }
    }

    /// Write the profile and return the paths written.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] when the directory cannot be created, a file cannot be
    /// written, or the profile fails to serialise.
    ///
    /// Test: `reporter_json_output`, `reporter_both_format_writes_two_files`.
    pub fn write_profile(
        &self,
        profile: &ContributorProfile,
    ) -> std::result::Result<Vec<PathBuf>, std::io::Error> {
        std::fs::create_dir_all(&self.output_dir)?;

        let mut written = Vec::new();
        let stem = profile_file_stem(profile);

        if matches!(self.format, ReportFormat::Json | ReportFormat::Both) {
            let json_path = self.output_dir.join(format!("{stem}.json"));
            let json = serde_json::to_string_pretty(profile)
                .map_err(|e| std::io::Error::other(format!("JSON serialise: {e}")))?;
            std::fs::write(&json_path, &json)?;
            info!(path = %json_path.display(), "profile JSON written");
            written.push(json_path);
        }

        if matches!(self.format, ReportFormat::Markdown | ReportFormat::Both) {
            let md_path = self.output_dir.join(format!("{stem}.md"));
            std::fs::write(&md_path, render_markdown(profile))?;
            info!(path = %md_path.display(), "profile Markdown written");
            written.push(md_path);
        }

        Ok(written)
    }
}

// ─── File stem ────────────────────────────────────────────────────────────────

/// Build a filesystem-safe stem from the email and window.
///
/// The email and window together are unique per profile run; `@`, `.`, `/`, and
/// spaces are replaced so the result is a legal filename on every platform.
///
/// Test: `profile_file_stem_safe`.
fn profile_file_stem(profile: &ContributorProfile) -> String {
    let email_safe = profile.canonical_email.replace(['@', '.', '/', ' '], "_");
    format!(
        "profile_{}_{}_{}",
        email_safe,
        &profile.profiled_since[..10.min(profile.profiled_since.len())],
        &profile.profiled_until[..10.min(profile.profiled_until.len())],
    )
}

// ─── Markdown renderer ────────────────────────────────────────────────────────

/// Render a profile as a Markdown document.
///
/// Why: this output is read directly by a person, and (from #5465) posted as a
/// GitHub issue body — so it is the profile's human-facing contract.
/// What: renders the header, the quality-trend table with a bar per row, the
/// strengths and weaknesses lists, the findings table, the narrative, and the
/// token/cost summary. Empty sections are omitted rather than rendered blank,
/// and the cost summary appears only when a narrative pass actually ran.
/// Test: `reporter_markdown_contains_sections`,
/// `reporter_markdown_no_cost_section_when_zero`,
/// `reporter_markdown_escapes_pipes_in_descriptions`.
pub fn render_markdown(profile: &ContributorProfile) -> String {
    let mut md = String::with_capacity(4096);

    md.push_str(&format!(
        "# Developer Profile: {} ({})\n\n",
        profile.canonical_name, profile.canonical_email
    ));
    md.push_str(&format!(
        "**Window**: {} → {}  \n",
        profile.profiled_since, profile.profiled_until
    ));
    if !profile.repositories.is_empty() {
        md.push_str(&format!(
            "**Repositories**: {}  \n",
            profile.repositories.join(", ")
        ));
    }
    let traj_str = match profile.improvement_trajectory {
        Trajectory::Improving => "Improving",
        Trajectory::Stable => "Stable",
        Trajectory::Declining => "Declining",
    };
    md.push_str(&format!("**Trajectory**: {traj_str}  \n"));
    md.push_str(&format!("**Generated**: {}  \n\n", profile.generated_at));

    if !profile.quality_trend.is_empty() {
        md.push_str("## Quality Trend\n\n");
        md.push_str("| Period | Score |\n|--------|:-----:|\n");
        for (label, score) in &profile.quality_trend {
            let bar = quality_bar(*score, 5.0);
            md.push_str(&format!("| {label} | {score:.2} {bar} |\n"));
        }
        md.push('\n');
    }

    if !profile.strengths.is_empty() {
        md.push_str("## Strengths\n\n");
        for s in &profile.strengths {
            md.push_str(&format!("- {s}\n"));
        }
        md.push('\n');
    }

    if !profile.recurring_weaknesses.is_empty() {
        md.push_str("## Areas for Improvement\n\n");
        for w in &profile.recurring_weaknesses {
            md.push_str(&format!("- {w}\n"));
        }
        md.push('\n');
    }

    if !profile.all_findings.is_empty() {
        md.push_str("## Findings\n\n");
        md.push_str(
            "| Period | Kind | Trend | Description |\n\
             |--------|------|-------|-------------|\n",
        );
        for lf in &profile.all_findings {
            let tag = trend_tag_str(lf.trend_tag);
            // A `|` inside a description would otherwise split the table row.
            let desc = lf.finding.description.replace('|', "\\|");
            md.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                lf.period_label, lf.finding.kind, tag, desc
            ));
        }
        md.push('\n');
    }

    if !profile.narrative.is_empty() {
        md.push_str("## Engineering Assessment\n\n");
        md.push_str(&profile.narrative);
        md.push_str("\n\n");
    }

    let tc = &profile.token_cost;
    if tc.input_tokens > 0 || tc.output_tokens > 0 {
        md.push_str("## Token & Cost Summary\n\n");
        md.push_str(&format!(
            "| Metric | Value |\n|--------|-------|\n\
             | Input tokens | {} |\n\
             | Output tokens | {} |\n\
             | Estimated cost | ${:.6} |\n\
             | Total latency | {}ms |\n\n",
            tc.input_tokens, tc.output_tokens, tc.cost_usd, tc.latency_ms
        ));
    }

    md.push_str(&format!(
        "---\n*Generated by tga {} — {}*\n",
        profile.review_version, profile.generated_at
    ));

    md
}

/// Render a five-cell bar for a score against `max`.
fn quality_bar(score: f64, max: f64) -> String {
    let filled = ((score / max) * 5.0).round().clamp(0.0, 5.0) as usize;
    let empty = 5usize.saturating_sub(filled);
    format!("{}{}", "▓".repeat(filled), "░".repeat(empty))
}

/// Render a [`TrendTag`] for the findings table.
fn trend_tag_str(tag: Option<TrendTag>) -> &'static str {
    match tag {
        Some(TrendTag::Recurring) => "🔁 Recurring",
        Some(TrendTag::New) => "🆕 New",
        Some(TrendTag::Resolved) => "✅ Resolved",
        Some(TrendTag::Worsening) => "📈 Worsening",
        None => "—",
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "reporter_tests.rs"]
mod tests;
