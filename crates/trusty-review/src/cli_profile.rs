//! `trusty-review profile` CLI subcommand (#568).
//!
//! Why: the profile subcommand orchestrates the full longitudinal profiling
//! pipeline — selector → batch assembly → diff sampling → per-period LLM
//! review → synthesis → report — from a single CLI invocation.
//! What: defines `ProfileArgs` (clap-derive struct) and `cmd_profile` (async
//! handler).  Progress to STDERR; stdout stays clean.  Always profile-dry-run-
//! safe: never posts PR comments; `--github-issue` is the only GitHub write
//! and is opt-in.
//! Test: `tests::profile_args_parse_defaults` verifies clap arg parsing for
//! all flags using `try_parse_from`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Parser;
use tracing::{info, warn};

use trusty_review::{
    config::{Provider, ReviewConfig},
    llm::build_provider,
    profile::{
        ContributorProfile, DiffSamplerConfig, assemble_period_batches,
        batch::Window,
        batch_reviewer::BatchReviewer,
        reporter::{GithubIssueConfig, ReportFormat, Reporter},
        resolve_contributor, resolve_db_path, sample_diffs_for_batches,
        synthesizer::Synthesizer,
    },
};

// ─── Profile args ─────────────────────────────────────────────────────────────

/// Arguments for the `profile` subcommand.
///
/// Why: groups all profile-mode flags in one place for clarity and testability.
/// What: contributor identity, time window, repo paths, LLM provider, and
/// output options.
/// Test: `tests::profile_args_parse_defaults`.
#[derive(Debug, Parser)]
pub struct ProfileArgs {
    /// Contributor identifier: canonical email, GitHub login, or name fragment.
    #[arg(value_name = "CONTRIBUTOR")]
    pub contributor: String,

    /// Path to the tga SQLite database file.
    /// Default: auto-resolved from `$TRUSTY_TGA_DB` or the standard location.
    #[arg(long, value_name = "PATH")]
    pub db: Option<PathBuf>,

    /// Start of the profiling window (inclusive, ISO 8601 date e.g. 2025-01-01).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// End of the profiling window (inclusive, ISO 8601 date e.g. 2026-05-31).
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,

    /// Period window granularity: `quarterly` (default), `monthly`, or `weekly`.
    #[arg(long, default_value = "quarterly", value_name = "WINDOW")]
    pub window: String,

    /// Comma-separated list of repository names to include.
    /// Default: all repositories for this contributor.
    #[arg(long, value_name = "NAME,...", value_delimiter = ',')]
    pub repos: Option<Vec<String>>,

    /// Root directory containing local repository checkouts.
    /// Used to resolve `<repos-root>/<repo-name>` for diff fetching.
    #[arg(long, value_name = "PATH")]
    pub repos_root: Option<PathBuf>,

    /// Output directory for profile files.
    /// Default: the configured log directory.
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Output format: `json`, `markdown`, or `both` (default: both).
    #[arg(long, default_value = "both", value_name = "FORMAT")]
    pub format: String,

    /// Maximum number of diffs to sample per period (default: 10).
    #[arg(long, default_value_t = 10, value_name = "N")]
    pub max_diffs: usize,

    /// Skip LLM calls — emit a stats-only profile (no findings, no narrative).
    #[arg(long)]
    pub dry_run: bool,

    /// LLM provider: `bedrock` (default) or `openrouter`.
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// Reviewer model slug for per-period finding extraction.
    /// Supports `bedrock/<id>` and `openrouter/<id>` prefixes.
    /// Default: the configured default reviewer model.
    #[arg(long, value_name = "SLUG")]
    pub reviewer_model: Option<String>,

    /// Post/update a per-contributor GitHub issue thread (opt-in).
    /// Requires GITHUB_TOKEN and --github-repo to be set.
    #[arg(long)]
    pub github_issue: bool,

    /// GitHub repository for issue upsert (format: `owner/repo`).
    /// Required when --github-issue is set.
    #[arg(long, value_name = "OWNER/REPO")]
    pub github_repo: Option<String>,
}

// ─── Command handler ──────────────────────────────────────────────────────────

/// Execute the `profile` subcommand.
///
/// Why: orchestrates the full longitudinal profiling pipeline in one function so
/// the CLI is a thin wrapper around the library.
/// What: resolves contributor identity, assembles period batches, (optionally)
/// samples diffs and calls the LLM, then writes the report.  All progress goes
/// to STDERR; STDOUT remains clean.
/// Test: CLI arg parsing via `tests::profile_args_parse_defaults`; pipeline
/// logic tested in individual module tests.
pub async fn cmd_profile(config: ReviewConfig, args: ProfileArgs) -> Result<()> {
    // Resolve DB path.
    let db_path = resolve_db_path(args.db.as_deref())
        .context("cannot resolve tga DB path — use --db <path>")?;

    eprintln!("[trusty-review profile] Using DB: {}", db_path.display());

    // Resolve contributor identity (opens its own DB connection).
    let identity = resolve_contributor(&db_path, &args.contributor)
        .with_context(|| format!("contributor '{}' not found in DB", args.contributor))?;

    // Open DB for subsequent pipeline stages.
    let db = tga::core::db::Database::open(&db_path)
        .with_context(|| format!("failed to open tga DB: {}", db_path.display()))?;

    eprintln!(
        "[trusty-review profile] Contributor: {} <{}>",
        identity.canonical_name, identity.canonical_email
    );

    // Resolve window.
    let window = parse_window(&args.window);

    // Assemble period batches.
    eprintln!(
        "[trusty-review profile] Assembling period batches (window={:?})...",
        window
    );
    let mut batches = assemble_period_batches(
        &db,
        &identity.canonical_email,
        window,
        args.since.as_deref(),
        args.until.as_deref(),
    )
    .context("failed to assemble period batches")?;

    eprintln!(
        "[trusty-review profile] {} period(s) assembled.",
        batches.len()
    );

    // Initialise the profile skeleton.
    let mut profile = ContributorProfile::new(
        &identity.canonical_email,
        &identity.canonical_name,
        args.since.as_deref().unwrap_or("earliest"),
        args.until.as_deref().unwrap_or("latest"),
    );
    // github_login is not stored in tga; leave as None unless added later.

    // Collect repositories from batches.
    let mut repos: std::collections::HashSet<String> = std::collections::HashSet::new();
    for b in &batches {
        for r in &b.stats.repositories {
            repos.insert(r.clone());
        }
    }
    if let Some(ref filter) = args.repos {
        profile.repositories = filter.clone();
    } else {
        profile.repositories = repos.into_iter().collect();
        profile.repositories.sort();
    }

    // Diff sampling + LLM unless --dry-run.
    let all_period_findings: Vec<Vec<trusty_review::profile::LongitudinalFinding>> = if args.dry_run
    {
        eprintln!("[trusty-review profile] --dry-run: skipping diff sampling and LLM calls.");
        vec![]
    } else {
        // Sample diffs.
        let sampler_config = DiffSamplerConfig {
            max_diffs: args.max_diffs,
            repo_paths: std::collections::HashMap::new(),
            repos_root: args.repos_root.clone(),
        };

        eprintln!("[trusty-review profile] Sampling diffs...");
        if let Err(e) = sample_diffs_for_batches(
            &mut batches,
            &db,
            &identity.canonical_email,
            &sampler_config,
        ) {
            warn!("diff sampling failed: {e} — continuing without diffs");
        }

        let total_diffs: usize = batches.iter().map(|b| b.sampled_diffs.len()).sum();
        eprintln!("[trusty-review profile] Sampled {total_diffs} diffs across all periods.");

        // Build LLM provider.
        let default_provider = args
            .provider
            .as_deref()
            .and_then(|p| p.parse::<Provider>().ok())
            .unwrap_or_else(|| config.role_models.reviewer.provider.clone());
        let reviewer_model = args
            .reviewer_model
            .clone()
            .unwrap_or_else(|| config.role_models.reviewer.model.clone());

        eprintln!("[trusty-review profile] Building LLM provider (model={reviewer_model})...");
        let llm = build_provider(
            &reviewer_model,
            &default_provider,
            &config.openrouter_api_key,
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

        // Per-period batch review.
        let batch_reviewer = BatchReviewer::new(Arc::clone(&llm), &reviewer_model);
        let mut all_findings = Vec::new();

        for batch in &batches {
            eprintln!(
                "[trusty-review profile] Reviewing period {} ...",
                batch.stats.period_label
            );
            let findings = batch_reviewer
                .review_period(batch, &mut profile.token_cost)
                .await;
            eprintln!("[trusty-review profile]   → {} finding(s)", findings.len());
            all_findings.push(findings);
        }

        // Update profile with batch data.
        profile.periods = batches.clone();

        // Synthesise.
        eprintln!("[trusty-review profile] Synthesising across periods...");
        let synthesizer = Synthesizer::new(llm, &reviewer_model);
        profile = synthesizer
            .synthesize(profile, all_findings, &batches)
            .await;

        // Return empty vec (already merged into profile by synthesizer).
        vec![]
    };

    // Populate periods if not already done (dry-run path).
    if args.dry_run {
        profile.periods = batches;
        // Populate quality_trend deterministically.
        profile.quality_trend = profile
            .periods
            .iter()
            .map(|b| (b.stats.period_label.clone(), b.stats.quality_score))
            .collect();
        use trusty_review::profile::synthesizer::derive_trajectory;
        profile.improvement_trajectory = derive_trajectory(&profile.quality_trend);
    }

    let _ = all_period_findings; // consumed by synthesizer on the non-dry-run path.

    // Resolve output directory.
    let output_dir = args
        .output
        .clone()
        .unwrap_or_else(|| config.log_dir.join("profiles"));

    // Parse format.
    let report_format: ReportFormat = args.format.parse().unwrap_or_else(|e| {
        warn!("unknown --format {}: {e} — defaulting to both", args.format);
        ReportFormat::Both
    });

    // Build reporter.
    let mut reporter = Reporter::new(&output_dir, report_format);

    // Optional GitHub issue config.
    if args.github_issue {
        if let Some(ref gh_repo) = args.github_repo {
            let parts: Vec<&str> = gh_repo.splitn(2, '/').collect();
            if parts.len() == 2 {
                let token = config.github_token.clone();
                if token.is_empty() {
                    warn!("--github-issue set but GITHUB_TOKEN is empty — skipping issue upsert");
                } else {
                    let gc = GithubIssueConfig {
                        owner: parts[0].to_string(),
                        repo: parts[1].to_string(),
                        label: "dev-profile".to_string(),
                        token,
                    };
                    reporter = reporter.with_github_issue(gc);
                }
            } else {
                warn!("--github-repo must be in 'owner/repo' format — got {gh_repo}");
            }
        } else {
            warn!("--github-issue set but --github-repo not provided — skipping issue upsert");
        }
    }

    // Write report files.
    eprintln!(
        "[trusty-review profile] Writing report to {} ...",
        output_dir.display()
    );
    match reporter.write_profile(&profile) {
        Ok(paths) => {
            for p in &paths {
                eprintln!("[trusty-review profile] Written: {}", p.display());
            }
        }
        Err(e) => {
            warn!("failed to write profile report: {e}");
        }
    }

    // Optional GitHub issue upsert.
    if args.github_issue {
        eprintln!("[trusty-review profile] Upserting GitHub issue...");
        match reporter.upsert_github_issue(&profile).await {
            Some(url) => eprintln!("[trusty-review profile] GitHub issue: {url}"),
            None => eprintln!("[trusty-review profile] GitHub issue upsert skipped or failed."),
        }
    }

    // Print summary to STDERR.
    info!(
        contributor = %identity.canonical_email,
        periods = profile.periods.len(),
        findings = profile.all_findings.len(),
        trajectory = ?profile.improvement_trajectory,
        cost_usd = profile.token_cost.cost_usd,
        "profile complete"
    );

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Parse the `--window` string into a `Window` variant.
fn parse_window(s: &str) -> Window {
    match s.to_lowercase().as_str() {
        "quarterly" | "q" => Window::Quarterly,
        "monthly" | "m" => Window::Monthly,
        "weekly" | "w" => Window::Weekly,
        other => {
            // Try to parse as an integer number of weeks.
            if let Ok(n) = other.parse::<u32>() {
                Window::Custom(n)
            } else {
                warn!("unknown window '{}' — defaulting to quarterly", s);
                Window::Quarterly
            }
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: all ProfileArgs flags must have correct clap definitions that
    /// parse without error.
    /// What: uses `try_parse_from` with just the contributor positional arg
    /// and asserts the defaults are set correctly.
    /// Test: this test itself.
    #[test]
    fn profile_args_parse_defaults() {
        let args = ProfileArgs::try_parse_from(["profile", "alice@example.com"])
            .expect("parse should succeed");
        assert_eq!(args.contributor, "alice@example.com");
        assert!(args.db.is_none());
        assert_eq!(args.window, "quarterly");
        assert_eq!(args.max_diffs, 10);
        assert!(!args.dry_run);
        assert!(!args.github_issue);
        assert_eq!(args.format, "both");
    }

    /// Why: all optional flags must be parseable.
    /// What: passes all flags and asserts values.
    /// Test: this test itself.
    #[test]
    fn profile_args_parse_all_flags() {
        let args = ProfileArgs::try_parse_from([
            "profile",
            "alice@example.com",
            "--db",
            "/tmp/org.tga.db",
            "--since",
            "2026-01-01",
            "--until",
            "2026-06-30",
            "--window",
            "monthly",
            "--repos",
            "acme/api,acme/web",
            "--repos-root",
            "/repos",
            "--output",
            "/tmp/profiles",
            "--format",
            "json",
            "--max-diffs",
            "5",
            "--dry-run",
            "--provider",
            "bedrock",
            "--reviewer-model",
            "bedrock/us.anthropic.claude-sonnet-4-6",
            "--github-issue",
            "--github-repo",
            "acme/trusty-profiles",
        ])
        .expect("parse should succeed");

        assert_eq!(args.contributor, "alice@example.com");
        assert_eq!(args.db, Some(PathBuf::from("/tmp/org.tga.db")));
        assert_eq!(args.since, Some("2026-01-01".to_string()));
        assert_eq!(args.until, Some("2026-06-30".to_string()));
        assert_eq!(args.window, "monthly");
        assert_eq!(
            args.repos,
            Some(vec!["acme/api".to_string(), "acme/web".to_string()])
        );
        assert_eq!(args.repos_root, Some(PathBuf::from("/repos")));
        assert_eq!(args.output, Some(PathBuf::from("/tmp/profiles")));
        assert_eq!(args.format, "json");
        assert_eq!(args.max_diffs, 5);
        assert!(args.dry_run);
        assert_eq!(args.provider, Some("bedrock".to_string()));
        assert_eq!(
            args.reviewer_model,
            Some("bedrock/us.anthropic.claude-sonnet-4-6".to_string())
        );
        assert!(args.github_issue);
        assert_eq!(args.github_repo, Some("acme/trusty-profiles".to_string()));
    }

    /// Why: `parse_window` must map all string variants to the correct `Window`.
    /// What: tests all known strings plus a custom integer and an unknown string.
    /// Test: this test itself.
    #[test]
    fn parse_window_variants() {
        assert_eq!(parse_window("quarterly"), Window::Quarterly);
        assert_eq!(parse_window("monthly"), Window::Monthly);
        assert_eq!(parse_window("weekly"), Window::Weekly);
        assert_eq!(parse_window("4"), Window::Custom(4));
        // Unknown → Quarterly fallback.
        assert_eq!(parse_window("sprint"), Window::Quarterly);
    }
}
