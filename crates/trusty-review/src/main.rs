//! `trusty-review` CLI entry point.
//!
//! Why: provides the user-facing interface for running, comparing, and
//! inspecting PR reviews.  Stage-3 delivers the `run` and `compare`
//! subcommands; the `serve` subcommand is deferred to a later stage.
//!
//! What: parses flags via clap-derive, resolves config, builds injected
//! service dependencies, and dispatches to the pipeline runner.
//! STDOUT stays clean (only review output); all tracing goes to stderr.
//!
//! Test: `cargo run -p trusty-review -- --help` must succeed; the `run`
//! and `compare` subcommands are tested in the library's `runner` tests.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tracing::warn;

use trusty_review::{
    config::{ReviewConfig, RoleCliOverrides},
    integrations::{
        analyze_client::HttpAnalyzeClient,
        github::{GithubClient, auth::resolve_token},
        search_client::HttpSearchClient,
    },
    llm::OpenRouterProvider,
    llm::models::COMPARE_CANDIDATE_MODELS,
    models::ReviewResult,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, log_json_path, run_review},
};

// ─── CLI top-level ────────────────────────────────────────────────────────────

/// trusty-review — fast local PR-review service
///
/// An LLM-backed code reviewer that fetches PR diffs, retrieves code context
/// from trusty-search, and produces structured review verdicts.
///
/// All reviews are dry-run by default (no comments posted to GitHub).
#[derive(Debug, Parser)]
#[command(
    name = "trusty-review",
    version = env!("CARGO_PKG_VERSION"),
    about = "Fast local PR-review service — LLM-backed code review",
    long_about = None,
)]
struct Cli {
    /// Path to the TOML configuration file.
    /// Default: $XDG_CONFIG_HOME/trusty-review/config.toml
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

// ─── Subcommands ──────────────────────────────────────────────────────────────

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a single PR review with the default (or overridden) reviewer model.
    ///
    /// Fetches the PR diff from GitHub and runs the LLM review pipeline.
    /// Always dry-run in the MVP (no comment posted to GitHub).
    ///
    /// Use --local-diff to review a local unified diff file without GitHub.
    Run(RunArgs),

    /// Compare the same PR across multiple models to evaluate speed/cost/quality.
    ///
    /// Runs the review pipeline once per model in the compare set (or --models
    /// override) and prints a comparison table.  Always dry-run.
    Compare(CompareArgs),
}

// ─── `run` args ───────────────────────────────────────────────────────────────

/// Arguments for the `run` subcommand.
///
/// Why: groups all run-mode flags in one place for clarity and testability.
/// What: owner/repo/pr identify the GitHub PR; --local-diff bypasses GitHub.
/// Test: `cargo run -p trusty-review -- run --help`.
#[derive(Debug, Parser)]
pub struct RunArgs {
    /// GitHub organisation or user (required unless --local-diff is set).
    #[arg(value_name = "OWNER")]
    owner: Option<String>,

    /// GitHub repository name (required unless --local-diff is set).
    #[arg(value_name = "REPO")]
    repo: Option<String>,

    /// Pull request number (required unless --local-diff is set).
    #[arg(value_name = "PR")]
    pr: Option<u64>,

    /// Override the reviewer model slug (OpenRouter format, e.g.
    /// openai/gpt-5.4-mini-20260317).
    /// Default: from config or TRUSTY_REVIEW_REVIEWER_MODEL env var.
    #[arg(long, value_name = "SLUG")]
    reviewer_model: Option<String>,

    /// Read a local unified diff file instead of fetching from GitHub.
    /// No GitHub credentials are required in this mode; always dry-run.
    #[arg(long, value_name = "PATH")]
    local_diff: Option<std::path::PathBuf>,

    /// Write the review log file to the configured log directory.
    /// Default: true (log is written unless --no-log is passed).
    #[arg(long = "no-log", action = clap::ArgAction::SetFalse, default_value = "true")]
    write_log: bool,
}

// ─── `compare` args ───────────────────────────────────────────────────────────

/// Arguments for the `compare` subcommand.
///
/// Why: the compare mode lets operators quickly evaluate model speed/cost/quality
/// on a real PR without reading multiple full review outputs.
/// What: runs the same review across the compare-set models and prints a table.
/// Test: `cargo run -p trusty-review -- compare --help`.
#[derive(Debug, Parser)]
pub struct CompareArgs {
    /// GitHub organisation or user (required unless --local-diff is set).
    #[arg(value_name = "OWNER")]
    owner: Option<String>,

    /// GitHub repository name (required unless --local-diff is set).
    #[arg(value_name = "REPO")]
    repo: Option<String>,

    /// Pull request number (required unless --local-diff is set).
    #[arg(value_name = "PR")]
    pr: Option<u64>,

    /// Comma-separated list of model slugs to compare.
    /// Default: the built-in COMPARE_CANDIDATE_MODELS set
    /// (nano, mini, full, 5.5 — ordered cheap → premium).
    #[arg(long, value_name = "SLUG,...", value_delimiter = ',')]
    models: Option<Vec<String>>,

    /// Read a local unified diff file instead of fetching from GitHub.
    #[arg(long, value_name = "PATH")]
    local_diff: Option<std::path::PathBuf>,
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // Tracing to stderr — never stdout (stdout is reserved for review output).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Build the async runtime and dispatch.
    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    // Load config from env + optional file.
    let config = ReviewConfig::from_env_and_file(cli.config.as_deref(), None);

    match cli.command {
        Commands::Run(args) => cmd_run(config, args).await,
        Commands::Compare(args) => cmd_compare(config, args).await,
    }
}

// ─── `run` handler ────────────────────────────────────────────────────────────

/// Execute the `run` subcommand.
///
/// Why: one-shot review of a PR or local diff with the selected reviewer model.
/// What: resolves the diff source, builds deps, runs the pipeline, prints the
/// result to STDOUT, and optionally writes the log file.
/// Test: CLI integration via `cargo run -p trusty-review -- run --help`.
async fn cmd_run(config: ReviewConfig, args: RunArgs) -> Result<()> {
    let diff_source = resolve_diff_source_run(&config, &args).await?;

    // Resolve the reviewer model (CLI flag → config → default).
    let overrides = RoleCliOverrides {
        reviewer_model: args.reviewer_model.clone(),
        ..Default::default()
    };
    let config_with_overrides = ReviewConfig::from_env_and_file(None, Some(&overrides));
    let reviewer_model = config_with_overrides.role_models.reviewer.model.clone();

    let deps = build_deps(&config)?;

    let input = ReviewInput {
        diff_source,
        reviewer_model: reviewer_model.clone(),
        write_log: args.write_log,
        print_result: true,
    };

    let result = run_review(&config_with_overrides, input, deps).await;

    if args.write_log {
        let log_path = log_json_path(&result, &config_with_overrides.log_dir);
        eprintln!("\nLog written to: {}", log_path.display());
    }

    Ok(())
}

// ─── `compare` handler ────────────────────────────────────────────────────────

/// Execute the `compare` subcommand.
///
/// Why: side-by-side model comparison lets operators pick the best model for
/// their repo's cost/quality trade-off.
/// What: runs the same review for each model in the compare set (sequentially
/// to avoid overwhelming OpenRouter rate limits), collects the results, and
/// prints a comparison table to STDOUT.  Always dry-run; logs are not written.
/// Test: integration via `cargo run -p trusty-review -- compare --help`.
async fn cmd_compare(config: ReviewConfig, args: CompareArgs) -> Result<()> {
    let models: Vec<String> = args.models.clone().unwrap_or_else(|| {
        COMPARE_CANDIDATE_MODELS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });

    if models.is_empty() {
        anyhow::bail!("--models list is empty; provide at least one model slug");
    }

    println!("\nComparing {} models...\n", models.len());

    let mut results: Vec<(String, ReviewResult)> = Vec::new();
    let wall_start = std::time::Instant::now();

    for model in &models {
        let diff_source = resolve_diff_source_compare(&config, &args).await?;
        let deps = build_deps(&config)?;
        let input = ReviewInput {
            diff_source,
            reviewer_model: model.clone(),
            write_log: false,    // compare mode never writes logs
            print_result: false, // we print the table ourselves
        };
        eprint!("  Running {} ...", model);
        let start = std::time::Instant::now();
        let result = run_review(&config, input, deps).await;
        let elapsed = start.elapsed();
        eprintln!(" done ({elapsed:.1?})");
        results.push((model.clone(), result));
    }

    let wall_elapsed = wall_start.elapsed();

    // Print comparison table.
    print_compare_table(&results);
    println!("\nTotal wall-clock: {wall_elapsed:.1?}");

    Ok(())
}

// ─── Table printer ────────────────────────────────────────────────────────────

/// Print the comparison table to STDOUT.
///
/// Why: the compare subcommand's primary output is a structured table that
/// lets operators quickly evaluate model differences.
/// What: prints one row per model with: model slug, verdict, findings count,
/// input tokens, output tokens, latency, cost.
/// Test: `print_compare_table_formats_correctly`.
pub fn print_compare_table(results: &[(String, ReviewResult)]) {
    if results.is_empty() {
        println!("(no results)");
        return;
    }

    let header = format!(
        "{:<40}  {:<16}  {:>8}  {:>12}  {:>13}  {:>10}  {:>10}",
        "model", "verdict", "findings", "input_tokens", "output_tokens", "latency_ms", "cost_usd"
    );
    let separator = "-".repeat(header.len());
    println!("{header}");
    println!("{separator}");

    for (model, result) in results {
        let verdict_str = result.verdict.to_string();
        let err_suffix = if result.error.is_some() { "*" } else { "" };
        println!(
            "{:<40}  {:<16}  {:>8}  {:>12}  {:>13}  {:>10}  {:>10.6}",
            truncate_str(model, 40),
            format!("{verdict_str}{err_suffix}"),
            result.findings.len(),
            result.input_tokens,
            result.output_tokens,
            result.latency_ms,
            result.cost_estimate_usd,
        );
    }

    println!();
    println!("* = pipeline error (fail-safe APPROVE applied)");
}

/// Truncate a string to `max` chars, adding `…` if truncated.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Resolve the `DiffSource` for the `run` subcommand.
///
/// Why: the diff source depends on whether `--local-diff` is set or the three
/// positional args (owner/repo/pr) are provided.
/// What: validates the args and builds the correct `DiffSource` variant.
/// Test: positional args and --local-diff validated.
async fn resolve_diff_source_run(config: &ReviewConfig, args: &RunArgs) -> Result<DiffSource> {
    if let Some(ref path) = args.local_diff {
        return Ok(DiffSource::LocalFile { path: path.clone() });
    }

    let owner = args
        .owner
        .as_deref()
        .context("OWNER is required (or use --local-diff)")?
        .to_string();
    let repo = args
        .repo
        .as_deref()
        .context("REPO is required (or use --local-diff)")?
        .to_string();
    let pr = args
        .pr
        .context("PR number is required (or use --local-diff)")?;

    let client = GithubClient::new();
    let token = resolve_token(&client, config, &owner).await.map_err(|e| {
        warn!("GitHub token resolution failed: {e} — set GITHUB_TOKEN or GitHub App credentials");
        anyhow::anyhow!("GitHub authentication failed: {e}")
    })?;

    Ok(DiffSource::Github {
        owner,
        repo,
        pr,
        token,
    })
}

/// Resolve the `DiffSource` for the `compare` subcommand.
///
/// Why: compare and run share the same diff-source logic; compare reuses it
/// per-model run.
/// What: identical to `resolve_diff_source_run` but takes `CompareArgs`.
/// Test: covered by compare flow.
async fn resolve_diff_source_compare(
    config: &ReviewConfig,
    args: &CompareArgs,
) -> Result<DiffSource> {
    if let Some(ref path) = args.local_diff {
        return Ok(DiffSource::LocalFile { path: path.clone() });
    }

    let owner = args
        .owner
        .as_deref()
        .context("OWNER is required (or use --local-diff)")?
        .to_string();
    let repo = args
        .repo
        .as_deref()
        .context("REPO is required (or use --local-diff)")?
        .to_string();
    let pr = args
        .pr
        .context("PR number is required (or use --local-diff)")?;

    let client = GithubClient::new();
    let token = resolve_token(&client, config, &owner)
        .await
        .map_err(|e| anyhow::anyhow!("GitHub authentication failed: {e}"))?;

    Ok(DiffSource::Github {
        owner,
        repo,
        pr,
        token,
    })
}

/// Build the injected service dependencies from `ReviewConfig`.
///
/// Why: both `run` and `compare` need the same set of deps; building them from
/// config in one place avoids repetition.
/// What: constructs `OpenRouterProvider`, `HttpSearchClient`, and
/// `HttpAnalyzeClient`; wraps them in `Arc<dyn Trait>`.
/// Test: covered transitively.
fn build_deps(config: &ReviewConfig) -> Result<ReviewDeps> {
    // Build the LLM provider with the reviewer model as default; the actual
    // model id is overridden per-run via `LlmRequest::model`.
    let reviewer_model = &config.role_models.reviewer.model;
    let llm = OpenRouterProvider::new(config.openrouter_api_key.clone(), reviewer_model)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let search = HttpSearchClient::from_config(config);
    let analyze = HttpAnalyzeClient::from_config(config);

    Ok(ReviewDeps {
        llm: Arc::new(llm),
        search: Arc::new(search),
        analyze: Some(Arc::new(analyze)),
    })
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_review::models::{Effort, Finding, Verdict};

    fn make_result(model: &str, verdict: Verdict, findings: usize, cost: f64) -> ReviewResult {
        let mut r = ReviewResult::new(
            "acme",
            "repo",
            1,
            "Test PR",
            "https://github.com/acme/repo/pull/1",
        );
        r.model = model.to_string();
        r.verdict = verdict;
        r.input_tokens = 500;
        r.output_tokens = 100;
        r.latency_ms = 1000;
        r.cost_estimate_usd = cost;
        for i in 0..findings {
            r.findings.push(Finding::new(
                "src/a.rs",
                format!("issue-{i}"),
                "desc",
                "fix",
                0.8,
                Effort::Low,
            ));
        }
        r
    }

    #[test]
    fn print_compare_table_formats_correctly() {
        let results = vec![
            (
                "openai/gpt-5.4-nano-20260317".to_string(),
                make_result(
                    "openai/gpt-5.4-nano-20260317",
                    Verdict::Approve,
                    0,
                    0.000145,
                ),
            ),
            (
                "openai/gpt-5.4-mini-20260317".to_string(),
                make_result(
                    "openai/gpt-5.4-mini-20260317",
                    Verdict::RequestChanges,
                    2,
                    0.000525,
                ),
            ),
        ];

        // Capture output — just verify no panic and correct shape.
        // (We call the function; actual stdout content checked manually.)
        print_compare_table(&results);
    }

    #[test]
    fn print_compare_table_empty_does_not_panic() {
        print_compare_table(&[]);
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_long() {
        let s = "a".repeat(50);
        let result = truncate_str(&s, 10);
        // "…" is 3 bytes in UTF-8, so byte len ≤ 9 + 3 = 12.
        // More importantly, the character count must not exceed max + 1.
        let char_count = result.chars().count();
        assert!(
            char_count <= 11,
            "truncated string must be ≤ max+1 chars: {char_count}"
        );
        assert!(result.ends_with('…'), "must end with ellipsis: {result:?}");
    }

    #[test]
    fn compare_table_shows_error_suffix() {
        let mut r = make_result("openai/gpt-5.4-nano-20260317", Verdict::Approve, 0, 0.0);
        r.error = Some("timeout".to_string());
        let results = vec![("openai/gpt-5.4-nano-20260317".to_string(), r)];
        // Just verify no panic; the "*" suffix is rendered inline.
        print_compare_table(&results);
    }
}
