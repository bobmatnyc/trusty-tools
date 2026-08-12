//! `tga profile <contributor>` — longitudinal contributor quality profile.
//!
//! Why: profiling is tga's domain (#5468), and the pipeline `src/profile/`
//! carries is only usable from a program until something drives it from a
//! command line. This is that driver, and it is the first caller of
//! `PeriodReviewer::review_period`.
//!
//! What: resolves the contributor, assembles period batches, samples diffs,
//! reviews each period through `trusty_common::inference`, synthesises, writes
//! the report, and optionally upserts a per-contributor GitHub issue thread.
//! Progress goes to STDERR; STDOUT stays clean.
//!
//! #5465: a period whose provider call FAILED is reported as skipped, never as
//! a period with no findings — see [`PeriodRunSummary`]. Without that, an
//! outage across twelve quarters renders as twelve clean quarters and the
//! trajectory is computed over a sample the reader cannot see is smaller.
//!
//! Test: `tests` at the foot of this file cover argument parsing and window
//! mapping; the pipeline stages carry their own tests in `src/profile/`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Args;
use tracing::{info, warn};

use tga::collect::github::GitHubClient;
use tga::core::config::{expand_path, Config};
use tga::profile::{
    apply_deterministic_synthesis, assemble_period_batches, sample_diffs_for_batches,
    upsert_profile_issue, ContributorProfile, ContributorSelector, DiffSamplerConfig,
    GithubIssueConfig, PeriodReviewer, PeriodRunSummary, ReportFormat, Reporter, Synthesizer,
    Window,
};

/// Model slug used when `--model` is not supplied.
///
/// Why: the slug must carry a routing prefix — `trusty_common::inference`
/// dispatches on it — so a bare model id from the `llm:` config block is not a
/// usable default here. This is the workspace's standard OpenRouter Sonnet
/// route, the same one trusty-mpm defaults its own model to.
/// Test: `profile_args_parse_defaults`.
pub const DEFAULT_PROFILE_MODEL: &str = "openrouter/anthropic/claude-sonnet-4-6";

// ─── Arguments ────────────────────────────────────────────────────────────────

/// Arguments for `tga profile`.
///
/// Why: every profiling decision a caller can make — who, over what window, at
/// what granularity, with which model, and whether to publish — is one flag, so
/// clap validates them before the pipeline starts a multi-minute run.
/// What: clap `Args` wired into `Commands::Profile` in `main.rs`.
/// Test: `profile_args_parse_defaults`, `profile_args_parse_all_flags`.
#[derive(Args, Debug)]
#[command(
    about = "Longitudinal per-contributor quality profile.",
    long_about = "Build a longitudinal quality profile for one contributor.\n\n\
  Pipeline: identity resolution → period batches → diff sampling → per-period \
  LLM review → cross-period synthesis → JSON/Markdown report.\n\n\
  Pass --dry-run for a deterministic, model-free profile (trend and trajectory \
  only). Pass --github-issue with --github-repo to publish the report to a \
  per-contributor GitHub issue thread."
)]
pub struct ProfileArgs {
    /// Contributor identifier: canonical email, GitHub login, or display name.
    #[arg(value_name = "CONTRIBUTOR")]
    pub contributor: String,

    /// Path to the tga SQLite database. Defaults to the database the global
    /// `--database` / config resolution already selected.
    #[arg(long, value_name = "PATH")]
    pub db: Option<PathBuf>,

    /// Inclusive start of the profiling window (ISO 8601, e.g. 2026-01-01).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Inclusive end of the profiling window (ISO 8601, e.g. 2026-06-30).
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,

    /// Period granularity: `quarterly` (default), `monthly`, `weekly`, or a
    /// number of weeks.
    #[arg(long, default_value = "quarterly", value_name = "WINDOW")]
    pub window: String,

    /// Restrict the reported repository list to these names.
    #[arg(long, value_name = "NAME,...", value_delimiter = ',')]
    pub repos: Option<Vec<String>>,

    /// Root directory holding local checkouts, used as `<root>/<repo-name>`
    /// when sampling diffs.
    #[arg(long, value_name = "PATH")]
    pub repos_root: Option<PathBuf>,

    /// Output directory for the profile files.
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Output format: `json`, `markdown`, or `both` (default).
    #[arg(long, default_value = "both", value_name = "FORMAT")]
    pub format: String,

    /// Maximum diffs sampled per period.
    #[arg(long, default_value_t = 10, value_name = "N")]
    pub max_diffs: usize,

    /// Skip every model call — emit a deterministic, stats-only profile.
    #[arg(long)]
    pub dry_run: bool,

    /// Model slug, including its routing prefix (e.g. `openrouter/…`,
    /// `bedrock/…`).
    #[arg(long, value_name = "SLUG")]
    pub model: Option<String>,

    /// Publish the report to a per-contributor GitHub issue thread.
    #[arg(long)]
    pub github_issue: bool,

    /// Repository holding the profile issues, as `owner/repo`. Required with
    /// `--github-issue`.
    #[arg(long, value_name = "OWNER/REPO")]
    pub github_repo: Option<String>,
}

// ─── Command handler ──────────────────────────────────────────────────────────

/// Execute `tga profile`.
///
/// Why: the pipeline is a sequence of independently testable stages, so this
/// function stays a thin sequencer over them rather than owning any of the
/// logic itself.
/// What: runs the six stages, writing progress to STDERR. A failure that costs
/// only part of the result (diff sampling, one period's review, the narrative,
/// the GitHub post) is reported and the run continues; a failure that makes the
/// result meaningless (unknown contributor, unreadable database) aborts.
///
/// # Errors
///
/// Contributor resolution, database access, report-directory writes, and a
/// malformed `--github-repo` all abort with context.
///
/// Test: the stages carry their own tests; this sequencer is exercised by the
/// binary.
pub async fn run(config: Config, db_path: &Path, args: ProfileArgs) -> Result<()> {
    let db_path = match args.db.as_deref() {
        Some(explicit) => expand_path(explicit),
        None => db_path.to_path_buf(),
    };
    eprintln!("[tga profile] database: {}", db_path.display());

    let selector = ContributorSelector::open(&db_path)
        .with_context(|| format!("cannot open tga database {}", db_path.display()))?;
    let identity = selector.resolve(&args.contributor)?;
    eprintln!(
        "[tga profile] contributor: {} <{}>",
        identity.canonical_name, identity.canonical_email
    );

    let window = parse_window(&args.window);
    let mut batches = assemble_period_batches(
        selector.database(),
        &identity.canonical_email,
        window,
        args.since.as_deref(),
        args.until.as_deref(),
    )?;
    eprintln!(
        "[tga profile] {} period(s) assembled (window={window:?})",
        batches.len()
    );

    let mut profile = ContributorProfile::new(
        &identity.canonical_email,
        &identity.canonical_name,
        args.since.as_deref().unwrap_or("earliest"),
        args.until.as_deref().unwrap_or("latest"),
    );
    profile.repositories = resolve_repositories(&args, &batches);

    let model = args
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_PROFILE_MODEL.to_string());

    let summary = if args.dry_run {
        eprintln!("[tga profile] --dry-run: no diff sampling, no model calls");
        apply_deterministic_synthesis(&mut profile, Vec::new(), &batches);
        PeriodRunSummary::default()
    } else {
        sample_diffs(&mut batches, &selector, &identity.canonical_email, &args);
        let summary = review_periods(&batches, &mut profile, &model).await?;
        narrate(&mut profile, &model).await;
        summary
    };
    profile.periods = batches;

    // #5465: a skipped period is not a clean period — say so, in both places.
    eprintln!("[tga profile] coverage: {}", summary.coverage_line());
    if !summary.is_complete() {
        warn!(
            skipped = summary.skipped.len(),
            "some periods were never reviewed; findings and trajectory cover a smaller sample"
        );
    }

    let output_dir = resolve_output_dir(&config, &args);
    let format: ReportFormat = args.format.parse().unwrap_or_else(|e| {
        warn!("unknown --format {}: {e} — using 'both'", args.format);
        ReportFormat::Both
    });
    let mut reporter = Reporter::new(&output_dir, format);
    if let Some(note) = summary.coverage_note() {
        reporter = reporter.with_coverage_note(note);
    }

    for path in reporter
        .write_profile(&profile)
        .with_context(|| format!("cannot write profile to {}", output_dir.display()))?
    {
        eprintln!("[tga profile] written: {}", path.display());
    }

    if args.github_issue {
        publish_issue(&config, &args, &profile, &reporter.render(&profile)).await?;
    }

    info!(
        contributor = %identity.canonical_email,
        periods = profile.periods.len(),
        reviewed = summary.reviewed,
        skipped = summary.skipped.len(),
        findings = profile.all_findings.len(),
        cost_usd = profile.token_cost.cost_usd,
        "profile complete"
    );
    Ok(())
}

// ─── Stages ───────────────────────────────────────────────────────────────────

/// Attach sampled diffs, warning rather than failing when they cannot be read.
///
/// A machine that has not cloned every repository in an org-wide database is the
/// normal case, so a profile without diffs is degraded, not wrong.
fn sample_diffs(
    batches: &mut [tga::profile::PeriodBatch],
    selector: &ContributorSelector,
    canonical_email: &str,
    args: &ProfileArgs,
) {
    let sampler = DiffSamplerConfig {
        max_diffs: args.max_diffs,
        repo_paths: std::collections::HashMap::new(),
        repos_root: args.repos_root.clone(),
    };
    eprintln!("[tga profile] sampling diffs…");
    if let Err(e) =
        sample_diffs_for_batches(batches, selector.database(), canonical_email, &sampler)
    {
        warn!("diff sampling failed: {e} — continuing without diffs");
        return;
    }
    let total: usize = batches.iter().map(|b| b.sampled_diffs.len()).sum();
    eprintln!("[tga profile] sampled {total} diff(s)");
}

/// Review every period, folding the outcomes into a [`PeriodRunSummary`].
///
/// Why: this is the first caller of `review_period`, and the loop below is the
/// reason `PeriodReview` is not a `Result` — one period's provider failure must
/// cost that period, not the run.
///
/// # Errors
///
/// Only credential/adapter resolution, which fails before any period is
/// attempted and would otherwise fail identically twelve times.
async fn review_periods(
    batches: &[tga::profile::PeriodBatch],
    profile: &mut ContributorProfile,
    model: &str,
) -> Result<PeriodRunSummary> {
    eprintln!("[tga profile] resolving model {model}…");
    let reviewer = PeriodReviewer::from_slug(model)
        .with_context(|| format!("cannot reach model '{model}' for the period review"))?;

    let mut summary = PeriodRunSummary::default();
    let mut per_period = Vec::with_capacity(batches.len());

    for batch in batches {
        let label = batch.stats.period_label.clone();
        eprintln!("[tga profile] reviewing {label}…");
        let review = reviewer.review_period(batch, &mut profile.token_cost).await;
        let skipped = review.was_skipped();
        let findings = summary.record(&label, review);
        if skipped {
            eprintln!("[tga profile]   {label}: SKIPPED — provider call failed");
        } else {
            eprintln!("[tga profile]   {label}: {} finding(s)", findings.len());
        }
        per_period.push(findings);
    }

    apply_deterministic_synthesis(profile, per_period, batches);
    Ok(summary)
}

/// Run the narrative pass, falling back rather than failing.
async fn narrate(profile: &mut ContributorProfile, model: &str) {
    eprintln!("[tga profile] synthesising across periods…");
    match Synthesizer::from_slug(model) {
        Ok(synth) => {
            if let Some(e) = synth.synthesize(profile).await {
                eprintln!("[tga profile] narrative unavailable ({e}) — using the fallback");
            }
        }
        Err(e) => {
            warn!("cannot resolve the narrative model: {e} — using the fallback");
            profile.narrative.clear();
            tga::profile::synthesizer::apply_fallback_narrative(profile);
        }
    }
}

/// Publish the rendered report to the contributor's GitHub issue thread.
///
/// # Errors
///
/// A malformed `--github-repo`. A missing token or a failed post is reported
/// and swallowed — the report is already on disk, and losing it to a GitHub
/// outage would be the larger failure.
async fn publish_issue(
    config: &Config,
    args: &ProfileArgs,
    profile: &ContributorProfile,
    markdown: &str,
) -> Result<()> {
    let Some(slug) = args.github_repo.as_deref() else {
        warn!("--github-issue needs --github-repo <owner/repo> — skipping the issue upsert");
        return Ok(());
    };
    let issue_config = GithubIssueConfig::from_slug(slug)?;

    let gh = config.github.clone().unwrap_or_default();
    if gh.token.is_none() {
        warn!("no github.token configured — skipping the issue upsert");
        return Ok(());
    }
    let client = GitHubClient::new_for_prs(
        &gh,
        vec![(issue_config.owner.clone(), issue_config.repo.clone())],
    )
    .context("cannot build a GitHub client for the issue upsert")?;

    eprintln!("[tga profile] upserting the GitHub issue thread…");
    match upsert_profile_issue(&client, &issue_config, profile, markdown).await {
        Ok(up) => {
            let verb = if up.created { "opened" } else { "appended to" };
            eprintln!("[tga profile] {verb} {}", up.html_url);
        }
        Err(e) => warn!("GitHub issue upsert failed: {e} — the report is still on disk"),
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Map the `--window` string onto a [`Window`].
///
/// An integer is read as a number of weeks; anything else falls back to
/// quarterly with a warning rather than aborting a run over a typo.
///
/// Test: `parse_window_variants`.
fn parse_window(s: &str) -> Window {
    match s.to_lowercase().as_str() {
        "quarterly" | "q" => Window::Quarterly,
        "monthly" | "m" => Window::Monthly,
        "weekly" | "w" => Window::Weekly,
        other => match other.parse::<u32>() {
            Ok(n) => Window::Custom(n),
            Err(_) => {
                warn!("unknown --window '{s}' — using quarterly");
                Window::Quarterly
            }
        },
    }
}

/// The repository list the report names: `--repos` when given, else every
/// repository the batches touched, sorted.
fn resolve_repositories(args: &ProfileArgs, batches: &[tga::profile::PeriodBatch]) -> Vec<String> {
    if let Some(filter) = &args.repos {
        return filter.clone();
    }
    let mut repos: Vec<String> = batches
        .iter()
        .flat_map(|b| b.stats.repositories.iter().cloned())
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

/// `--output`, else the configured output directory, else `./profiles`.
fn resolve_output_dir(config: &Config, args: &ProfileArgs) -> PathBuf {
    if let Some(dir) = &args.output {
        return expand_path(dir);
    }
    config
        .output
        .as_ref()
        .and_then(|o| o.directory.as_ref())
        .map(|d| expand_path(d).join("profiles"))
        .unwrap_or_else(|| PathBuf::from("profiles"))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper so `ProfileArgs` (a clap `Args`, not a `Parser`) can be parsed
    /// standalone in tests.
    #[derive(Parser, Debug)]
    struct Harness {
        #[command(flatten)]
        args: ProfileArgs,
    }

    fn parse(argv: &[&str]) -> ProfileArgs {
        Harness::try_parse_from(argv)
            .expect("parse should succeed")
            .args
    }

    /// Why: the defaults ARE the contract for a bare `tga profile <who>` — a
    /// silent change to any of them changes what an unflagged run does.
    /// What: parses the positional argument alone and asserts every default.
    /// Test: this test itself.
    #[test]
    fn profile_args_parse_defaults() {
        let args = parse(&["profile", "alice@example.com"]);
        assert_eq!(args.contributor, "alice@example.com");
        assert!(args.db.is_none());
        assert_eq!(args.window, "quarterly");
        assert_eq!(args.format, "both");
        assert_eq!(args.max_diffs, 10);
        assert!(!args.dry_run, "a bare run is not a dry run");
        assert!(
            !args.github_issue,
            "publishing to GitHub must never be the default"
        );
        assert!(args.model.is_none());
        assert!(
            DEFAULT_PROFILE_MODEL.contains('/'),
            "the default slug must carry a routing prefix"
        );
    }

    /// Why: a flag that parses to the wrong field silently profiles the wrong
    /// window or posts to the wrong repository.
    /// What: passes every flag and asserts each landed.
    /// Test: this test itself.
    #[test]
    fn profile_args_parse_all_flags() {
        let args = parse(&[
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
            "--model",
            "bedrock/us.anthropic.claude",
            "--github-issue",
            "--github-repo",
            "acme/profiles",
        ]);

        assert_eq!(args.db, Some(PathBuf::from("/tmp/org.tga.db")));
        assert_eq!(args.since.as_deref(), Some("2026-01-01"));
        assert_eq!(args.until.as_deref(), Some("2026-06-30"));
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
        assert_eq!(args.model.as_deref(), Some("bedrock/us.anthropic.claude"));
        assert!(args.github_issue);
        assert_eq!(args.github_repo.as_deref(), Some("acme/profiles"));
    }

    /// Why: an unrecognised window silently changing the period length would
    /// make two runs incomparable without saying so.
    /// What: asserts the named windows, a numeric one, and the typo fallback.
    /// Test: this test itself.
    #[test]
    fn parse_window_variants() {
        assert_eq!(parse_window("quarterly"), Window::Quarterly);
        assert_eq!(parse_window("Monthly"), Window::Monthly);
        assert_eq!(parse_window("w"), Window::Weekly);
        assert_eq!(parse_window("4"), Window::Custom(4));
        assert_eq!(parse_window("sprint"), Window::Quarterly);
    }
}
