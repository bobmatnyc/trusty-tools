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
    config::{ReviewConfig, RoleCliOverrides, SourceRootOutcome},
    integrations::{
        NullAnalyzeClient, NullSearchClient,
        github::{AuthStrategy, GithubClient, RunMode},
        search_client::HttpSearchClient,
        subprocess_analyze_client::SubprocessAnalyzeClient,
    },
    llm::build_provider,
    pipeline::{
        CallerContext, DiffSource, ReviewDeps, ReviewInput, TriggerDecision, log_json_path,
        run_review,
    },
};

use crate::cli_verify;
use crate::commands::diff_source::{LocalDiffFlags, resolve_local_diff_source};

// ─── run args (re-used by compare) ─────────────────────────────────────────

/// Arguments for the `run` subcommand.
///
/// Why: groups all run-mode flags in one place for clarity and testability.
/// What: owner/repo/pr identify the GitHub PR; --local-diff bypasses GitHub
/// (`-` reads a unified diff from stdin); --base [--head] derives the diff
/// from a local git ref range instead (issue #2993). --base and --local-diff
/// are mutually exclusive (enforced by clap).
/// Test: `cargo run -p trusty-review -- run --help`.
#[derive(Debug, clap::Parser)]
pub struct RunArgs {
    /// GitHub organisation or user (required unless --local-diff/--base is set).
    #[arg(value_name = "OWNER")]
    pub owner: Option<String>,

    /// GitHub repository name (required unless --local-diff/--base is set).
    #[arg(value_name = "REPO")]
    pub repo: Option<String>,

    /// Pull request number (required unless --local-diff/--base is set).
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

    /// Explicit source-root directory for code-context retrieval (issue #2994).
    ///
    /// If `dir` maps to an already-registered trusty-search index (matched by
    /// the same longest-root-path logic as the `TRUSTY_SEARCH_INDEX`/CWD
    /// auto-derive), that index is used — same resolution as today, just
    /// explicit instead of CWD-derived. If it does NOT map to a registered
    /// index, the review degrades to diff-only (no code-context retrieval)
    /// with a clear stderr notice and an in-body banner, instead of silently
    /// querying an unrelated project's index. `TRUSTY_SEARCH_INDEX` remains
    /// the fully-explicit override and takes precedence over `--source-root`
    /// when both are set.
    #[arg(long, value_name = "DIR")]
    pub source_root: Option<std::path::PathBuf>,

    /// Write the review log file to the configured log directory.
    #[arg(long = "no-log", action = clap::ArgAction::SetFalse, default_value = "true")]
    pub write_log: bool,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `run` subcommand.
///
/// Why: one-shot review of a PR or local diff with the selected reviewer model.
/// What: resolves the diff source, builds deps, runs the pipeline, prints the
/// result to STDOUT, and optionally writes the log file.  Resolves
/// `--source-root` (issue #2994) first — it wins over CWD auto-derive but
/// loses to an explicit `TRUSTY_SEARCH_INDEX` — then calls `resolve_index` so
/// the correct trusty-search index is used even when `TRUSTY_SEARCH_INDEX` is
/// unset (issue #670 / auto-derive #661).
/// Test: CLI integration via `cargo run -p trusty-review -- run --help`;
/// `resolve_index` wiring covered by
/// `wiring_cmd_run_resolve_index_updates_before_pipeline` in
/// `config/config_resolve_index_tests.rs`; `--source-root` wiring covered by
/// `run_args_source_root_parses` / `run_args_source_root_absent_is_none`.
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

    // ── --source-root (issue #2994) ────────────────────────────────────────
    // Resolved BEFORE the CWD/env auto-derive below: an explicit --source-root
    // wins over CWD-derivation, but TRUSTY_SEARCH_INDEX (search_index_explicit)
    // — the fully-explicit operator override — still wins over both, so this
    // block is skipped entirely when the env var is already set.
    let source_root_notice = resolve_source_root_arg(
        &mut config_with_overrides,
        &search_for_resolve,
        args.source_root.as_deref(),
    )
    .await;

    config_with_overrides
        .resolve_index(&search_for_resolve)
        .await;

    let mut deps =
        build_deps_async(&config_with_overrides, &reviewer_model, &default_provider).await?;
    apply_source_root_fallback(&mut deps, source_root_notice.as_deref());

    let input = ReviewInput {
        diff_source,
        reviewer_model: reviewer_model.clone(),
        write_log: args.write_log,
        print_result: true,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: true,
        caller_context: CallerContext::default(),
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
/// Why: the diff source depends on whether `--local-diff`/`--base` is set or
/// the three positional args (owner/repo/pr) are provided.
/// What: delegates local-mode selection (`LocalFile`/`Stdin`/`GitRange`) to
/// the shared `resolve_local_diff_source` (issue #2993); falls back to
/// resolving a GitHub PR source from the positional args + a resolved token.
/// Test: positional args and --local-diff/--base validated; local-mode
/// selection itself is covered by `diff_source::tests`.
pub async fn resolve_diff_source_run(config: &ReviewConfig, args: &RunArgs) -> Result<DiffSource> {
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
        .context("OWNER is required (or use --local-diff / --base)")?
        .to_string();
    let repo = args
        .repo
        .as_deref()
        .context("REPO is required (or use --local-diff / --base)")?
        .to_string();
    let pr = args
        .pr
        .context("PR number is required (or use --local-diff / --base)")?;

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
    let llm = build_provider(model, default_provider, config)
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

/// Resolve an optional `--source-root <dir>` argument against `config`,
/// shared by `cmd_run` and `cmd_compare` (issue #2994).
///
/// Why: both subcommands need the exact same precedence and fallback
/// behaviour — a matched source-root sets the index (and is left alone from
/// then on), a no-match forces a diff-only review — so the logic lives here
/// once rather than being duplicated per subcommand.
/// What: a no-op (returns `None`) when `source_root` is `None`, OR when
/// `config.search_index_explicit` is already `true` (an explicit
/// `TRUSTY_SEARCH_INDEX` always wins over `--source-root`, logged at `warn`).
/// Otherwise delegates to `ReviewConfig::resolve_source_root` and returns the
/// notice string on `SourceRootOutcome::DiffOnly` (the caller must pass it to
/// `apply_source_root_fallback` to swap both `deps.search` and `deps.analyze`
/// for null clients built from that notice); returns `None` on
/// `SourceRootOutcome::Matched` (config was already updated in-place). The
/// matched/no-match/daemon-unreachable branches themselves — including the
/// single `warn!` that logs the notice — are covered where
/// `ReviewConfig::resolve_source_root` is defined; this helper does NOT
/// duplicate that log line (a prior version printed the same notice twice —
/// once via `warn!` inside `resolve_source_root`, once via a raw `eprintln!`
/// here — fixed as part of the #2994 re-review).
/// Test: `resolve_source_root_arg_none_is_noop`,
/// `resolve_source_root_arg_explicit_env_index_wins`.
pub async fn resolve_source_root_arg(
    config: &mut ReviewConfig,
    search_client: &HttpSearchClient,
    source_root: Option<&std::path::Path>,
) -> Option<String> {
    let dir = source_root?;
    if config.search_index_explicit {
        warn!(
            "--source-root {} ignored: TRUSTY_SEARCH_INDEX={} is already set and takes precedence",
            dir.display(),
            config.search_index
        );
        return None;
    }
    match config.resolve_source_root(search_client, dir).await {
        SourceRootOutcome::Matched(_) => None,
        SourceRootOutcome::DiffOnly(notice) => Some(notice),
    }
}

/// Apply the `--source-root` diff-only fallback notice to already-built deps,
/// shared by `cmd_run` and `cmd_compare` (issue #2994 re-review, finding #1).
///
/// Why: `SourceRootOutcome::DiffOnly` must fully disconnect BOTH
/// network-facing context dependencies from whatever `config.search_index`
/// happens to hold — not just search. `ReviewConfig::resolve_source_root`
/// clears `context.require_analyze` alongside `require_search`, but that flag
/// only affects the required-context gate (`pipeline::context_gate`); it does
/// NOT stop `gather_context` from calling the REAL analyze client against the
/// stale index if the daemon happens to be healthy (currently unexploitable
/// only because `SubprocessAnalyzeClient` hardcodes empty results — fixed by
/// design here, not left to that accident). This helper mirrors the existing
/// `deps.search` swap for `deps.analyze` so both are covered symmetrically.
/// What: no-op when `notice` is `None`; otherwise replaces `deps.search` with
/// a `NullSearchClient` and `deps.analyze` with `Some(NullAnalyzeClient)`,
/// both carrying the same operator-facing notice string.
/// Test: `apply_source_root_fallback_nulls_both_clients_when_notice_present`,
/// `apply_source_root_fallback_is_noop_when_notice_absent`.
pub fn apply_source_root_fallback(deps: &mut ReviewDeps, notice: Option<&str>) {
    if let Some(notice) = notice {
        deps.search = Arc::new(NullSearchClient::new(notice));
        deps.analyze = Some(Arc::new(NullAnalyzeClient::new(notice)));
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    /// Guards the `conflicts_with = "base"` wiring on `local_diff` (#2993):
    /// a field-id rename that silently drops the attribute would otherwise
    /// only be caught by manual testing.
    #[test]
    fn run_args_base_and_local_diff_conflict() {
        let result = RunArgs::try_parse_from(["run", "--base", "main", "--local-diff", "x.diff"]);
        assert!(
            result.is_err(),
            "--base and --local-diff must be mutually exclusive"
        );
    }

    /// Guards the `requires = "base"` wiring on `head` (#2993).
    #[test]
    fn run_args_head_without_base_errors() {
        let result = RunArgs::try_parse_from(["run", "--head", "feature"]);
        assert!(result.is_err(), "--head requires --base");
    }

    #[test]
    fn run_args_base_alone_parses() {
        let args = RunArgs::try_parse_from(["run", "--base", "main"]).expect("parse");
        assert_eq!(args.base.as_deref(), Some("main"));
        assert!(args.head.is_none());
    }

    #[test]
    fn run_args_base_and_head_parses() {
        let args =
            RunArgs::try_parse_from(["run", "--base", "main", "--head", "feature"]).expect("parse");
        assert_eq!(args.base.as_deref(), Some("main"));
        assert_eq!(args.head.as_deref(), Some("feature"));
    }

    #[test]
    fn run_args_local_diff_dash_parses() {
        let args = RunArgs::try_parse_from(["run", "--local-diff", "-"]).expect("parse");
        assert_eq!(args.local_diff.as_deref(), Some(std::path::Path::new("-")));
    }

    // ── --source-root (#2994) ───────────────────────────────────────────────

    /// `--source-root <dir>` must parse to the given path.
    ///
    /// Why: guards the clap wiring itself — a typo'd `#[arg]` attribute or a
    /// field rename would otherwise only surface as a runtime "unrecognised
    /// flag" error.
    /// What: parses `run --source-root /tmp/proj` and asserts the field.
    /// Test: this test.
    #[test]
    fn run_args_source_root_parses() {
        let args = RunArgs::try_parse_from(["run", "--source-root", "/tmp/proj"]).expect("parse");
        assert_eq!(
            args.source_root.as_deref(),
            Some(std::path::Path::new("/tmp/proj"))
        );
    }

    /// `--source-root` must default to `None` when absent — the zero-regression
    /// requirement of #2994 (existing behaviour must be byte-for-byte unchanged
    /// when the flag is not used).
    /// Why: proves adding the flag does not change parsing of any existing
    /// invocation.
    /// What: parses `run --base main` (no `--source-root`) and asserts `None`.
    /// Test: this test.
    #[test]
    fn run_args_source_root_absent_is_none() {
        let args = RunArgs::try_parse_from(["run", "--base", "main"]).expect("parse");
        assert!(
            args.source_root.is_none(),
            "--source-root must default to None when not passed"
        );
    }

    /// `resolve_source_root_arg` must be a pure no-op when `source_root` is
    /// `None` — the zero-regression path exercised at the helper level (not
    /// just clap parsing).
    /// Why: proves the command-level wiring never touches `config` when
    /// `--source-root` is absent, regardless of what the (unreachable) search
    /// client would have returned.
    /// What: calls the helper with `None` and an intentionally-unreachable
    /// `HttpSearchClient`; asserts `None` and that `config` is untouched.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_source_root_arg_none_is_noop() {
        let mut config = ReviewConfig::load(None);
        config.search_index = "main".to_string();
        config.search_index_explicit = false;
        let client = HttpSearchClient::new("http://127.0.0.1:1").expect("client init");

        let notice = resolve_source_root_arg(&mut config, &client, None).await;

        assert!(notice.is_none());
        assert_eq!(config.search_index, "main", "config must be untouched");
        assert!(!config.search_index_explicit);
    }

    /// An explicit `TRUSTY_SEARCH_INDEX` (`search_index_explicit = true`) must
    /// win over `--source-root` — the fully-explicit override takes precedence.
    /// Why: proves the precedence rule from #2994 ("Keep TRUSTY_SEARCH_INDEX as
    /// the explicit override") without needing a reachable trusty-search daemon
    /// — the whole point is that the daemon is never even queried in this case.
    /// What: sets `search_index_explicit = true`, calls the helper with a
    /// `Some(dir)` source_root and an unreachable client; asserts the helper
    /// returns `None` (no diff-only fallback triggered) and leaves the
    /// operator-chosen index untouched.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_source_root_arg_explicit_env_index_wins() {
        let mut config = ReviewConfig::load(None);
        config.search_index = "operator-chosen".to_string();
        config.search_index_explicit = true;
        let client = HttpSearchClient::new("http://127.0.0.1:1").expect("client init");

        let notice = resolve_source_root_arg(
            &mut config,
            &client,
            Some(std::path::Path::new("/tmp/proj")),
        )
        .await;

        assert!(
            notice.is_none(),
            "explicit TRUSTY_SEARCH_INDEX must win — no diff-only fallback"
        );
        assert_eq!(
            config.search_index, "operator-chosen",
            "explicit index must remain unchanged"
        );
    }

    // ── apply_source_root_fallback (#2994 re-review, finding #1) ───────────

    /// Minimal `LlmProvider` stub so `ReviewDeps` can be constructed in tests
    /// that never actually invoke `complete` (they only assert on the
    /// `search`/`analyze` wiring performed by `apply_source_root_fallback`).
    struct UnusedLlm;

    #[async_trait::async_trait]
    impl trusty_review::llm::LlmProvider for UnusedLlm {
        fn name(&self) -> &str {
            "unused"
        }

        async fn complete(
            &self,
            _req: trusty_review::llm::LlmRequest,
        ) -> Result<trusty_review::llm::LlmResponse, trusty_review::llm::LlmError> {
            unreachable!("apply_source_root_fallback tests never call complete()")
        }
    }

    /// Builds a `ReviewDeps` with a real (unreachable) `HttpSearchClient` and
    /// a real `SubprocessAnalyzeClient` standing in for "the deps as built by
    /// `build_deps_async` before any fallback is applied".
    fn deps_with_real_clients() -> ReviewDeps {
        let config = ReviewConfig::load(None);
        ReviewDeps {
            llm: Arc::new(UnusedLlm),
            verifier: None,
            search: Arc::new(HttpSearchClient::new("http://127.0.0.1:1").expect("client init")),
            analyze: Some(Arc::new(
                trusty_review::integrations::subprocess_analyze_client::SubprocessAnalyzeClient::from_config(&config)
                    .expect("client init"),
            )),
            dedup: None,
        }
    }

    /// `apply_source_root_fallback` must be a pure no-op when `notice` is
    /// `None` — the zero-regression path when `--source-root` was never
    /// given (or matched a registered index and produced no notice).
    /// Why: proves the helper never touches deps when there is nothing to
    /// fall back from.
    /// What: captures `Arc::as_ptr` identity of `search`/`analyze` before and
    /// after calling the helper with `None`.
    /// Test: this test.
    #[test]
    fn apply_source_root_fallback_is_noop_when_notice_absent() {
        let mut deps = deps_with_real_clients();
        let search_ptr_before = Arc::as_ptr(&deps.search);
        let analyze_ptr_before = deps.analyze.as_ref().map(Arc::as_ptr);

        apply_source_root_fallback(&mut deps, None);

        assert_eq!(
            Arc::as_ptr(&deps.search),
            search_ptr_before,
            "deps.search must be untouched when notice is None"
        );
        assert_eq!(
            deps.analyze.as_ref().map(Arc::as_ptr),
            analyze_ptr_before,
            "deps.analyze must be untouched when notice is None"
        );
    }

    /// `apply_source_root_fallback` must swap BOTH `deps.search` AND
    /// `deps.analyze` for null clients when a diff-only notice is present —
    /// the #1 re-review finding was that only `deps.search` was ever nulled,
    /// leaving `deps.analyze` wired to the real client.
    /// Why: asserts on the ACTUAL swapped deps wiring (by driving each
    /// client's behaviour), not just on the `config.require_*` flags, per the
    /// finding's explicit instruction.
    /// What: builds real (non-null) deps, applies the fallback with a notice,
    /// then proves BOTH clients now report themselves unavailable/absent and
    /// carry the notice text.
    /// Test: this test.
    #[tokio::test]
    async fn apply_source_root_fallback_nulls_both_clients_when_notice_present() {
        let mut deps = deps_with_real_clients();
        let notice = "--source-root /tmp/proj has no registered trusty-search index";

        apply_source_root_fallback(&mut deps, Some(notice));

        let search_err = deps
            .search
            .health()
            .await
            .expect_err("search must be nulled to always-unavailable");
        assert!(
            search_err.to_string().contains(notice),
            "nulled search client must carry the source-root notice: {search_err}"
        );

        let analyze = deps
            .analyze
            .as_ref()
            .expect("analyze must remain Some (nulled, not removed)");
        assert!(
            !analyze.has_analysis("any-index").await,
            "nulled analyze client must always report no analysis available"
        );
        let analyze_err = analyze
            .health()
            .await
            .expect_err("nulled analyze client must always report unavailable");
        assert!(
            analyze_err.to_string().contains(notice),
            "nulled analyze client must carry the same source-root notice: {analyze_err}"
        );
    }
}
