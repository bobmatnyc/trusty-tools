//! Profile reporter for the contributor-profile pipeline (#567).
//!
//! Why: the pipeline output (`ContributorProfile`) must be persisted and
//! optionally published to GitHub as a per-contributor issue thread so
//! engineering managers can review profiles without running the CLI.
//! What: `Reporter` serialises the profile to `profile.json` and renders
//! `profile.md` (ASCII quality-trend table, findings table, narrative, cost
//! summary).  The GitHub issue upsert is opt-in — default OFF so a plain
//! `profile` run never unexpectedly creates issues.  The GitHub write uses
//! `issues:write` scope (POST/PATCH `/repos/{owner}/{repo}/issues`), which
//! is distinct from the push firewall (git-write).
//! Test: `tests` module covers JSON serialisation, Markdown rendering, and
//! the GitHub issue request construction (url/method/body, fully mocked —
//! no real network calls).

use std::path::PathBuf;

use tracing::{info, warn};

use crate::integrations::github::GithubClient;
use crate::profile::types::{ContributorProfile, Trajectory, TrendTag};

#[path = "reporter_github.rs"]
mod reporter_github;

// ─── Output format ────────────────────────────────────────────────────────────

/// Output format for the profile report.
///
/// Why: callers may need JSON only (for programmatic consumption), Markdown
/// only (for human reading), or both.
/// What: three-variant enum used by `Reporter::write_profile`.
/// Test: `tests::reporter_json_output` and `tests::reporter_markdown_output`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportFormat {
    /// Emit only `profile.json`.
    Json,
    /// Emit only `profile.md`.
    Markdown,
    /// Emit both `profile.json` and `profile.md`.
    Both,
}

impl std::str::FromStr for ReportFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            "both" => Ok(Self::Both),
            other => Err(format!("unknown format: {other}")),
        }
    }
}

// ─── Reporter ─────────────────────────────────────────────────────────────────

/// Profile reporter — writes JSON/Markdown outputs and optionally upserts a
/// GitHub issue thread.
///
/// Why: separating the I/O from the pipeline logic allows testing the
/// rendering code without a filesystem.
/// What: `write_profile` writes files; `upsert_github_issue` (opt-in) calls
/// the GitHub REST API.  Both operations are independent — a file-write error
/// does not prevent issue upsert and vice versa.
/// Test: see `tests` module below.
pub struct Reporter {
    output_dir: PathBuf,
    format: ReportFormat,
    /// GitHub issue upsert config (None → disabled).
    github_config: Option<GithubIssueConfig>,
}

/// Configuration for the opt-in GitHub issue upsert.
///
/// Why: bundles the GitHub parameters needed for issue upsert so they can be
/// passed as a single optional value — default None keeps it off.
/// What: `owner` and `repo` identify the target repository; `label` is the
/// search label (e.g. `"dev-profile"`).  Token resolved externally.
/// Test: `tests::reporter_github_issue_request_construction`.
#[derive(Debug, Clone)]
pub struct GithubIssueConfig {
    /// Repository owner (org or user).
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Label applied to every profile issue (used for search too).
    pub label: String,
    /// GitHub PAT or App installation token.
    pub token: String,
}

impl Reporter {
    /// Create a `Reporter` with the given output directory and format.
    ///
    /// Why: most callers need only file output; the GitHub config is optional
    /// and set separately via `with_github_issue`.
    /// What: stores `output_dir` and `format`; `github_config` is `None`.
    /// Test: exercised by all reporter tests.
    pub fn new(output_dir: impl Into<PathBuf>, format: ReportFormat) -> Self {
        Self {
            output_dir: output_dir.into(),
            format,
            github_config: None,
        }
    }

    /// Enable the opt-in GitHub issue upsert for this reporter.
    ///
    /// Why: the `--github-issue` CLI flag enables this; without it, no GitHub
    /// write is performed, keeping plain profile runs side-effect-free.
    /// What: sets the `github_config` field; call `write_profile` as normal —
    /// the upsert is triggered automatically when this is set.
    /// Test: `tests::reporter_github_issue_request_construction`.
    pub fn with_github_issue(mut self, config: GithubIssueConfig) -> Self {
        self.github_config = Some(config);
        self
    }

    /// Write the profile to disk in the configured format.
    ///
    /// Why: the pipeline calls this once after `Synthesizer::synthesize`
    /// to persist the final profile.
    /// What: creates `output_dir` if needed, writes `profile.json` and/or
    /// `profile.md` depending on `format`, returns the paths written.
    /// Test: `tests::reporter_json_output`, `tests::reporter_markdown_output`.
    pub fn write_profile(
        &self,
        profile: &ContributorProfile,
    ) -> Result<Vec<PathBuf>, std::io::Error> {
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
            let md = render_markdown(profile);
            std::fs::write(&md_path, &md)?;
            info!(path = %md_path.display(), "profile Markdown written");
            written.push(md_path);
        }

        Ok(written)
    }

    /// Upsert a per-contributor GitHub issue thread (opt-in).
    ///
    /// Why: engineering managers can track contributor profiles over time in
    /// GitHub issues; each run updates (PATCH) an existing issue or creates
    /// (POST) a new one.  This is always opt-in — never called unless
    /// `with_github_issue` was called.
    /// What: searches issues by `label` + title prefix `[dev-profile]
    /// <canonical_email>`.  If found, POSTs a new comment with the updated
    /// Markdown.  If not found, creates a new issue.
    /// Returns the issue URL on success; on any error logs and returns `None`.
    /// Test: `tests::reporter_github_issue_request_construction` (mock only —
    /// no real network calls).
    pub async fn upsert_github_issue(&self, profile: &ContributorProfile) -> Option<String> {
        let config = self.github_config.as_ref()?;
        let client = match GithubClient::new() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "upsert_github_issue: failed to build HTTP client — skipping");
                return None;
            }
        };
        let markdown = render_markdown(profile);

        match reporter_github::github_upsert_issue(&client, config, profile, &markdown).await {
            Ok(url) => {
                info!(url = %url, "dev-profile issue upserted");
                Some(url)
            }
            Err(e) => {
                warn!(error = %e, "upsert_github_issue failed — continuing without issue update");
                None
            }
        }
    }
}

// ─── File stem helper ─────────────────────────────────────────────────────────

/// Generate a filesystem-safe file stem for profile output files.
///
/// Why: the email address and profile window form a natural unique key;
/// sanitising to a filesystem-safe string avoids issues with special chars.
/// What: replaces `@`, `.`, `/`, and spaces with `_`.
/// Test: `tests::profile_file_stem_safe`.
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

/// Render a `ContributorProfile` as a Markdown document.
///
/// Why: the Markdown output is human-readable and suitable for GitHub issue
/// bodies and local log review.
/// What: renders header, quality-trend ASCII table, strengths/weaknesses bullet
/// lists, per-period finding table, LLM narrative, and token/cost summary.
/// Test: `tests::reporter_markdown_contains_sections`.
pub fn render_markdown(profile: &ContributorProfile) -> String {
    let mut md = String::with_capacity(4096);

    // Header.
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

    // Quality trend table.
    if !profile.quality_trend.is_empty() {
        md.push_str("## Quality Trend\n\n");
        md.push_str("| Period | Score |\n|--------|:-----:|\n");
        for (label, score) in &profile.quality_trend {
            let bar = quality_bar(*score, 5.0);
            md.push_str(&format!("| {label} | {score:.2} {bar} |\n"));
        }
        md.push('\n');
    }

    // Strengths.
    if !profile.strengths.is_empty() {
        md.push_str("## Strengths\n\n");
        for s in &profile.strengths {
            md.push_str(&format!("- {s}\n"));
        }
        md.push('\n');
    }

    // Recurring weaknesses.
    if !profile.recurring_weaknesses.is_empty() {
        md.push_str("## Areas for Improvement\n\n");
        for w in &profile.recurring_weaknesses {
            md.push_str(&format!("- {w}\n"));
        }
        md.push('\n');
    }

    // Per-period finding table.
    if !profile.all_findings.is_empty() {
        md.push_str("## Findings\n\n");
        md.push_str(
            "| Period | Kind | Trend | Description |\n\
             |--------|------|-------|-------------|\n",
        );
        for lf in &profile.all_findings {
            let tag = trend_tag_str(lf.trend_tag.as_ref());
            let desc = lf.finding.description.replace('|', "\\|");
            md.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                lf.period_label, lf.finding.kind, tag, desc
            ));
        }
        md.push('\n');
    }

    // Narrative.
    if !profile.narrative.is_empty() {
        md.push_str("## Engineering Assessment\n\n");
        md.push_str(&profile.narrative);
        md.push_str("\n\n");
    }

    // Token / cost summary.
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
        "---\n*Generated by trusty-review {} — {}*\n",
        profile.review_version, profile.generated_at
    ));

    md
}

/// Simple ASCII quality bar (▓ per point, max 5 chars).
fn quality_bar(score: f64, max: f64) -> String {
    let filled = ((score / max) * 5.0).round() as usize;
    let empty = 5usize.saturating_sub(filled);
    format!("{}{}", "▓".repeat(filled), "░".repeat(empty))
}

/// Format a `TrendTag` for Markdown display.
fn trend_tag_str(tag: Option<&TrendTag>) -> &'static str {
    match tag {
        Some(TrendTag::Recurring) => "🔁 Recurring",
        Some(TrendTag::New) => "🆕 New",
        Some(TrendTag::Resolved) => "✅ Resolved",
        Some(TrendTag::Worsening) => "📈 Worsening",
        None => "—",
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Tests live in reporter_tests.rs to keep this file under 500 lines.

#[cfg(test)]
#[path = "reporter_tests.rs"]
mod tests;
