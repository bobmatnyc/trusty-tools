//! Handler for the `run` subcommand.
//!
//! Why: extracted from `main.rs` to keep that file under the 500-line cap (#610).
//!
//! What: resolves the diff source, builds deps, runs the review pipeline,
//! optionally writes the log file, and exits non-zero on a skipped review.
//!
//! Test: CLI integration via `cargo run -p trusty-review -- run --help`;
//! pipeline logic covered by `runner::tests`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use tracing::warn;

use trusty_review::{
    config::{ReviewConfig, RoleCliOverrides},
    integrations::{
        github::{AuthStrategy, GithubClient, RunMode},
        search_client::HttpSearchClient,
        subprocess_analyze_client::SubprocessAnalyzeClient,
    },
    llm::build_provider,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, TriggerDecision, log_json_path, run_review},
};

use crate::cli_verify;

// ─── run args (re-used by compare) ─────────────────────────────────────────

/// Arguments for the `run` subcommand.
///
/// Why: groups all run-mode flags in one place for clarity and testability.
/// What: owner/repo/pr identify the GitHub PR; --local-diff bypasses GitHub.
/// Test: `cargo run -p trusty-review -- run --help`.
#[derive(Debug, clap::Parser)]
pub struct RunArgs {
    /// GitHub organisation or user (required unless --local-diff is set).
    #[arg(value_name = "OWNER")]
    pub owner: Option<String>,

    /// GitHub repository name (required unless --local-diff is set).
    #[arg(value_name = "REPO")]
    pub repo: Option<String>,

    /// Pull request number (required unless --local-diff is set).
    #[arg(value_name = "PR")]
    pub pr: Option<u64>,

    /// Override the reviewer model slug.
    /// Accepts bare ids (uses default/selected provider), a `bedrock/<id>`
    /// prefix to force AWS Bedrock, or an `openrouter/<id>` prefix to force
    /// OpenRouter.
    #[arg(long, value_name = "SLUG")]
    pub reviewer_model: Option<String>,

    /// Provider backend: `bedrock` (default) or `openrouter`.
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// Read a local unified diff file instead of fetching from GitHub.
    #[arg(long, value_name = "PATH")]
    pub local_diff: Option<std::path::PathBuf>,

    /// Write the review log file to the configured log directory.
    #[arg(long = "no-log", action = clap::ArgAction::SetFalse, default_value = "true")]
    pub write_log: bool,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `run` subcommand.
///
/// Why: one-shot review of a PR or local diff with the selected reviewer model.
/// What: resolves the diff source, builds deps, runs the pipeline, prints the
/// result to STDOUT, and optionally writes the log file.  Calls `resolve_index`
/// before the pipeline so the correct trusty-search index is used even when
/// `TRUSTY_SEARCH_INDEX` is unset (issue #670 / auto-derive #661).
/// Test: CLI integration via `cargo run -p trusty-review -- run --help`;
/// `resolve_index` wiring covered by `cmd_run_resolve_index_called` in
/// `commands/run_tests.rs`.
pub async fn cmd_run(config: ReviewConfig, args: RunArgs) -> Result<()> {
    let diff_source = resolve_diff_source_run(&config, &args).await?;

    let overrides = RoleCliOverrides {
        reviewer_model: args.reviewer_model.clone(),
        provider: args.provider.clone(),
        ..Default::default()
    };
    let mut config_with_overrides = ReviewConfig::from_env_and_file(None, Some(&overrides));
    // Clone both to avoid holding a borrow across the mutable resolve_index call.
    let reviewer_model = config_with_overrides.role_models.reviewer.model.clone();
    let default_provider = config_with_overrides.role_models.reviewer.provider.clone();

    // Resolve the search index from the daemon before building deps so the
    // correct index is used even when TRUSTY_SEARCH_INDEX is not set.
    // When the operator set TRUSTY_SEARCH_INDEX explicitly, resolve_index is
    // a no-op.  On any failure (daemon unreachable, no match) it logs a
    // warning and leaves search_index at its current value.
    let search_for_resolve = HttpSearchClient::from_config(&config_with_overrides)
        .map_err(|e| anyhow::anyhow!("failed to build search HTTP client: {e}"))?;
    config_with_overrides
        .resolve_index(&search_for_resolve)
        .await;

    let deps = build_deps_async(&config_with_overrides, &reviewer_model, &default_provider).await?;

    let input = ReviewInput {
        diff_source,
        reviewer_model: reviewer_model.clone(),
        write_log: args.write_log,
        print_result: true,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: true,
    };

    let result = run_review(&config_with_overrides, input, deps).await;

    if args.write_log {
        let log_path = log_json_path(&result, &config_with_overrides.log_dir);
        eprintln!("\nLog written to: {}", log_path.display());
    }

    if result.status.is_skipped() {
        anyhow::bail!(
            "review skipped — {}",
            result
                .error
                .as_deref()
                .unwrap_or("required code-context dependency unavailable")
        );
    }

    Ok(())
}

// ─── shared helpers ──────────────────────────────────────────────────────────

/// Resolve the `DiffSource` for the `run` subcommand.
///
/// Why: the diff source depends on whether `--local-diff` is set or the three
/// positional args (owner/repo/pr) are provided.
/// What: validates the args and builds the correct `DiffSource` variant.
/// Test: positional args and --local-diff validated.
pub async fn resolve_diff_source_run(config: &ReviewConfig, args: &RunArgs) -> Result<DiffSource> {
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

    let client = GithubClient::new()
        .map_err(|e| anyhow::anyhow!("failed to build GitHub HTTP client: {e}"))?;
    let token = AuthStrategy::select(RunMode::Cli, None)
        .resolve_token(&client, config, &owner)
        .await
        .map_err(|e| {
            warn!(
                "GitHub token resolution failed: {e} — set GITHUB_TOKEN/GH_TOKEN or run `gh auth login`"
            );
            anyhow::anyhow!("GitHub authentication failed: {e}")
        })?;

    Ok(DiffSource::Github {
        owner,
        repo,
        pr,
        token,
    })
}

/// Build the injected service dependencies from `ReviewConfig` and a model id.
///
/// Why: both `run` and `compare` need the same set of deps; building them from
/// config in one place avoids repetition.  Async because `BedrockProvider::new`
/// loads AWS credentials asynchronously.
/// What: uses `build_provider` (which resolves the `bedrock/`/`openrouter/`
/// prefix), builds the optional verifier, constructs search/analyze clients.
/// Test: covered transitively by runner tests that inject a FakeLlm.
pub async fn build_deps_async(
    config: &ReviewConfig,
    model: &str,
    default_provider: &trusty_review::config::Provider,
) -> Result<ReviewDeps> {
    let llm = build_provider(model, default_provider, &config.openrouter_api_key)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let verifier = cli_verify::build_verifier_opt(config).await;

    let search = HttpSearchClient::from_config(config)
        .map_err(|e| anyhow::anyhow!("failed to build search HTTP client: {e}"))?;
    // Use the on-demand subprocess client instead of the HTTP daemon client.
    // Rationale: #632 — trusty-analyze is invoked on demand as a subprocess
    // (trusty-analyze review --index-id <id> -) rather than requiring a
    // long-running trusty-analyze serve daemon.
    let analyze = SubprocessAnalyzeClient::from_config(config)
        .map_err(|e| anyhow::anyhow!("failed to build analyze HTTP client: {e}"))?;

    Ok(ReviewDeps {
        llm,
        verifier,
        search: Arc::new(search),
        analyze: Some(Arc::new(analyze)),
        dedup: None,
    })
}
