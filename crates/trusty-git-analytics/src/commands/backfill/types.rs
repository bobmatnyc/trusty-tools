//! Shared argument types and data structures for `tga backfill`.
//!
//! Why: clap arg structs and the `EffortRow` internal record type are used
//! across multiple submodules; centralising them avoids cyclic imports and keeps
//! each submodule focused on logic rather than type declarations.

use clap::{Args, Subcommand};

/// Arguments for `tga backfill`.
#[derive(Args, Debug)]
#[command(
    about = "Retroactive maintenance operations on existing commit rows.",
    long_about = "Re-run extraction or scoring steps on commits already in the database.\n\n\
These operations update existing rows in-place rather than ingesting new data.\n\
Each subcommand supports --dry-run to preview changes without writing.\n\n\
NOTE: --branch is collect-only. Commits in the DB do not carry branch\n\
attribution after the walk, so there is no branch filter on backfill operations.\n\
If you need to re-walk specific branches, use `tga collect --branch <name>`.\n\n\
TIPS:\n\
  - Use --repos to limit scope to one service at a time on large corpora.\n\
  - Use --since/--until or --weeks to limit the date window for fast iteration.",
    after_help = "EXAMPLES:\n\
  # Re-extract ticket IDs for all commits (after pattern change)\n\
  tga backfill ticket-ids\n\n\
  # Re-score effort for the last 4 weeks of one repo\n\
  tga backfill effort --repos my-service --weeks 4 --force\n\n\
  # Re-run reachability scan after adding release-branch patterns\n\
  tga backfill reachability --repos core-api"
)]
pub struct BackfillArgs {
    /// Backfill subcommand.
    #[command(subcommand)]
    pub subcommand: BackfillSubcommand,
    /// Report what would change without writing.
    #[arg(long, default_value_t = false, global = true)]
    pub dry_run: bool,
    /// Limit backfill to these repository names (comma-separated). [global]
    ///
    /// Matches against the `repository` column in the `commits` table
    /// (for ticket-ids, revert-flags) or the repo `name` in config
    /// (for reachability, effort). When omitted, all repos are processed.
    ///
    /// NOTE: not applicable to ai-detection (global LLM re-classification).
    #[arg(long, value_delimiter = ',', global = true)]
    pub repos: Vec<String>,
    /// Limit backfill to commits in the last N ISO weeks. [global]
    ///
    /// Restricts the set of commits processed by timestamp. Mutually exclusive
    /// with --since/--until. Not applicable to reachability (uses config repos).
    #[arg(long, value_name = "N", global = true, conflicts_with_all = ["since", "until"])]
    pub weeks: Option<u32>,
    /// Limit backfill to commits on or after this date (ISO8601: YYYY-MM-DD). [global]
    ///
    /// Lower bound on the author timestamp. Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", global = true, conflicts_with = "weeks")]
    pub since: Option<String>,
    /// Limit backfill to commits on or before this date (ISO8601: YYYY-MM-DD). [global]
    ///
    /// Upper bound on the author timestamp. Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", global = true, conflicts_with = "weeks")]
    pub until: Option<String>,
}

/// `tga backfill` subcommands.
#[derive(Subcommand, Debug)]
pub enum BackfillSubcommand {
    /// Re-run LLM classification on low-confidence prior LLM verdicts.
    ///
    /// Clears `classification_id` on commits classified by the LLM tier
    /// with confidence < 0.7, making them eligible for re-classification
    /// on the next `tga classify` run. Use `tga classify --force` after
    /// this to immediately re-process the cleared commits.
    AiDetection,
    /// Scan commit messages for revert patterns and update `is_revert`.
    ///
    /// Detects `Revert "..."`, `revert:`, and `revert"` prefixes
    /// (case-insensitive). Use --repos/--since/--until to limit scope.
    RevertFlags,
    /// Scan commit messages for ticket references and update `ticket_id`/`ticketed`.
    ///
    /// Useful after extending ticket patterns or when collecting a new
    /// repo whose commits were never run through ticket extraction.
    /// --branch is collect-only and not applicable here.
    TicketIds,
    /// Re-run the tag/branch/default-branch reachability scan.
    ///
    /// Upserts `fact_commit_reachability` rows without re-collecting commits.
    /// Use this to fix `on_default_branch=0` rows in existing databases
    /// without running the full 20-minute `tga collect` pipeline (issue #290).
    ///
    /// Use --repos (via BackfillArgs) to limit to specific repositories.
    /// --branch is collect-only; reachability is computed from the live git
    /// repo graph, not from the branch the commits were originally collected on.
    Reachability,
    /// Compute empirical effort scores for historical commits.
    ///
    /// Persists scores in `fact_commit_effort` using the v1 formula
    /// (LoC + file count + tests factor, mapped to XS/S/M/L/XL).
    ///
    /// Default path (db-only): reads from `commits JOIN files` — no on-disk
    /// git repo required. Use --range or --notes to switch to the git path.
    ///
    /// --branch is collect-only and not applicable here.
    Effort(EffortBackfillArgs),
    /// Fill in missing `complexity` scores (1–5) for already-classified commits.
    ///
    /// The `complexity` column is only ever populated by the LLM tier, which
    /// the normal `tga classify` run consults solely for low-confidence
    /// commits. Commits resolved by rules or external sources (JIRA/GitHub)
    /// therefore keep `complexity = NULL`. This subcommand asks the LLM for a
    /// 1–5 complexity score for every classification with `complexity IS NULL`
    /// and a non-`exact_rule` method, leaving category/confidence/method
    /// untouched. Requires `use_llm: true` (or `--use-llm`) and an LLM API key.
    ///
    /// Equivalent to `tga classify --backfill-complexity`; exposed here so the
    /// operation is discoverable under `tga backfill` (issue #397, bug 2).
    /// --repos/--since/--until/--weeks do not scope this operation: all NULL
    /// rows are processed.
    Complexity(ComplexityBackfillArgs),
    /// Recompute `commits.ticketed` using the fixed regex rules (issue #445).
    ///
    /// Bare `#N` refs no longer mark a commit as ticketed; only JIRA/Linear
    /// (`PROJ-N`), GitHub action keywords (`closes/fixes/resolves #N`), and
    /// Azure DevOps (`AB#N`) do. This subcommand re-evaluates every stored
    /// `commits.message` with the corrected [`is_ticketed`] and updates rows
    /// that differ from the stored value. No LLM required — pure regex.
    ///
    /// Use --repos/--since/--until to limit scope on large databases.
    Ticketed,
    /// Scan existing `commits.message` for AI co-authorship trailers (issue #445).
    ///
    /// Detects `Co-Authored-By:` trailers for Claude, GitHub Copilot, and
    /// Cursor; sets `commits.is_ai_assisted` and `commits.ai_tool`.
    /// No LLM required — pure string matching.
    ///
    /// Use --repos/--since/--until to limit scope.
    AiDetectionCommits,
    /// Fill in `classifications.top_level_category` from existing subcategory
    /// values using the built-in taxonomy (issue #445).
    ///
    /// The top_level_category column was added in migration v17 and is
    /// populated for new classifications at write time. This subcommand
    /// retroactively fills existing rows by resolving each subcategory through
    /// the taxonomy registry. No LLM required.
    TopLevel,
    /// Fill in `fact_commit_effort.effort_tshirt` from existing `size` values
    /// (issue #445).
    ///
    /// Maps the text size label (XS/S/M/L/XL) to the numeric T-shirt integer
    /// (1–5) for existing rows that pre-date migration v17.
    EffortTshirt,
    /// Recompute and persist per-engineer-per-week quality scores to
    /// `fact_weekly_quality` for all historical data (issue #445 batch B).
    ///
    /// Reads the full `commits` table (left-joined against `classifications`
    /// and `authors`), re-runs the aggregator's weekly bucketing logic, and
    /// UPSERTs every (author, week, repo) grain into `fact_weekly_quality`.
    /// Idempotent — running twice produces the same result. Supports --dry-run
    /// to report the row count without writing.
    ///
    /// No LLM required — quality scoring is a pure formula applied to
    /// per-bucket counts of reverts, bugfixes, and ticketed commits.
    Quality,
}

/// Arguments for `tga backfill complexity`.
#[derive(Args, Debug)]
pub struct ComplexityBackfillArgs {
    /// Enable the LLM tier for this run even if `config.classification.use_llm`
    /// is `false`.
    ///
    /// Complexity scoring is LLM-only, so the LLM tier must be on. Pass this
    /// flag (or set `use_llm: true` in config) along with an API key
    /// (`OPENAI_API_KEY` / `OPENROUTER_API_KEY`).
    #[arg(long, default_value_t = false)]
    pub use_llm: bool,
}

/// Arguments for `tga backfill effort`.
#[derive(Args, Debug)]
pub struct EffortBackfillArgs {
    /// Scope effort computation to a git commit range (e.g. `HEAD~10..HEAD`).
    ///
    /// When omitted, all commits in the chosen repo(s) that do not already
    /// have a `fact_commit_effort` row are processed (unless `--force`).
    /// Requires a live on-disk git repository.
    #[arg(long, value_name = "RANGE")]
    pub range: Option<String>,

    /// Recompute effort even if a row already exists (UPSERT semantics).
    ///
    /// Without this flag, commits that already have a row in
    /// `fact_commit_effort` are skipped.  With `--force`, every commit is
    /// re-scored and the existing row is replaced.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Also write a git note to `refs/notes/effort` for each scored commit.
    ///
    /// The note body is `Effort: <size>` (e.g. `Effort: M`), matching the
    /// format the pre-commit hook injects into commit messages.  Off by
    /// default to keep the backfill lightweight. Requires a live git repo.
    #[arg(long, default_value_t = false)]
    pub notes: bool,

    /// Maximum commits to process per repository.
    ///
    /// Useful for smoke-testing on a large corpus.  When omitted, all
    /// eligible commits are processed.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

/// A single row to be written to `fact_commit_effort`.
///
/// Why: multiple effort-path functions (db path and git path) produce
/// homogeneous output that `persist_effort_rows` can batch-insert.
/// What: holds all columns for one `fact_commit_effort` row.
/// Test: constructed in `effort.rs` tests via `backfill_effort_persists_rows`.
pub(super) struct EffortRow {
    pub sha: String,
    pub repository: String,
    pub size: String,
    pub score: f64,
    pub loc: u32,
    pub files: u32,
    pub test_loc: u32,
    pub tests_factor: f64,
    pub formula_version: String,
    pub computed_at: i64,
    /// Numeric T-shirt size (static label mapping): XS=1, S=2, M=3, L=4, XL=5.
    ///
    /// Note: `persist_effort_rows` recomputes this from stored percentile thresholds
    /// when available. This field is used as a fallback when no thresholds are stored.
    #[allow(dead_code)]
    pub effort_tshirt: i64,
}
