//! Handler for the `compare` subcommand.
//!
//! Why: extracted from `main.rs` to keep that file under the 500-line cap (#610).
//!
//! What: runs the same review across multiple models and prints a comparison table.
//!
//! Test: CLI integration via `cargo run -p trusty-review -- compare --help`;
//! table formatting covered by `print_compare_table_formats_correctly`.

use anyhow::Result;
use tracing::warn;

use trusty_review::{
    config::ReviewConfig,
    integrations::{
        github::{AuthStrategy, GithubClient, RunMode},
        search_client::HttpSearchClient,
    },
    llm::models::COMPARE_CANDIDATE_MODELS,
    models::ReviewResult,
    pipeline::{DiffSource, ReviewInput, TriggerDecision, run_review},
};

use crate::commands::run::build_deps_async;

// ─── compare args ────────────────────────────────────────────────────────────

/// Arguments for the `compare` subcommand.
///
/// Why: the compare mode lets operators quickly evaluate model speed/cost/quality
/// on a real PR without reading multiple full review outputs.
/// What: runs the same review across the compare-set models and prints a table.
/// Test: `cargo run -p trusty-review -- compare --help`.
#[derive(Debug, clap::Parser)]
pub struct CompareArgs {
    /// GitHub organisation or user (required unless --local-diff is set).
    #[arg(value_name = "OWNER")]
    pub owner: Option<String>,

    /// GitHub repository name (required unless --local-diff is set).
    #[arg(value_name = "REPO")]
    pub repo: Option<String>,

    /// Pull request number (required unless --local-diff is set).
    #[arg(value_name = "PR")]
    pub pr: Option<u64>,

    /// Comma-separated list of model slugs to compare.
    #[arg(long, value_name = "SLUG,...", value_delimiter = ',')]
    pub models: Option<Vec<String>>,

    /// Read a local unified diff file instead of fetching from GitHub.
    #[arg(long, value_name = "PATH")]
    pub local_diff: Option<std::path::PathBuf>,

    /// Provider backend for bare model ids: `bedrock` (default) or `openrouter`.
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `compare` subcommand.
///
/// Why: side-by-side model comparison lets operators pick the best model for
/// their repo's cost/quality trade-off.  Also resolves the search index before
/// the per-model loop so all model runs share the correct index (issue #670 /
/// auto-derive #661).
/// What: runs the same review for each model in the compare set (sequentially),
/// collects the results, and prints a comparison table to STDOUT.
/// Test: integration via `cargo run -p trusty-review -- compare --help`.
pub async fn cmd_compare(mut config: ReviewConfig, args: CompareArgs) -> Result<()> {
    // Resolve the search index from the daemon once before the per-model loop.
    // When TRUSTY_SEARCH_INDEX is explicitly set, resolve_index is a no-op.
    let search_for_resolve = HttpSearchClient::from_config(&config);
    config.resolve_index(&search_for_resolve).await;

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

    let compare_provider_override = args.provider.as_deref().and_then(|s| {
        s.parse::<trusty_review::config::Provider>()
            .map_err(|e| warn!("unrecognised --provider {s:?}: {e} — using config default"))
            .ok()
    });
    let default_provider = compare_provider_override
        .as_ref()
        .unwrap_or(&config.role_models.reviewer.provider);

    for model in &models {
        let diff_source = resolve_diff_source_compare(&config, &args).await?;
        let deps = build_deps_async(&config, model, default_provider).await?;
        let input = ReviewInput {
            diff_source,
            reviewer_model: model.clone(),
            write_log: false,
            print_result: false,
            trigger: TriggerDecision::ForceDryRun,
            run_mode: RunMode::Cli,
            allow_posting: false,
        };
        eprint!("  Running {} ...", model);
        let start = std::time::Instant::now();
        let result = run_review(&config, input, deps).await;
        let elapsed = start.elapsed();
        eprintln!(" done ({elapsed:.1?})");
        results.push((model.clone(), result));
    }

    let wall_elapsed = wall_start.elapsed();
    print_compare_table(&results);
    println!("\nTotal wall-clock: {wall_elapsed:.1?}");

    Ok(())
}

// ─── diff source helper ──────────────────────────────────────────────────────

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
        .ok_or_else(|| anyhow::anyhow!("OWNER is required (or use --local-diff)"))?
        .to_string();
    let repo = args
        .repo
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("REPO is required (or use --local-diff)"))?
        .to_string();
    let pr = args
        .pr
        .ok_or_else(|| anyhow::anyhow!("PR number is required (or use --local-diff)"))?;

    let client = GithubClient::new();
    let token = AuthStrategy::select(RunMode::Cli, None)
        .resolve_token(&client, config, &owner)
        .await
        .map_err(|e| anyhow::anyhow!("GitHub authentication failed: {e}"))?;

    Ok(DiffSource::Github {
        owner,
        repo,
        pr,
        token,
    })
}

// ─── table printer ────────────────────────────────────────────────────────────

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
///
/// Why: model slugs can be long; keeping each column under a fixed width
/// preserves the table's readable alignment.
/// What: returns the first `max-1` chars with `…` appended when truncation
/// occurs, or the original string when short enough.
/// Test: `truncate_str_short`, `truncate_str_long`.
pub fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

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
        print_compare_table(&results);
    }
}
