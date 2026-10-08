//! Handler for the `run` subcommand.
//!
//! Why: extracted from `main.rs` to keep that file under the 500-line cap (#610).
//!
//! What: resolves the diff source, builds deps, runs the review pipeline,
//! optionally writes the log file, and exits non-zero on a review that failed.
//!
//! #6290: this is the per-invocation entry point that replaces the retired
//! `review.run` daemon method. `--json` prints the same `ReviewResult` object
//! that method returned, and the exit rule lives in
//! [`trusty_review::run_output`] so the JSON and the exit code cannot disagree.
//!
//! Test: CLI integration via `cargo run -p trusty-review -- run --help`;
//! pipeline logic covered by `runner::tests`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use tracing::warn;

use trusty_review::{
    config::{
        InvocationSurface, ReviewConfig, RoleCliOverrides, SourceRootOutcome,
        constants::{MAX_CALLER_CONTEXT_CHARS, MAX_ISSUE_DOCS_FILE_BYTES},
        repo_index::PinOrigin,
    },
    integrations::{
        NullAnalyzeClient, NullSearchClient,
        github::{AuthStrategy, GithubClient, RunMode},
        search_client::{HttpSearchClient, SearchClient},
        subprocess_analyze_client::SubprocessAnalyzeClient,
    },
    llm::build_provider,
    models::{ContextSourceRecord, ReviewResult, SourceState},
    pipeline::{
        CallerContext, DiffSource, IssueDoc, OptionalContextRequest, ReviewDeps, ReviewInput,
        ReviewOptions, TriggerDecision, log_json_path,
        post::{FinalizeAction, decide_action},
        pr_index::{IndexPin, PrIndex, resolve_pr_index},
        run_review_with,
    },
    run_output::{ledger_value, run_failure_reason, run_is_failure, run_json_payload},
    store::{DedupNeed, open_dedup_for},
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

    /// Name of a review template to append as a prompt addendum (issue #2995).
    /// Resolved bundled → `<config_dir>/trusty-review/templates/review/<name>.md`
    /// user override, where `<config_dir>` is `dirs::config_dir()` — on Linux
    /// `~/.config` (or `$XDG_CONFIG_HOME`), on macOS
    /// `~/Library/Application Support` (see
    /// `trusty_review::review_template::ReviewTemplateLoader`); composes with
    /// (never replaces) the stock rubric and any active voice/principles
    /// layers. Highest-precedence override — beats a repo `.trusty-review.toml`
    /// `[review] template` key, `TRUSTY_REVIEW_TEMPLATE`, and the config file.
    #[arg(long, value_name = "NAME")]
    pub review_template: Option<String>,

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
    /// querying an unrelated project's index. This per-run flag takes
    /// precedence over an ambient `TRUSTY_SEARCH_INDEX` when both are set
    /// (#8651). On a GitHub-PR run either pin is checked against OWNER/REPO,
    /// and an index recorded for another repo fails the run (#8651).
    #[arg(long, value_name = "DIR")]
    pub source_root: Option<std::path::PathBuf>,

    /// Write the review log file to the configured log directory.
    #[arg(long = "no-log", action = clap::ArgAction::SetFalse, default_value = "true")]
    pub write_log: bool,

    /// Print the review as JSON on stdout instead of the human report.
    ///
    /// #6290: this is what the retired `review.run` JSON-RPC method returned —
    /// the same `ReviewResult` object, field for field, so a caller that parsed
    /// the daemon's answer parses this unchanged. A run that fails still prints
    /// the JSON (with `error` set) AND exits non-zero; it never exits 0 on an
    /// empty success.
    #[arg(long)]
    pub json: bool,

    /// Post the review live to the GitHub PR. Without this flag, `run` never
    /// posts — the ambient `PR_INTELLIGENCE_DRY_RUN` env var is NOT consulted
    /// for this decision (#4460).
    ///
    /// #4460: a run intended as a local pre-merge check published a live
    /// REQUEST_CHANGES review with nothing at the call site signalling that
    /// posting was enabled — the caller had merely inherited
    /// `PR_INTELLIGENCE_DRY_RUN=false` from the shell environment. `--live`
    /// is now the one and only per-invocation opt-in: passing it forces a
    /// live post even if the ambient env says dry-run, and omitting it forces
    /// a dry-run even if the ambient env says live (`--live` "beats" the
    /// ambient value either way — see `trigger_for_live_flag`). Has no effect
    /// on `--local-diff` / `--base` sources, which never reach GitHub anyway.
    #[arg(long)]
    pub live: bool,

    /// PR description for the reviewer, as literal text (a leading `@` is
    /// plain text). On `--local-diff`/`--base` runs this is the only PR
    /// description the reviewer sees (#8654).
    #[arg(long, value_name = "TEXT", conflicts_with = "pr_description_file")]
    pub pr_description: Option<String>,

    /// Read the PR description from a regular file (at most 256 KiB).
    #[arg(long, value_name = "PATH")]
    pub pr_description_file: Option<std::path::PathBuf>,

    /// PR discussion (review and issue comments — author rationale), as
    /// literal text. The verifier also receives it (#8654).
    #[arg(long, value_name = "TEXT", conflicts_with = "pr_discussion_file")]
    pub pr_discussion: Option<String>,

    /// Read the PR discussion from a regular file (at most 256 KiB).
    #[arg(long, value_name = "PATH")]
    pub pr_discussion_file: Option<std::path::PathBuf>,

    /// Referenced or related code the diff depends on, as literal text (#8654).
    #[arg(long, value_name = "TEXT", conflicts_with = "referenced_code_file")]
    pub referenced_code: Option<String>,

    /// Read the referenced code from a regular file (at most 256 KiB).
    #[arg(long, value_name = "PATH")]
    pub referenced_code_file: Option<std::path::PathBuf>,

    /// Merge the fetched PR body into the reviewer's PR description, capped at
    /// 64,000 characters with a visible marker, ahead of any --pr-description
    /// text (#9192). A local diff has no PR body; the review runs without it.
    /// With a dedup store wired (`--live`), a repeat review of the same head
    /// is skipped as a duplicate whatever its flags: push a new head to rerun.
    #[arg(long)]
    pub include_pr_body: bool,

    /// Report every context source the review had (#9192, #9194): pr_body,
    /// caller_context, issues, spec_docs, claude_md, changed_files, search,
    /// analyze, symbol_context and external_sources, each used, truncated,
    /// absent (asked for, nothing there), unavailable (could not be read) or
    /// not_requested. With
    /// `--json` the output becomes `{"result": <review>, "context_sources":
    /// [...]}`; otherwise one line is printed per source that was absent,
    /// unavailable or truncated. Any new input flag does the same. Without
    /// one `--json` prints the review object alone, as before, and a failed
    /// search or external source is only logged.
    #[arg(long)]
    pub report_context: bool,

    /// Issue docs for the reviewer (#9197): a JSON array of
    /// `{"id": "#42", "title": "...", "body": "...", "url": "..."}` in a
    /// regular file of at most 256 KiB. GitHub issue numbers only; works on a
    /// local diff. Each body is capped at 16,000 characters; at most 8 docs
    /// and 48,000 characters are shown, and a doc past either limit is left
    /// out whole. The verifier never sees them. Reports context sources, like
    /// `--include-pr-body`.
    #[arg(long, value_name = "PATH")]
    pub issue_docs_file: Option<std::path::PathBuf>,

    /// Read the ADR, spec and SLD docs the PR body names (plus trusty-search
    /// hits) at the PR head SHA, for the reviewer only (#9193): at most 6
    /// docs, 16,000 characters each and 48,000 in total, cut with a visible
    /// marker. A local diff has no head SHA; the source is reported
    /// unavailable and the review runs. Reports context sources.
    #[arg(long)]
    pub spec_docs: bool,

    /// Read the root CLAUDE.md and up to 3 nested ones at the PR head SHA,
    /// 16,000 characters in total, for the reviewer only (#9193). Reports
    /// context sources.
    #[arg(long)]
    pub claude_md: bool,

    /// Show the PR's changed files whole, read at the PR head SHA, for the
    /// reviewer only (#9195), within `--changed-files-budget` bytes. A file
    /// is shown whole or named under "Not shown" with its reason; over
    /// budget, tests drop first, then generated files, then the largest. Its
    /// lines are context only: a finding still cites the diff. A local diff
    /// has no head SHA; the source is reported unavailable and the review
    /// runs. Reports context sources.
    #[arg(long)]
    pub changed_files: bool,

    /// Byte budget for `--changed-files` (#9195): default 120,000, at most
    /// 400,000 (a larger value is clamped and reported); 0 reviews the diff
    /// only. Does nothing without `--changed-files`.
    #[arg(long, value_name = "BYTES")]
    pub changed_files_budget: Option<usize>,

    /// For each function or method the diff declares or edits, show its
    /// callers, callees and tests from the trusty-search call graph, for the
    /// reviewer only (#9196): at most 12 symbols, 6 lines per list, 2,500
    /// characters per symbol and 24,000 in total, each cut marked. The graph
    /// is the indexed checkout's and may not be at the PR head. Works on a
    /// local diff. A search outage leaves the section out and the review
    /// runs. Reports context sources.
    #[arg(long)]
    pub symbol_context: bool,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Rebuild the review config with `run`'s per-invocation model overrides.
///
/// Why: #8947 — this rebuild used to pass no path, so a `--config` file's
/// `[verification]` and `[models.verifier]` tables were read in `main` and then
/// thrown away; only the env overrides survived.
/// What: layers `--reviewer-model`/`--provider`/`--review-template` over the
/// same `--config` file `main` loaded.
/// Test: `run_config_honours_the_config_file_verification_settings`,
/// `run_and_calibrate_rebuild_their_config_from_the_config_path` (main.rs).
fn run_config(config_path: Option<&std::path::Path>, args: &RunArgs) -> ReviewConfig {
    let overrides = RoleCliOverrides {
        reviewer_model: args.reviewer_model.clone(),
        provider: args.provider.clone(),
        review_template: args.review_template.clone(),
        ..Default::default()
    };
    ReviewConfig::from_env_and_file(config_path, Some(&overrides))
}

/// Execute the `run` subcommand.
///
/// Why: one-shot review of a PR or local diff with the selected reviewer model.
/// What: prints the resolved posting mode (#4460), then resolves the diff
/// source, builds deps, runs the pipeline, prints the result to STDOUT, and
/// optionally writes the log file. Index resolution order: (1)
/// `--source-root` (#2994), which wins over `TRUSTY_SEARCH_INDEX` (#8651);
/// (2) `TRUSTY_SEARCH_INDEX` when no `--source-root` is given; (3) for a
/// GitHub PR, `pr_index_for_run` resolves the PR repo's own index or verifies
/// the step-1/2 pin against that repo (#8651); a `--source-root` that matched
/// no index skips this step and stays diff-only; (4) otherwise
/// `resolve_index` derives the index from the CWD (#670, #661), a no-op once
/// step 1 or 2 pinned one. The PR-context flags fill the `CallerContext`
/// (#8654).
/// Test: CLI integration via `cargo run -p trusty-review -- run --help`;
/// `resolve_index` wiring covered by
/// `wiring_cmd_run_resolve_index_updates_before_pipeline` in
/// `config/config_resolve_index_tests.rs`; `--source-root` wiring covered by
/// `run_args_source_root_parses` / `run_args_source_root_absent_is_none`;
/// the posting-mode gate covered by `trigger_for_live_flag_*` and
/// `posting_mode_banner_*` below.
pub async fn cmd_run(
    config: ReviewConfig,
    config_path: Option<&std::path::Path>,
    args: RunArgs,
) -> Result<()> {
    // #4460: print the resolved posting mode before any network call, so the
    // signal is visible at the moment it matters — never a silent ambient
    // default. Stderr, not stdout, so `--json` output stays parseable.
    eprintln!("{}", posting_mode_banner(args.live));

    // #8654: read the PR-context flags before any network call, so an
    // unreadable or oversized `-file` fails the run instead of being dropped.
    let caller_context = caller_context_from_args(&args)?;
    let request = run_request_with_issue_docs(&args)?; // #9197: same rule as the -file flags
    let diff_source = resolve_diff_source_run(&config, &args).await?;

    let mut config_with_overrides = run_config(config_path, &args);
    // Clone both to avoid holding a borrow across the mutable resolve_index call.
    let reviewer_model = config_with_overrides.role_models.reviewer.model.clone();
    let default_provider = config_with_overrides.role_models.reviewer.provider.clone();

    // Resolve the search index from the daemon before building deps. The CWD
    // resolve_index below runs whenever `pr_index_for_run` returns `None`: for
    // a local diff (`--local-diff`/`--base`), and for a GitHub PR whose
    // --source-root matched no index. It is a no-op once TRUSTY_SEARCH_INDEX
    // or --source-root set `search_index_explicit`; on any failure (daemon
    // unreachable, no match) it logs a warning and leaves search_index as is.
    // #8651: a GitHub-PR run never queries an index kept that way — it uses
    // the PR repo's own index or a verified pin, runs diff-only, or fails.
    let search_for_resolve = HttpSearchClient::from_config(&config_with_overrides)
        .map_err(|e| anyhow::anyhow!("failed to build search HTTP client: {e}"))?;

    // #8651: read before --source-root resolution, which also sets the flag.
    let env_pinned = config_with_overrides.search_index_explicit;

    // ── --source-root (issue #2994) ────────────────────────────────────────
    // Resolved BEFORE the CWD auto-derive below. #8651: the per-run
    // --source-root wins over CWD-derivation AND over TRUSTY_SEARCH_INDEX,
    // which can be ambient (inherited from a parent MCP server's env).
    let source_root_notice = resolve_source_root_arg(
        &mut config_with_overrides,
        &search_for_resolve,
        args.source_root.as_deref(),
    )
    .await;

    // #8651: a GitHub PR reviews against its own repo's index, and a pin is
    // verified against it; the CWD auto-derive below is for local diffs.
    let pin = RunPin::from_flags(
        env_pinned,
        args.source_root.is_some(),
        source_root_notice.is_some(),
    );
    let pr_index = pr_index_for_run(
        &config_with_overrides,
        &search_for_resolve,
        &diff_source,
        pin,
    )
    .await?;
    if pr_index.is_none() {
        config_with_overrides
            .resolve_index(&search_for_resolve)
            .await;
    }

    let mut deps = build_deps_async(
        &config_with_overrides,
        &reviewer_model,
        &default_provider,
        // #9348: a dry run cannot post, so it never opens or claims the store.
        dedup_need_for(&diff_source, run_can_post(args.live)),
    )
    .await?;
    apply_source_root_fallback(&mut deps, source_root_notice.as_deref());
    let (config_with_overrides, deps) = match pr_index {
        Some(index) => index.apply(&config_with_overrides, deps),
        None => (config_with_overrides, deps),
    };

    let input = run_input(&args, diff_source, reviewer_model.clone(), caller_context);
    let wants_ledger = request.ledger_enabled();
    let outcome = run_review_with(
        &config_with_overrides,
        input,
        deps,
        ReviewOptions::new(request),
    );
    let outcome = outcome.await;
    let result = &outcome.result;
    // #9192: the ledger is reported only when a flag asked for it.
    let ledger = wants_ledger.then_some(outcome.context_sources.as_slice());

    if args.json {
        // Serialised before the failure check so a failed review still hands the
        // caller its reason as data (#6290 fail-open check).
        println!(
            "{}",
            serde_json::to_string_pretty(&run_json_value(result, ledger))
                .unwrap_or_else(|e| format!(r#"{{"error":"serialising the result failed: {e}"}}"#))
        );
    } else {
        for note in ledger_notes(ledger.unwrap_or_default()) {
            println!("{note}");
        }
    }

    if args.write_log {
        let log_path = log_json_path(result, &config_with_overrides.log_dir);
        eprintln!("\nLog written to: {}", log_path.display());
    }

    // #6290: was `status.is_skipped()` alone, which exited 0 on a provider
    // outage — `abort_dry` records that as `error: Some(..)` with the status
    // left at `Completed`. See `run_output::run_is_failure`.
    if run_is_failure(result) {
        anyhow::bail!("{}", run_failure_reason(result));
    }

    Ok(())
}

/// The optional inputs `run`'s flags ask for (#9192).
///
/// Why: `--include-pr-body` is a new input (with `--issue-docs-file`, read
/// by [`run_request_with_issue_docs`], #9197) and `--report-context` asks
/// for the ledger alone; the PR-context text flags are
/// legacy and turn the ledger on only beside one of those two (ruling
/// 2026-10-06 03:42Z).
/// What: the request with `include_pr_body`, `report_context`, (#9193)
/// `spec_docs` and `claude_md`, (#9195) `changed_files` and its budget, and
/// (#9196) `symbol_context` from the flags.
/// Test: `run_include_pr_body_flag_parses`, `run_report_context_flag_turns_the_ledger_on`,
/// `run_flags_set_the_request`, `run_changed_files_flags_set_the_request`,
/// `changed_files_budget_without_flag_is_inert`, `run_symbol_context_flag_sets_the_request`.
pub(crate) fn run_request(args: &RunArgs) -> OptionalContextRequest {
    let request = OptionalContextRequest::default()
        .with_pr_body(args.include_pr_body)
        .with_report_context(args.report_context)
        .with_spec_docs(args.spec_docs)
        .with_claude_md(args.claude_md)
        .with_changed_files(args.changed_files) // #9195
        .with_symbol_context(args.symbol_context); // #9196
    match args.changed_files_budget {
        Some(bytes) => request.with_changed_files_budget(bytes), // inert without the flag
        None => request,
    }
}

/// [`run_request`] plus the docs `--issue-docs-file` names (#9197).
///
/// Why: a bad file must fail the run before any network call, as an
/// unreadable `--pr-description-file` does (#8654).
/// What: reads the file through [`read_pr_context_file`] (regular file, at
/// most [`MAX_ISSUE_DOCS_FILE_BYTES`], UTF-8), parses it as JSON, and runs
/// the same strict parser as the MCP `issue_docs` parameter.
///
/// # Errors
///
/// The file cannot be read, is over the cap, is not JSON, or is not a valid
/// `issue_docs` array; the message names the flag and the path.
///
/// Test: `issue_docs_file_parses_and_reports_context`,
/// `issue_docs_file_over_256_kib_is_refused`, `issue_docs_file_with_a_jira_id_is_refused`.
pub(crate) fn run_request_with_issue_docs(args: &RunArgs) -> Result<OptionalContextRequest> {
    let request = run_request(args);
    let Some(path) = args.issue_docs_file.as_deref() else {
        return Ok(request);
    };
    let flag = || format!("--issue-docs-file {}", path.display());
    let text = read_pr_context_file(path).with_context(flag)?;
    let value: serde_json::Value = serde_json::from_str(&text).with_context(flag)?;
    let docs = IssueDoc::list_from_json(&value).with_context(flag)?;
    Ok(request.with_issue_docs(docs))
}

/// `run --json`'s output value (#9192).
///
/// Why: Architect ruling 1 (2026-10-06): with no ledger flag the output is
/// the review object exactly as before; with one, the ledger travels beside
/// the review, never inside it.
/// What: `ledger: None` is [`run_json_payload`]; `Some(sources)` is
/// `{"result": <payload>, "context_sources": sources}`.
/// Test: `run_json_without_ledger_flags_is_the_plain_result`,
/// `run_json_wraps_the_result_when_the_ledger_is_on`.
pub(crate) fn run_json_value(
    result: &ReviewResult,
    ledger: Option<&[ContextSourceRecord]>,
) -> serde_json::Value {
    let payload = run_json_payload(result);
    let Some(sources) = ledger else {
        return payload;
    };
    let sources = ledger_value(sources); // #9194: the MCP envelope's form too
    let mut wrapped = serde_json::Map::new();
    wrapped.insert("result".to_string(), payload);
    wrapped.insert("context_sources".to_string(), sources);
    serde_json::Value::Object(wrapped)
}

/// One line per ledger row a reader must act on: `unavailable`, `truncated`
/// or (#9194) `absent`.
///
/// Why: a human `run --include-pr-body` that could not read the body said
/// nothing (critic MEDIUM, #9192); #9194 AC3: a source that returned nothing
/// is what separates a context-starved APPROVE from a fully-informed one.
/// What: names the source and the reason, or the characters cut. `used` and
/// `not_requested` rows print nothing.
/// Test: `ledger_notes_name_unavailable_and_truncated_rows`,
/// `run_human_output_names_absent_unavailable_and_truncated_rows`,
/// `ledger_notes_skip_used_and_not_requested_rows`.
pub(crate) fn ledger_notes(sources: &[ContextSourceRecord]) -> Vec<String> {
    sources
        .iter()
        .filter_map(|row| match row.state {
            SourceState::Unavailable => Some(format!(
                "context source {}: unavailable — {}",
                row.source,
                row.detail.as_deref().unwrap_or("no detail")
            )),
            SourceState::Absent => Some(format!(
                "context source {}: absent — {}",
                row.source,
                row.detail.as_deref().unwrap_or("no text")
            )),
            SourceState::Truncated => Some(format!(
                "context source {}: truncated — {} characters omitted",
                row.source, row.chars_omitted
            )),
            _ => None,
        })
        .collect()
}

/// #5113: `run` may post a GitHub-PR review, so it must carry the claim gate
/// that keeps a re-run from posting a second comment.
const ALLOW_POSTING: bool = true;

/// Whether one `run` invocation can reach the post path (#9348).
///
/// Why: `run` opened the dedup store and claimed the head on every GitHub run,
/// `--dry-run` included, and a dry run never completes or releases that claim,
/// so it blocked a live review of the same head for `DEDUP_STALE_SECS`.
/// What: asks `decide_action` with the trigger `run_input` builds from `--live`
/// and `ALLOW_POSTING`. The config flag is passed as live so a trigger that
/// deferred to it would open the store rather than skip it.
/// Test: `dry_run_does_not_need_the_dedup_store`,
/// `dedup_need_for_github_posting_is_required`.
pub(crate) fn run_can_post(live: bool) -> bool {
    decide_action(false, trigger_for_live_flag(live), ALLOW_POSTING, true) == FinalizeAction::Post
}

/// The `ReviewInput` one `run` invocation hands the pipeline.
///
/// Why: one place builds it, so the test that proves the PR-context flags
/// reach the reviewer prompt drives the same input `cmd_run` does (#8654).
/// What: the diff source and model, `run`'s posting posture, and the caller
/// context read from the flags.
/// Test: `pr_description_flag_reaches_the_reviewer_prompt`.
pub(crate) fn run_input(
    args: &RunArgs,
    diff_source: DiffSource,
    reviewer_model: String,
    caller_context: CallerContext,
) -> ReviewInput {
    let surface = surface_for_diff_source(&diff_source);
    ReviewInput {
        diff_source,
        reviewer_model,
        write_log: args.write_log,
        // #6290: `--json` owns stdout, so the human report must not also be
        // written there — a caller piping this into `jq` would get prose ahead
        // of the object.
        print_result: !args.json,
        // #4460: never TriggerDecision::None here — that would defer live-vs-
        // dry entirely to the ambient PR_INTELLIGENCE_DRY_RUN env var, with no
        // call-site signal. `--live` is the only per-invocation opt-in.
        trigger: trigger_for_live_flag(args.live),
        run_mode: RunMode::Cli,
        allow_posting: ALLOW_POSTING,
        caller_context,
        surface,
    }
}

/// The caller context `run`'s PR-context flags describe (#8654).
///
/// Why: `run` built `CallerContext::default()`, so a caller had no way to give
/// the reviewer the PR description, discussion, or referenced code.
/// What: `--pr-description`, `--pr-discussion` and `--referenced-code` are
/// literal text — a leading `@` is plain text. Each `-file` twin reads a
/// regular file through [`read_pr_context_file`]. A blank value is `None`.
///
/// # Errors
///
/// A `-file` path that cannot be read, is not a regular file, or exceeds
/// [`PR_CONTEXT_FILE_CAP`], naming the flag and the path.
///
/// Test: `pr_context_flags_read_text_and_files`,
/// `unreadable_pr_context_file_fails_the_run`,
/// `pr_context_file_over_the_cap_or_not_regular_is_refused`.
pub(crate) fn caller_context_from_args(args: &RunArgs) -> Result<CallerContext> {
    let read = |flag: &str,
                text: Option<&str>,
                file: Option<&std::path::Path>|
     -> Result<Option<String>> {
        let text = match (text, file) {
            (_, Some(path)) => read_pr_context_file(path)
                .with_context(|| format!("{flag}-file: cannot read {}", path.display()))?,
            (Some(text), None) => text.to_string(),
            (None, None) => return Ok(None),
        };
        Ok((!text.trim().is_empty()).then_some(text))
    };
    Ok(CallerContext {
        pr_description: read(
            "--pr-description",
            args.pr_description.as_deref(),
            args.pr_description_file.as_deref(),
        )?,
        pr_discussion: read(
            "--pr-discussion",
            args.pr_discussion.as_deref(),
            args.pr_discussion_file.as_deref(),
        )?,
        referenced_code: read(
            "--referenced-code",
            args.referenced_code.as_deref(),
            args.referenced_code_file.as_deref(),
        )?,
    })
}

/// The largest PR-context file `run` reads: 256 KiB.
///
/// Why: a file flag must not hang or exhaust memory on a huge file or a device
/// such as `/dev/zero` (#8654). 256 KiB is the smallest power-of-two byte
/// bound that holds a full [`MAX_CALLER_CONTEXT_CHARS`]-character field at the
/// UTF-8 worst case of 4 bytes per character, so the file cap never refuses
/// text the pipeline's per-field cap would carry whole.
pub(crate) const PR_CONTEXT_FILE_CAP: u64 = 256 * 1024;
const _: () = assert!(PR_CONTEXT_FILE_CAP >= MAX_CALLER_CONTEXT_CHARS as u64 * 4);
// #9197: `--issue-docs-file` reads through the same bounded reader.
const _: () = assert!(PR_CONTEXT_FILE_CAP == MAX_ISSUE_DOCS_FILE_BYTES);

/// Read one PR-context file, bounded by [`PR_CONTEXT_FILE_CAP`] (#8654).
///
/// Why: an unbounded `read_to_string` blocks forever on a FIFO and never ends
/// on `/dev/zero`.
/// What: refuses a path that is not a regular file (checked before `open`, so
/// a FIFO never blocks the open, and again on the open handle), then reads at
/// most `CAP + 1` bytes through `take` and fails when more than `CAP` arrived.
///
/// # Errors
///
/// The path cannot be opened or read, is not a regular file, exceeds the cap,
/// or is not UTF-8.
///
/// Test: `pr_context_file_over_the_cap_or_not_regular_is_refused`.
fn read_pr_context_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read as _;
    let regular = |meta: std::fs::Metadata| -> Result<()> {
        anyhow::ensure!(meta.is_file(), "not a regular file");
        Ok(())
    };
    regular(std::fs::metadata(path)?)?;
    let file = std::fs::File::open(path)?;
    regular(file.metadata()?)?;
    let mut bytes = Vec::new();
    file.take(PR_CONTEXT_FILE_CAP + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= PR_CONTEXT_FILE_CAP,
        "larger than the {PR_CONTEXT_FILE_CAP}-byte cap for a PR-context file"
    );
    Ok(String::from_utf8(bytes)?)
}

/// Where a GitHub-PR `run`'s index pin came from (#8651).
///
/// Why: `TRUSTY_SEARCH_INDEX` and `--source-root` used to skip per-repo
/// resolution outright, so an ambient `TRUSTY_SEARCH_INDEX` reviewed every PR
/// against one unrelated index. Each pin is now checked against the PR repo.
/// What: one variant per pin source; `SourceRootDiffOnly` is a
/// `--source-root` that matched no index and already forced a diff-only run.
/// Test: `foreign_pin_is_refused_and_matching_pin_is_used`,
/// `source_root_index_is_checked_against_the_pr_repo`,
/// `run_pin_classifies_the_flags`,
/// `run_pin_env_pin_applies_without_source_root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunPin {
    /// No pin: resolve the PR repo's own index.
    None,
    /// `TRUSTY_SEARCH_INDEX` with no `--source-root`.
    Env,
    /// `--source-root` matched a registered index.
    SourceRoot,
    /// `--source-root` matched no index; the run is already diff-only.
    SourceRootDiffOnly,
}

impl RunPin {
    /// Classify the pin from the flags `cmd_run` saw; `--source-root` is
    /// checked before the env pin (#8651).
    pub(crate) fn from_flags(env_pinned: bool, source_root: bool, diff_only: bool) -> Self {
        match (source_root, diff_only, env_pinned) {
            (true, true, _) => RunPin::SourceRootDiffOnly,
            (true, false, _) => RunPin::SourceRoot,
            (false, _, true) => RunPin::Env,
            (false, _, false) => RunPin::None,
        }
    }
}

/// The index a GitHub-PR `run` reviews against, or `None` (#8651).
///
/// Why: `run` derived its index once from the CWD and fell back to `"main"`,
/// so a PR in any other repo was reviewed against the wrong index; a pin then
/// bypassed the check entirely.
/// What: for a `DiffSource::Github`, resolves the repo's own index
/// ([`RunPin::None`]) or verifies the pinned `config.search_index` against
/// owner/repo ([`RunPin::Env`], [`RunPin::SourceRoot`]) on the run's surface
/// (`Hosted`, so an unreadable registry fails unless search is opted out).
/// `None` for a local diff, which keeps the CWD resolution, and for
/// [`RunPin::SourceRootDiffOnly`], which keeps its diff-only fallback.
///
/// # Errors
///
/// The repo has no index, its name is invalid, a pin is foreign or
/// unverifiable, or the registry cannot be read while search is required.
///
/// Test: `github_run_resolves_its_own_repo_index`,
/// `github_run_with_no_repo_index_fails`,
/// `foreign_pin_is_refused_and_matching_pin_is_used`,
/// `source_root_index_is_checked_against_the_pr_repo`.
pub(crate) async fn pr_index_for_run(
    config: &ReviewConfig,
    search: &dyn SearchClient,
    diff_source: &DiffSource,
    pin: RunPin,
) -> Result<Option<PrIndex>> {
    let DiffSource::Github { owner, repo, .. } = diff_source else {
        return Ok(None);
    };
    let pinned = config.search_index.as_str();
    let pin = match pin {
        RunPin::None => IndexPin::Prefer,
        RunPin::Env => IndexPin::Require(pinned, PinOrigin::Env),
        RunPin::SourceRoot => IndexPin::Require(pinned, PinOrigin::SourceRoot),
        RunPin::SourceRootDiffOnly => return Ok(None),
    };
    let surface = surface_for_diff_source(diff_source);
    let index = resolve_pr_index(search, config, surface, owner, repo, pin).await?;
    Ok(Some(index))
}

// ─── posting-mode gate (#4460) ─────────────────────────────────────────────

/// Resolve the `run` subcommand's live-vs-dry trigger from the explicit
/// `--live` flag.
///
/// Why: the incident behind #4460 happened because `cmd_run` passed
/// `TriggerDecision::None`, which defers entirely to the ambient
/// `PR_INTELLIGENCE_DRY_RUN` env var (see `effective_dry_run`) — a caller who
/// merely inherited that var from the calling shell got no call-site signal
/// before a review published live. Returning a forcing decision instead of
/// `None` means `--live` is the only thing that can enable posting for this
/// CLI surface, and it wins over the ambient env either way (present but
/// `PR_INTELLIGENCE_DRY_RUN=true`? `--live` still posts; absent but
/// `PR_INTELLIGENCE_DRY_RUN=false`? still dry-run).
/// What: `true` → `TriggerDecision::ForceLive`; `false` →
/// `TriggerDecision::ForceDryRun`. Extracted as a pure function so the gate is
/// unit-testable without building a `ReviewConfig` or setting env vars.
/// Test: `trigger_for_live_flag_true_forces_live`,
/// `trigger_for_live_flag_false_forces_dry_run`,
/// `dry_run_env_var_alone_never_enables_posting` (regression, #4460),
/// `live_flag_posts_even_when_ambient_env_says_dry_run`.
pub fn trigger_for_live_flag(live: bool) -> TriggerDecision {
    if live {
        TriggerDecision::ForceLive
    } else {
        TriggerDecision::ForceDryRun
    }
}

/// Render the posting-mode line `cmd_run` prints before any network call.
///
/// Why: #4460's second ask — a run about to post live must say so, visibly,
/// at the call site, before it does any work. Extracted to a pure function
/// (rather than an inline `eprintln!`) so the exact wording is unit-testable.
/// What: `true` → a line stating this run WILL post; `false` → a line stating
/// this run is a dry-run and naming the flag that would change that.
/// Test: `posting_mode_banner_live`, `posting_mode_banner_dry_run`.
pub fn posting_mode_banner(live: bool) -> String {
    if live {
        "posting: enabled (--live) — this run will post a review comment to GitHub".to_string()
    } else {
        "posting: dry-run (default) — pass --live to post a review comment to GitHub".to_string()
    }
}

// ─── shared helpers ──────────────────────────────────────────────────────────

/// Decide the `InvocationSurface` for a resolved `DiffSource` (search-
/// unreachable semantics fix, closes #3030 tracked MEDIUM (a)).
///
/// Why: a local-diff/--base/--source-root review never posts to GitHub (the
/// `owner == "local"` sentinel forces log-only downstream regardless of
/// `allow_posting`), so it is safe and useful to default to a DEGRADED
/// diff-only review when search is down. A GitHub-PR `run` invocation CAN
/// post a live comment (same as the webhook bot), so it keeps the strict
/// `Hosted` default — unchanged behaviour (zero regression for the gate use
/// case). Extracted to a pure function (previously inlined in `cmd_run`) so
/// the CLI surface mapping is independently unit-testable without building a
/// full `ReviewConfig`/HTTP client.
/// What: `DiffSource::Github { .. }` → `InvocationSurface::Hosted`; every
/// other variant (`LocalFile`, `GitRange`, `Stdin`) → `InvocationSurface::Interactive`.
/// Test: `surface_for_diff_source_github_is_hosted`,
/// `surface_for_diff_source_local_file_is_interactive`,
/// `surface_for_diff_source_git_range_is_interactive`,
/// `surface_for_diff_source_stdin_is_interactive`.
pub fn surface_for_diff_source(diff_source: &DiffSource) -> InvocationSurface {
    if matches!(diff_source, DiffSource::Github { .. }) {
        InvocationSurface::Hosted
    } else {
        InvocationSurface::Interactive
    }
}

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

/// Decide whether a `run`-style invocation needs the durable dedup store
/// (#5113).
///
/// Why: `cmd_run` sets `allow_posting: true`, so a GitHub-PR invocation can
/// post a live comment — and a run that can post must carry the claim gate that
/// makes the post idempotent, or a re-run against the same
/// `(owner, repo, pr, head_sha)` posts a second comment. The local sources
/// never reach the post path, so opening (and locking) `dedup.redb` for them
/// would be pure cost. Extracted as a pure function for the same reason
/// `surface_for_diff_source` is: the rule is testable without building a
/// `ReviewConfig` or an LLM provider.
/// What: `Required` when posting is allowed AND the diff came from a GitHub PR;
/// `NotNeeded` otherwise.
/// Test: `dedup_need_for_github_posting_is_required`,
/// `dedup_need_for_github_without_posting_is_not_needed`,
/// `dedup_need_for_local_sources_is_not_needed`.
pub fn dedup_need_for(diff_source: &DiffSource, allow_posting: bool) -> DedupNeed {
    if allow_posting && matches!(diff_source, DiffSource::Github { .. }) {
        DedupNeed::Required
    } else {
        DedupNeed::NotNeeded
    }
}

/// Build the injected service dependencies from `ReviewConfig` and a model id.
///
/// Why: both `run` and `compare` need the same set of deps; building them from
/// config in one place avoids repetition.  Async because the search and dedup
/// dependencies it opens are async (#5469: no longer because of
/// `BedrockProvider::new`, whose AWS client is built lazily on first use).
/// What: uses `build_provider` (which resolves the `bedrock/`/`openrouter/`
/// prefix), builds the optional verifier, constructs search/analyze clients, and
/// opens the dedup store when `dedup_need` says this invocation can post
/// (#5113). A `Required` store that cannot be opened is an error, never a
/// downgrade to `dedup: None` — the same contract the `serve` path declares.
/// Test: covered transitively by runner tests that inject a FakeLlm;
/// `dedup_need_for_*` cover the need decision itself.
pub async fn build_deps_async(
    config: &ReviewConfig,
    model: &str,
    default_provider: &trusty_review::config::Provider,
    dedup_need: DedupNeed,
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

    let dedup = open_dedup_for(&config.log_dir, dedup_need)
        .map_err(|e| anyhow::anyhow!("failed to open the dedup claim store: {e}"))?;

    Ok(ReviewDeps {
        llm,
        verifier,
        search: Arc::new(search),
        analyze: Some(Arc::new(analyze)),
        dedup,
    })
}

/// Resolve an optional `--source-root <dir>` argument against `config`,
/// shared by `cmd_run` and `cmd_compare` (issue #2994).
///
/// Why: both subcommands need the exact same precedence and fallback
/// behaviour — a matched source-root sets the index (and is left alone from
/// then on), a no-match forces a diff-only review — so the logic lives here
/// once rather than being duplicated per subcommand.
/// What: a no-op (returns `None`) when `source_root` is `None`. Otherwise
/// it resolves `source_root` even when `TRUSTY_SEARCH_INDEX` is set: the
/// per-run flag overrides an env pin that may be ambient (#8651), logged at
/// `info`. It delegates to `ReviewConfig::resolve_source_root` and returns the
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
/// `resolve_source_root_arg_beats_env_index`.
pub async fn resolve_source_root_arg(
    config: &mut ReviewConfig,
    search_client: &HttpSearchClient,
    source_root: Option<&std::path::Path>,
) -> Option<String> {
    let dir = source_root?;
    // #8651: the per-run flag beats an env pin, which can be inherited.
    if config.search_index_explicit {
        tracing::info!(
            "--source-root {} overrides TRUSTY_SEARCH_INDEX={}",
            dir.display(),
            config.search_index
        );
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
    use trusty_review::pipeline::effective_dry_run;

    // ── --live / posting-mode gate (#4460) ──────────────────────────────────

    /// `--live` must parse and default to `false` when absent.
    ///
    /// Why: guards the clap wiring itself, same rationale as the other
    /// `run_args_*_parses` tests in this module.
    /// What: asserts the flag round-trips both ways.
    /// Test: this test.
    #[test]
    fn run_args_live_flag_parses() {
        let args = RunArgs::try_parse_from(["run", "--live"]).expect("parse");
        assert!(args.live);

        let args = RunArgs::try_parse_from(["run"]).expect("parse");
        assert!(!args.live, "--live must default to false");
    }

    /// #9192: `--include-pr-body` parses, defaults off, and turns the ledger
    /// on; a legacy text flag alone never does.
    #[test]
    fn run_include_pr_body_flag_parses() {
        let args = RunArgs::try_parse_from(["run", "--include-pr-body"]).expect("parse");
        assert!(args.include_pr_body);
        assert!(run_request(&args).requested_new());

        let args = RunArgs::try_parse_from(["run", "--pr-description", "x"]).expect("parse");
        assert!(
            !args.include_pr_body,
            "--include-pr-body must default to false"
        );
        assert!(!run_request(&args).ledger_enabled());
    }

    /// #9193: `--spec-docs` and `--claude-md` default off and set their own
    /// flag in the request, each a new input.
    #[test]
    fn run_flags_set_the_request() {
        let none = run_request(&RunArgs::try_parse_from(["run"]).expect("parse"));
        assert!(!none.spec_docs && !none.claude_md && !none.requested_new());
        let args = RunArgs::try_parse_from(["run", "--spec-docs"]).expect("parse");
        let request = run_request(&args);
        assert!(request.spec_docs && !request.claude_md && request.requested_new());
        let args = RunArgs::try_parse_from(["run", "--claude-md"]).expect("parse");
        let request = run_request(&args);
        assert!(request.claude_md && !request.spec_docs && request.requested_new());
    }

    /// #9192: `--report-context` turns the ledger on with no other new input,
    /// and the text flags turn it on only beside it.
    #[test]
    fn run_report_context_flag_turns_the_ledger_on() {
        let args = RunArgs::try_parse_from(["run", "--report-context"]).expect("parse");
        let request = run_request(&args);
        assert!(request.ledger_enabled() && !request.requested_new());
        let args = RunArgs::try_parse_from(["run", "--pr-discussion", "x", "--report-context"])
            .expect("parse");
        assert!(run_request(&args).ledger_enabled());
    }

    /// #9192: with no ledger flag, `--json` prints exactly the review object,
    /// the value origin/main printed.
    #[test]
    fn run_json_without_ledger_flags_is_the_plain_result() {
        let result = ReviewResult::new("acme", "billing", 7, "Add Y", "https://x/pull/7");
        for argv in [vec!["run"], vec!["run", "--pr-description", "x", "--json"]] {
            let args = RunArgs::try_parse_from(argv).expect("parse");
            assert!(!run_request(&args).ledger_enabled());
        }
        let plain = run_json_value(&result, None);
        assert_eq!(plain, run_json_payload(&result));
        assert_eq!(
            serde_json::to_string_pretty(&plain).expect("serialises"),
            serde_json::to_string_pretty(&run_json_payload(&result)).expect("serialises"),
        );
    }

    /// #9192: with the ledger on, `--json` wraps the review beside its ledger,
    /// even when the ledger is empty.
    #[test]
    fn run_json_wraps_the_result_when_the_ledger_is_on() {
        let result = ReviewResult::new("acme", "billing", 7, "Add Y", "https://x/pull/7");
        let rows = [ContextSourceRecord::new("pr_body", SourceState::Absent)];
        let wrapped = run_json_value(&result, Some(&rows));
        assert_eq!(wrapped["result"], run_json_payload(&result));
        assert_eq!(
            wrapped["context_sources"],
            serde_json::json!([{"source": "pr_body", "state": "absent"}])
        );
        assert_eq!(
            run_json_value(&result, Some(&[]))["context_sources"],
            serde_json::json!([])
        );
    }

    /// #9192: the human output names an unavailable or truncated source, and
    /// says nothing about a used one (#9194: an absent one is named too).
    #[test]
    fn ledger_notes_name_unavailable_and_truncated_rows() {
        let mut down = ContextSourceRecord::new("pr_body", SourceState::Unavailable);
        down.detail = Some("local diff has no PR".to_string());
        let mut cut = ContextSourceRecord::new("caller_context", SourceState::Truncated);
        cut.chars_omitted = 6_000;
        let used = ContextSourceRecord::new("pr_body", SourceState::Used);
        assert_eq!(
            ledger_notes(&[down, used, cut]),
            [
                "context source pr_body: unavailable — local diff has no PR",
                "context source caller_context: truncated — 6000 characters omitted",
            ]
        );
    }

    #[test]
    fn trigger_for_live_flag_true_forces_live() {
        assert_eq!(trigger_for_live_flag(true), TriggerDecision::ForceLive);
    }

    #[test]
    fn trigger_for_live_flag_false_forces_dry_run() {
        assert_eq!(trigger_for_live_flag(false), TriggerDecision::ForceDryRun);
    }

    /// REGRESSION (#4460): the ambient `PR_INTELLIGENCE_DRY_RUN=false` env var
    /// must never, by itself, enable live posting for `trusty-review run` —
    /// this is the exact incident that published a live review to PR #4436
    /// with nothing at the call site signalling it would happen.
    ///
    /// Why: pre-fix, `cmd_run` passed `TriggerDecision::None` into
    /// `ReviewInput`, so `effective_dry_run` deferred entirely to
    /// `config.dry_run` (the env var) — `effective_dry_run(false,
    /// TriggerDecision::None)` is `false` (live). This test fails against
    /// that behaviour: it asserts the run stays dry-run even when the ambient
    /// config says "live" (`config_dry_run = false`), as long as `--live` was
    /// not passed.
    /// What: `trigger_for_live_flag(false)` folded through the same
    /// `effective_dry_run` formula the pipeline uses, with `config_dry_run =
    /// false` (the ambient "live" env value) — asserts the result is still
    /// dry-run.
    /// Test: this test.
    #[test]
    fn dry_run_env_var_alone_never_enables_posting() {
        let trigger = trigger_for_live_flag(false);
        assert!(
            effective_dry_run(false, trigger),
            "no --live flag must stay dry-run even when the ambient env says live"
        );
    }

    /// `--live` must post even when the ambient env says dry-run — the other
    /// half of "beats the ambient env either way" (#4460).
    /// Why: an operator who explicitly passes `--live` has stated intent at
    /// the call site; a stale `PR_INTELLIGENCE_DRY_RUN=true` left over from a
    /// different invocation must not silently override that.
    /// What: `trigger_for_live_flag(true)` folded through `effective_dry_run`
    /// with `config_dry_run = true` (the ambient "dry" env value) — asserts
    /// the result is live.
    /// Test: this test.
    #[test]
    fn live_flag_posts_even_when_ambient_env_says_dry_run() {
        let trigger = trigger_for_live_flag(true);
        assert!(
            !effective_dry_run(true, trigger),
            "--live must post even when the ambient env says dry-run"
        );
    }

    #[test]
    fn posting_mode_banner_live() {
        assert_eq!(
            posting_mode_banner(true),
            "posting: enabled (--live) — this run will post a review comment to GitHub"
        );
    }

    #[test]
    fn posting_mode_banner_dry_run() {
        assert_eq!(
            posting_mode_banner(false),
            "posting: dry-run (default) — pass --live to post a review comment to GitHub"
        );
    }

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

    // ── surface_for_diff_source (#3030 tracked MEDIUM (a)) ──────────────────

    /// A GitHub PR diff source can post a live comment, so it must map to the
    /// strict `Hosted` surface — unchanged behaviour (zero regression for the
    /// gate use case).
    #[test]
    fn surface_for_diff_source_github_is_hosted() {
        let source = DiffSource::Github {
            owner: "acme".to_string(),
            repo: "repo".to_string(),
            pr: 1,
            token: "tok".to_string(),
        };
        assert_eq!(surface_for_diff_source(&source), InvocationSurface::Hosted);
    }

    /// `--local-diff <PATH>` never posts to GitHub — must map to `Interactive`
    /// so an unreachable search degrades instead of hard-skipping.
    #[test]
    fn surface_for_diff_source_local_file_is_interactive() {
        let source = DiffSource::LocalFile {
            path: std::path::PathBuf::from("/tmp/x.diff"),
        };
        assert_eq!(
            surface_for_diff_source(&source),
            InvocationSurface::Interactive
        );
    }

    /// `--base`/`--head` (issue #2993) never posts to GitHub — must map to
    /// `Interactive`.
    #[test]
    fn surface_for_diff_source_git_range_is_interactive() {
        let source = DiffSource::GitRange {
            repo_root: std::path::PathBuf::from("/repo"),
            base: "main".to_string(),
            head: None,
        };
        assert_eq!(
            surface_for_diff_source(&source),
            InvocationSurface::Interactive
        );
    }

    /// `--local-diff -` (stdin) never posts to GitHub — must map to
    /// `Interactive`.
    #[test]
    fn surface_for_diff_source_stdin_is_interactive() {
        assert_eq!(
            surface_for_diff_source(&DiffSource::Stdin),
            InvocationSurface::Interactive
        );
    }

    // ── dedup_need_for (#5113) ──────────────────────────────────────────────

    /// REGRESSION (#5113): a posting-capable `run` against a GitHub PR must
    /// declare the claim store required.
    ///
    /// Why: `cmd_run` sets `allow_posting: true` and `build_deps_async` hardcoded
    /// `dedup: None`, so a re-run against the same `(owner, repo, pr, head_sha)`
    /// posted a second comment with nothing to stop it.
    /// What: asserts the need decision for the posting GitHub case.
    /// Test: this test.
    #[test]
    fn dedup_need_for_github_posting_is_required() {
        let source = DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: "tok".to_string(),
        };
        assert_eq!(dedup_need_for(&source, true), DedupNeed::Required);
    }

    /// REGRESSION (#9348): `run` without `--live` cannot post, so it must not
    /// open — or claim in — the per-user dedup store a live run and the
    /// `webhook-listen` worker share.
    /// What: the need `cmd_run` computes for a GitHub source, with and without
    /// `--live`.
    /// Test: this test.
    #[test]
    fn dry_run_does_not_need_the_dedup_store() {
        let source = DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: "tok".to_string(),
        };
        assert_eq!(
            dedup_need_for(&source, run_can_post(false)),
            DedupNeed::NotNeeded
        );
        assert_eq!(
            dedup_need_for(&source, run_can_post(true)),
            DedupNeed::Required
        );
    }

    /// A GitHub source that cannot post (`compare`, the MCP tools) has nothing
    /// for the claim gate to guard, so it must not open — or lock — the file.
    #[test]
    fn dedup_need_for_github_without_posting_is_not_needed() {
        let source = DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: "tok".to_string(),
        };
        assert_eq!(dedup_need_for(&source, false), DedupNeed::NotNeeded);
    }

    /// Every local source is forced log-only downstream, so posting stays
    /// unreachable even with `allow_posting: true` — which is exactly how the
    /// CLI runs `--local-diff` and `--base`.
    #[test]
    fn dedup_need_for_local_sources_is_not_needed() {
        let sources = [
            DiffSource::LocalFile {
                path: std::path::PathBuf::from("/tmp/x.diff"),
            },
            DiffSource::GitRange {
                repo_root: std::path::PathBuf::from("/repo"),
                base: "main".to_string(),
                head: None,
            },
            DiffSource::Stdin,
        ];
        for source in &sources {
            assert_eq!(
                dedup_need_for(source, true),
                DedupNeed::NotNeeded,
                "a local source can never post: {source:?}"
            );
        }
    }

    // ── --config (#8947) ────────────────────────────────────────────────────

    /// REGRESSION (#8947): `run` must keep the `--config` file's verification
    /// settings when it rebuilds the config with its model overrides.
    #[test]
    #[serial_test::serial]
    fn run_config_honours_the_config_file_verification_settings() {
        // SAFETY: `#[serial]` keeps every env-mutating test in this binary
        // from running concurrently with this one.
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_VERIFICATION_ENABLED");
            std::env::remove_var("TRUSTY_REVIEW_VERIFIER_MODEL");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("review.toml");
        std::fs::write(
            &path,
            "[verification]\nenabled = false\n\n[models.verifier]\nmodel = \"test/verifier-8947\"\n",
        )
        .expect("write config");
        let args = RunArgs::try_parse_from(["run", "--base", "main"]).expect("parse");

        let config = run_config(Some(&path), &args);

        assert!(
            !config.verification.enabled,
            "[verification] enabled = false from --config must reach the run"
        );
        assert_eq!(config.role_models.verifier.model, "test/verifier-8947");
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

    /// An explicit `--source-root` beats `TRUSTY_SEARCH_INDEX`
    /// (`search_index_explicit = true`), which can be ambient (#8651).
    /// Why: an env pin used to make the helper return early, so the per-run
    /// flag was ignored.
    /// What: sets the env pin, passes `Some(dir)` and an unreachable client,
    /// and asserts the helper still queried the daemon: it returns the
    /// diff-only notice and turns `require_search` off. The env-pinned index
    /// is never used for retrieval, because the caller nulls both clients.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_source_root_arg_beats_env_index() {
        let mut config = ReviewConfig::load(None);
        config.search_index = "ambient-env-pin".to_string();
        config.search_index_explicit = true;
        config.context.require_search = Some(true);
        let client = HttpSearchClient::new("http://127.0.0.1:1").expect("client init");

        let notice = resolve_source_root_arg(
            &mut config,
            &client,
            Some(std::path::Path::new("/tmp/proj")),
        )
        .await;

        let notice = notice.expect("--source-root must be resolved despite the env pin");
        assert!(notice.contains("--source-root"), "notice: {notice}");
        assert_eq!(config.context.require_search, Some(false));
        assert!(config.search_index_explicit);
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

// #8651 / #8654: per-repo index and PR-context flags for `run`.
#[cfg(test)]
#[path = "run_pr_tests.rs"]
mod pr_tests;

// #9197: `--issue-docs-file`.
#[cfg(test)]
#[path = "run_issue_docs_tests.rs"]
mod issue_docs_tests;
