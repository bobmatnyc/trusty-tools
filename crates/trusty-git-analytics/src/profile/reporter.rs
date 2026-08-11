//! JSON and Markdown rendering for a finished contributor profile.
//!
//! Why: the profile has two audiences — a program reading `profile.json` and a
//! person reading `profile.md` — and neither should have to run the pipeline
//! again to get its own format.
//! What: [`Reporter`] writes the configured formats into an output directory;
//! [`render_markdown`] produces the human-readable document (header, quality
//! trend, strengths, weaknesses, findings table, narrative, cost). Publishing
//! the profile to a GitHub issue is #5465 and is deliberately absent here.
//! Test: the `tests` module covers JSON round-tripping, every Markdown
//! section, format parsing, and the file-stem sanitisation.

use std::path::PathBuf;

use tracing::info;

use super::types::{ContributorProfile, Trajectory, TrendTag};

// ─── Output format ───────────────────────────────────────────────────────────

/// Which files a profile run writes.
///
/// Why: a dashboard wants only JSON, a manager only Markdown, and a CLI run
/// usually wants both.
/// What: three-variant enum consumed by [`Reporter::write_profile`], parsable
/// from a CLI string via `FromStr`.
/// Test: `report_format_from_str`, `reporter_both_format_writes_two_files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// Write only the JSON profile.
    Json,
    /// Write only the Markdown profile.
    Markdown,
    /// Write both.
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

// ─── Reporter ────────────────────────────────────────────────────────────────

/// Writes a finished profile to disk.
///
/// Why: keeping I/O out of the pipeline means the rendering can be tested
/// without a filesystem and the pipeline without a renderer.
/// What: holds the output directory and the format; [`Reporter::write_profile`]
/// creates the directory and writes the files, returning their paths.
/// Test: `reporter_json_output`, `reporter_both_format_writes_two_files`.
pub struct Reporter {
    output_dir: PathBuf,
    format: ReportFormat,
}

impl Reporter {
    /// Create a reporter writing `format` into `output_dir`.
    ///
    /// Why: both settings are fixed for a run, so they belong on the reporter
    /// rather than on every call.
    /// What: stores the directory and format; the directory is created lazily
    /// on the first write.
    /// Test: used by every reporter test.
    pub fn new(output_dir: impl Into<PathBuf>, format: ReportFormat) -> Self {
        Self {
            output_dir: output_dir.into(),
            format,
        }
    }

    /// Write the profile in the configured format.
    ///
    /// Why: this is the pipeline's last step, and the caller needs the paths
    /// back so it can print or publish them.
    /// What: creates the output directory if needed, writes `<stem>.json`
    /// and/or `<stem>.md` where the stem is derived from the contributor and
    /// window, and returns the paths written in that order.
    /// Test: `reporter_json_output`, `reporter_both_format_writes_two_files`.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] when the directory cannot be created, a file cannot
    /// be written, or the profile cannot be serialised.
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

// ─── File stem helper ────────────────────────────────────────────────────────

/// Build a filesystem-safe stem for a profile's output files.
///
/// Why: the email plus the window is the natural unique key, but an email is
/// not a safe filename on every platform.
/// What: replaces `@`, `.`, `/`, and spaces with `_`, then appends the first
/// ten characters of each window bound.
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

// ─── Markdown renderer ───────────────────────────────────────────────────────

/// Render a profile as a Markdown document.
///
/// Why: Markdown is what a person reads and what a tracker comment accepts, so
/// one renderer serves both.
/// What: emits the identity header, a quality-trend table with an ASCII bar,
/// strengths and weaknesses, the trend-tagged findings table, the narrative,
/// and — only when a model was actually called — the token/cost table. Empty
/// sections are omitted rather than rendered blank.
/// Test: `reporter_markdown_contains_sections`,
/// `reporter_markdown_no_cost_section_when_zero`.
pub fn render_markdown(profile: &ContributorProfile) -> String {
    use std::fmt::Write as _;

    let mut md = String::with_capacity(4096);

    let _ = write!(
        md,
        "# Developer Profile: {} ({})\n\n",
        profile.canonical_name, profile.canonical_email
    );
    // The two trailing spaces are a Markdown hard line break.
    let _ = writeln!(
        md,
        "**Window**: {} → {}  ",
        profile.profiled_since, profile.profiled_until
    );
    if !profile.repositories.is_empty() {
        let _ = writeln!(
            md,
            "**Repositories**: {}  ",
            profile.repositories.join(", ")
        );
    }
    let traj_str = match profile.improvement_trajectory {
        Trajectory::Improving => "Improving",
        Trajectory::Stable => "Stable",
        Trajectory::Declining => "Declining",
    };
    let _ = writeln!(md, "**Trajectory**: {traj_str}  ");
    let _ = writeln!(md, "**Generated**: {}  \n", profile.generated_at);

    if !profile.quality_trend.is_empty() {
        md.push_str("## Quality Trend\n\n");
        md.push_str("| Period | Score |\n|--------|:-----:|\n");
        for (label, score) in &profile.quality_trend {
            let bar = quality_bar(*score, 5.0);
            let _ = writeln!(md, "| {label} | {score:.2} {bar} |");
        }
        md.push('\n');
    }

    if !profile.strengths.is_empty() {
        md.push_str("## Strengths\n\n");
        for s in &profile.strengths {
            let _ = writeln!(md, "- {s}");
        }
        md.push('\n');
    }

    if !profile.recurring_weaknesses.is_empty() {
        md.push_str("## Areas for Improvement\n\n");
        for w in &profile.recurring_weaknesses {
            let _ = writeln!(md, "- {w}");
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
            // Escape pipes so a description can never break the table.
            let desc = lf.finding.description.replace('|', "\\|");
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} |",
                lf.period_label,
                lf.finding.kind,
                trend_tag_str(lf.trend_tag),
                desc
            );
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
        let _ = write!(
            md,
            "| Metric | Value |\n|--------|-------|\n\
             | Input tokens | {} |\n\
             | Output tokens | {} |\n\
             | Estimated cost | ${:.6} |\n\
             | Total latency | {}ms |\n\n",
            tc.input_tokens, tc.output_tokens, tc.cost_usd, tc.latency_ms
        );
    }

    let _ = write!(
        md,
        "---\n*Generated by tga {} — {}*\n",
        profile.review_version, profile.generated_at
    );

    md
}

/// Render a five-cell ASCII bar for a score out of `max`.
fn quality_bar(score: f64, max: f64) -> String {
    let filled = ((score / max) * 5.0).round().clamp(0.0, 5.0) as usize;
    let empty = 5usize.saturating_sub(filled);
    format!("{}{}", "▓".repeat(filled), "░".repeat(empty))
}

/// Render a trend tag for the Markdown findings table.
fn trend_tag_str(tag: Option<TrendTag>) -> &'static str {
    match tag {
        Some(TrendTag::Recurring) => "🔁 Recurring",
        Some(TrendTag::New) => "🆕 New",
        Some(TrendTag::Resolved) => "✅ Resolved",
        Some(TrendTag::Worsening) => "📈 Worsening",
        None => "—",
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::types::{
        FindingEffort, LongitudinalFinding, ProfileFinding, TokenCostSummary,
    };

    fn make_profile() -> ContributorProfile {
        let mut p = ContributorProfile::new(
            "alice@example.com",
            "Alice Smith",
            "2026-01-01",
            "2026-06-30",
        );
        p.repositories = vec!["acme/api".to_string()];
        p.improvement_trajectory = Trajectory::Improving;
        p.quality_trend = vec![("2026-Q1".to_string(), 3.0), ("2026-Q2".to_string(), 3.8)];
        p.strengths = vec!["Consistent ticket coverage".to_string()];
        p.recurring_weaknesses = vec!["Missing error handling".to_string()];
        p.all_findings = vec![LongitudinalFinding {
            period_label: "2026-Q1".to_string(),
            finding: ProfileFinding::new(
                "src/lib.rs",
                "error_handling",
                "Missing propagation",
                "Use ?",
                0.8,
                FindingEffort::Medium,
            ),
            trend_tag: Some(TrendTag::Recurring),
        }];
        p.narrative = "Alice shows strong improvement.".to_string();
        p.token_cost = TokenCostSummary {
            input_tokens: 500,
            output_tokens: 200,
            cost_usd: 0.005,
            latency_ms: 1500,
        };
        p
    }

    /// Why: the JSON file is the machine-readable contract, so it must parse
    /// back into the same profile rather than merely being written.
    /// What: writes to a temp directory, reads the file back, and asserts the
    /// identity survived.
    /// Test: this test itself.
    #[test]
    fn reporter_json_output() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let reporter = Reporter::new(tmp.path(), ReportFormat::Json);

        let paths = reporter.write_profile(&make_profile()).expect("write");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].extension().is_some_and(|e| e == "json"));

        let content = std::fs::read_to_string(&paths[0]).expect("read");
        let back: ContributorProfile = serde_json::from_str(&content).expect("parse");
        assert_eq!(back.canonical_email, "alice@example.com");
    }

    /// Why: the Markdown document is what a manager actually reads; a silently
    /// dropped section loses evidence without failing anything.
    /// What: renders a fully-populated profile and asserts every section
    /// heading and one piece of its content.
    /// Test: this test itself.
    #[test]
    fn reporter_markdown_contains_sections() {
        let md = render_markdown(&make_profile());

        assert!(md.contains("# Developer Profile: Alice Smith"), "header");
        assert!(md.contains("## Quality Trend"), "quality trend section");
        assert!(md.contains("2026-Q1"), "period label");
        assert!(md.contains("## Strengths"), "strengths section");
        assert!(md.contains("Consistent ticket coverage"), "strength text");
        assert!(md.contains("## Areas for Improvement"), "weaknesses");
        assert!(md.contains("## Findings"), "findings section");
        assert!(md.contains("error_handling"), "finding kind");
        assert!(md.contains("Recurring"), "trend tag");
        assert!(
            md.contains("## Engineering Assessment"),
            "narrative section"
        );
        assert!(md.contains("Alice shows strong improvement"), "narrative");
        assert!(md.contains("## Token & Cost Summary"), "cost section");
        assert!(md.contains("500"), "input tokens");
        assert!(md.contains("Generated by tga"), "footer names tga");
    }

    /// Why: `Both` is the CLI default shape, and writing one file while
    /// reporting two paths would be a silent data loss.
    /// What: writes with `Both` and asserts one `.json` and one `.md`.
    /// Test: this test itself.
    #[test]
    fn reporter_both_format_writes_two_files() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let reporter = Reporter::new(tmp.path(), ReportFormat::Both);

        let paths = reporter.write_profile(&make_profile()).expect("write");
        assert_eq!(paths.len(), 2, "Both writes two files");
        assert!(paths
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "json")));
        assert!(paths
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "md")));
    }

    /// Why: an email contains `@` and `.`, which make for a hostile filename;
    /// the stem is also how a later run finds the previous profile.
    /// What: asserts the stem is prefixed and carries no `@`.
    /// Test: this test itself.
    #[test]
    fn profile_file_stem_safe() {
        let stem = profile_file_stem(&make_profile());
        assert!(!stem.contains('@'), "stem must not contain @: {stem}");
        assert!(stem.starts_with("profile_"), "stem prefix: {stem}");
    }

    /// Why: the format comes from a CLI flag, so unknown input must be rejected
    /// rather than silently defaulted.
    /// What: parses each accepted spelling and asserts an unknown one errors.
    /// Test: this test itself.
    #[test]
    fn report_format_from_str() {
        use std::str::FromStr;
        assert_eq!(
            ReportFormat::from_str("json").expect("json"),
            ReportFormat::Json
        );
        assert_eq!(
            ReportFormat::from_str("markdown").expect("markdown"),
            ReportFormat::Markdown
        );
        assert_eq!(
            ReportFormat::from_str("both").expect("both"),
            ReportFormat::Both
        );
        assert_eq!(
            ReportFormat::from_str("md").expect("md"),
            ReportFormat::Markdown
        );
        assert!(ReportFormat::from_str("xml").is_err());
    }

    /// Why: a deterministic run makes no model calls, and a cost table full of
    /// zeros would imply one was made and was free.
    /// What: zeroes the telemetry and asserts the section is absent.
    /// Test: this test itself.
    #[test]
    fn reporter_markdown_no_cost_section_when_zero() {
        let mut profile = make_profile();
        profile.token_cost = TokenCostSummary::default();
        let md = render_markdown(&profile);
        assert!(
            !md.contains("## Token & Cost Summary"),
            "zero cost omits the cost section"
        );
    }

    /// Why: a description containing a pipe would break the Markdown table and
    /// silently corrupt every column after it.
    /// What: renders a finding whose description contains `|` and asserts the
    /// pipe was escaped.
    /// Test: this test itself.
    #[test]
    fn reporter_markdown_escapes_pipes_in_description() {
        let mut profile = make_profile();
        profile.all_findings[0].finding.description = "matches a|b in the regex".to_string();
        let md = render_markdown(&profile);
        assert!(
            md.contains("matches a\\|b in the regex"),
            "pipe must be escaped: {md}"
        );
    }
}
