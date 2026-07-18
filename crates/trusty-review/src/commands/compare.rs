//! Handler for the `compare` subcommand.
//!
//! Why: extracted from `main.rs` to keep that file under the 500-line cap (#610).
//!
//! What: runs the same review across multiple models and prints a comparison table.
//!
//! Test: CLI integration via `cargo run -p trusty-review -- compare --help`;
//! table formatting covered by `print_compare_table_formats_correctly`.

use anyhow::{Context as _, Result};
use tracing::warn;

use trusty_review::{
    config::ReviewConfig,
    integrations::{
        github::{AuthStrategy, GithubClient, RunMode},
        search_client::HttpSearchClient,
    },
    llm::models::COMPARE_CANDIDATE_MODELS,
    models::ReviewResult,
    pipeline::{CallerContext, DiffSource, ReviewInput, TriggerDecision, run_review},
};

use crate::commands::diff_source::{LocalDiffFlags, resolve_local_diff_source};
use crate::commands::run::{apply_source_root_fallback, build_deps_async, resolve_source_root_arg};

// ─── compare args ────────────────────────────────────────────────────────────

/// Arguments for the `compare` subcommand.
///
/// Why: the compare mode lets operators quickly evaluate model speed/cost/quality
/// on a real PR without reading multiple full review outputs.
/// What: runs the same review across the compare-set models and prints a table.
/// Test: `cargo run -p trusty-review -- compare --help`.
#[derive(Debug, clap::Parser)]
pub struct CompareArgs {
    /// GitHub organisation or user (required unless --local-diff/--base is set).
    #[arg(value_name = "OWNER")]
    pub owner: Option<String>,

    /// GitHub repository name (required unless --local-diff/--base is set).
    #[arg(value_name = "REPO")]
    pub repo: Option<String>,

    /// Pull request number (required unless --local-diff/--base is set).
    #[arg(value_name = "PR")]
    pub pr: Option<u64>,

    /// Comma-separated list of model slugs to compare.
    #[arg(long, value_name = "SLUG,...", value_delimiter = ',')]
    pub models: Option<Vec<String>>,

    /// Read a local unified diff file instead of fetching from GitHub.
    /// Pass `-` to read the unified diff from stdin instead of a file.
    #[arg(long, value_name = "PATH", conflicts_with = "base")]
    pub local_diff: Option<std::path::PathBuf>,

    /// Diff the local git repository (current directory) from this ref instead
    /// of fetching from GitHub — `git diff -M <base>...<head>` (three-dot
    /// merge-base range). Mutually exclusive with --local-diff.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,

    /// Head ref for --base; defaults to HEAD (the last commit, not the
    /// working tree). Requires --base.
    #[arg(long, value_name = "REF", requires = "base")]
    pub head: Option<String>,

    /// Provider backend for bare model ids: `bedrock` (default) or `openrouter`.
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// Explicit source-root directory for code-context retrieval (issue #2994).
    /// See `RunArgs::source_root` for full semantics; applies to every model
    /// compared in this run.
    #[arg(long, value_name = "DIR")]
    pub source_root: Option<std::path::PathBuf>,

    /// Name of a review template to append as a prompt addendum (issue #2995).
    /// See `RunArgs::review_template` for the resolution and layering details;
    /// applied identically here — highest-precedence override for every model
    /// run in the compare set.
    #[arg(long, value_name = "NAME")]
    pub review_template: Option<String>,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `compare` subcommand.
///
/// Why: side-by-side model comparison lets operators pick the best model for
/// their repo's cost/quality trade-off.  Also resolves the search index before
/// the per-model loop so all model runs share the correct index (issue #670 /
/// auto-derive #661), and resolves `--source-root` (issue #2994) and
/// `--review-template` (issue #2995) once so every model shares the same
/// diff-only-or-matched context and template overrides.
/// What: runs the same review for each model in the compare set (sequentially),
/// collects the results, and prints a comparison table to STDOUT.
/// Test: integration via `cargo run -p trusty-review -- compare --help`;
/// `--source-root` clap wiring covered by `compare_args_source_root_parses`;
/// `--review-template` covered by clap args parsing.
pub async fn cmd_compare(mut config: ReviewConfig, args: CompareArgs) -> Result<()> {
    let search_for_resolve = HttpSearchClient::from_config(&config)
        .map_err(|e| anyhow::anyhow!("failed to build search HTTP client: {e}"))?;

    // ── --source-root (issue #2994) ────────────────────────────────────────
    // Resolved BEFORE the CWD/env auto-derive below, once for the whole
    // compare run — see `resolve_source_root_arg`'s doc comment for the exact
    // precedence rule (TRUSTY_SEARCH_INDEX always wins).
    let source_root_notice = resolve_source_root_arg(
        &mut config,
        &search_for_resolve,
        args.source_root.as_deref(),
    )
    .await;

    // Resolve the search index from the daemon once before the per-model loop.
    // When TRUSTY_SEARCH_INDEX is explicitly set, resolve_index is a no-op.
    config.resolve_index(&search_for_resolve).await;

    // `--review-template` is the highest-precedence override (issue #2995);
    // apply it directly to the already-resolved config, matching how
    // `--provider` is handled locally below rather than re-deriving the whole
    // config (compare, unlike `run`, does not rebuild `config` from scratch).
    if let Some(name) = args.review_template.clone() {
        config.review_template = Some(name);
    }

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

    // Resolved ONCE, outside the per-model loop: every model compares against
    // the same diff. `materialize_stdin_if_needed` additionally guards against
    // stdin only being readable once — see its doc comment (issue #2993).
    let diff_source = resolve_diff_source_compare(&config, &args).await?;
    // Keeps the tempfile alive for the whole loop when stdin was materialized;
    // dropped (and deleted) when `cmd_compare` returns. `_stdin_tmp` itself is
    // never read — it exists only to extend the tempfile's lifetime.
    let (diff_source, _stdin_tmp) = materialize_stdin_if_needed(diff_source)?;

    for model in &models {
        let mut deps = build_deps_async(&config, model, default_provider).await?;
        apply_source_root_fallback(&mut deps, source_root_notice.as_deref());
        let input = ReviewInput {
            diff_source: diff_source.clone(),
            reviewer_model: model.clone(),
            write_log: false,
            print_result: false,
            trigger: TriggerDecision::ForceDryRun,
            run_mode: RunMode::Cli,
            allow_posting: false,
            caller_context: CallerContext::default(),
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

// ─── diff source helpers ──────────────────────────────────────────────────────

/// Resolve the `DiffSource` for the `compare` subcommand.
///
/// Why: compare and run share the same diff-source logic; compare resolves it
/// once and reuses it across every model run (see `cmd_compare`).
/// What: delegates local-mode selection (`LocalFile`/`Stdin`/`GitRange`) to
/// the shared `resolve_local_diff_source` (issue #2993); falls back to
/// resolving a GitHub PR source from the positional args + a resolved token.
/// Test: local-mode selection is covered by `diff_source::tests`;
/// `materialize_stdin_if_needed_passes_through_non_stdin` covers the stdin
/// safety net this feeds into.
async fn resolve_diff_source_compare(
    config: &ReviewConfig,
    args: &CompareArgs,
) -> Result<DiffSource> {
    if let Some(source) = resolve_local_diff_source(LocalDiffFlags {
        local_diff: args.local_diff.as_deref(),
        base: args.base.as_deref(),
        head: args.head.as_deref(),
    })? {
        return Ok(source);
    }

    let owner = args
        .owner
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("OWNER is required (or use --local-diff / --base)"))?
        .to_string();
    let repo = args
        .repo
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("REPO is required (or use --local-diff / --base)"))?
        .to_string();
    let pr = args
        .pr
        .ok_or_else(|| anyhow::anyhow!("PR number is required (or use --local-diff / --base)"))?;

    let client = GithubClient::new()
        .map_err(|e| anyhow::anyhow!("failed to build GitHub HTTP client: {e}"))?;
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

/// Materialize a `Stdin` diff source into a tempfile-backed `LocalFile`.
///
/// Why: `compare` runs the SAME diff through every model in a loop, and each
/// iteration's `run_review` independently calls `load_diff`. Stdin can only be
/// read to EOF once — a second read after EOF returns `Ok("")`, not an error —
/// so without this, every model after the first would silently compare an
/// EMPTY diff instead of failing loudly (issue #2993). Reading stdin once up
/// front and handing every iteration a re-readable `LocalFile` (the same
/// mechanism `--local-diff <PATH>` already uses) closes that gap.
/// What: a no-op for every other `DiffSource` variant (returned unchanged,
/// `None` guard); for `Stdin`, reads stdin to EOF, writes it to a `NamedTempFile`,
/// and returns `(LocalFile, Some(guard))` — the caller must keep the guard
/// alive for as long as the source is used; the file is deleted when it drops.
/// Test: `materialize_stdin_if_needed_passes_through_non_stdin`,
/// `materialize_stdin_if_needed_git_range_passes_through`. The `Stdin` branch
/// itself needs a real piped stdin and is exercised manually
/// (`git diff | trusty-review compare --local-diff - --models a,b`);
/// `pipeline::diff::read_diff_from_reader` covers the underlying read loop
/// with an in-memory `Cursor` instead of real stdin.
fn materialize_stdin_if_needed(
    source: DiffSource,
) -> Result<(DiffSource, Option<tempfile::NamedTempFile>)> {
    if !matches!(source, DiffSource::Stdin) {
        return Ok((source, None));
    }
    use std::io::{Read as _, Write as _};
    let mut buf = String::new();
    std::io::stdin()
        .lock()
        .read_to_string(&mut buf)
        .context("failed to read diff from stdin")?;
    let mut tmp =
        tempfile::NamedTempFile::new().context("failed to create tempfile for stdin diff")?;
    tmp.write_all(buf.as_bytes())
        .context("failed to write stdin diff to tempfile")?;
    let path = tmp.path().to_path_buf();
    Ok((DiffSource::LocalFile { path }, Some(tmp)))
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
    use clap::Parser as _;
    use trusty_review::models::{Effort, Finding, Verdict};

    // ── CompareArgs clap wiring (#2993) ─────────────────────────────────

    /// Guards the `conflicts_with = "base"` wiring on `local_diff`: a
    /// field-id rename that silently drops the attribute would otherwise
    /// only be caught by manual testing.
    #[test]
    fn compare_args_base_and_local_diff_conflict() {
        let result =
            CompareArgs::try_parse_from(["compare", "--base", "main", "--local-diff", "x.diff"]);
        assert!(
            result.is_err(),
            "--base and --local-diff must be mutually exclusive"
        );
    }

    /// Guards the `requires = "base"` wiring on `head`.
    #[test]
    fn compare_args_head_without_base_errors() {
        let result = CompareArgs::try_parse_from(["compare", "--head", "feature"]);
        assert!(result.is_err(), "--head requires --base");
    }

    #[test]
    fn compare_args_base_and_head_parses() {
        let args = CompareArgs::try_parse_from(["compare", "--base", "main", "--head", "feature"])
            .expect("parse");
        assert_eq!(args.base.as_deref(), Some("main"));
        assert_eq!(args.head.as_deref(), Some("feature"));
    }

    /// `--source-root <dir>` must parse on `compare` the same way it does on
    /// `run` (issue #2994).
    /// Why: guards the clap wiring; a typo'd `#[arg]` attribute would only
    /// otherwise surface as a runtime "unrecognised flag" error.
    /// What: parses `compare --source-root /tmp/proj` and asserts the field.
    /// Test: this test.
    #[test]
    fn compare_args_source_root_parses() {
        let args =
            CompareArgs::try_parse_from(["compare", "--source-root", "/tmp/proj"]).expect("parse");
        assert_eq!(
            args.source_root.as_deref(),
            Some(std::path::Path::new("/tmp/proj"))
        );
    }

    /// `--source-root` must default to `None` when absent (zero-regression).
    /// Why: proves adding the flag does not change parsing of any existing
    /// `compare` invocation.
    /// What: parses `compare --base main` (no `--source-root`) and asserts
    /// `None`.
    /// Test: this test.
    #[test]
    fn compare_args_source_root_absent_is_none() {
        let args = CompareArgs::try_parse_from(["compare", "--base", "main"]).expect("parse");
        assert!(
            args.source_root.is_none(),
            "--source-root must default to None when not passed"
        );
    }

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

    // ── materialize_stdin_if_needed (#2993) ────────────────────────────

    #[test]
    fn materialize_stdin_if_needed_passes_through_non_stdin() {
        let source = DiffSource::LocalFile {
            path: std::path::PathBuf::from("/tmp/some.diff"),
        };
        let (result, tmp) =
            materialize_stdin_if_needed(source).expect("non-stdin must never read stdin");
        assert!(tmp.is_none(), "no tempfile should be created for LocalFile");
        match result {
            DiffSource::LocalFile { path } => {
                assert_eq!(path, std::path::PathBuf::from("/tmp/some.diff"));
            }
            other => panic!("expected LocalFile to pass through unchanged, got {other:?}"),
        }
    }

    #[test]
    fn materialize_stdin_if_needed_git_range_passes_through() {
        let source = DiffSource::GitRange {
            repo_root: std::path::PathBuf::from("/repo"),
            base: "main".to_string(),
            head: None,
        };
        let (result, tmp) =
            materialize_stdin_if_needed(source).expect("git-range must never read stdin");
        assert!(tmp.is_none());
        assert!(matches!(result, DiffSource::GitRange { .. }));
    }
}
