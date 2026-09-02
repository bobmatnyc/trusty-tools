//! `tm pr` — deterministic pull-request open and merge-queue gates.
//!
//! Why: `version-control` opens every PR by hand-assembling `gh pr create`,
//! and re-derives four separate judgments each time — is the seven-field body
//! complete, is the attribution footer exact, were the shipped labels and
//! assignee attached, does this diff owe a changelog fragment. Each of those
//! is a mechanical check the agent currently performs from prose in
//! `tm-workflow.md`. The same is true before a merge: the merge-queue
//! procedure is a fixed decision table read out of three `gh` calls.
//! Neither needs a model.
//! What: [`PrCmd`] — `open` (validate, then spawn `gh pr create`) and
//! `queue-check` (one verdict line per open PR on a base branch).
//! Test: `cli_parses_pr_*` in `tests.rs`; the semantics live in
//! `commands::pr`.

use std::path::PathBuf;

/// Verbs for the `tm pr` command group.
///
/// Why: the two verbs share a `gh` seam and the repo's PR conventions but
/// have disjoint flag sets, so a sub-subcommand enum keeps each parseable on
/// its own and keeps `Command` under the 500-SLOC production cap.
/// What: `Open` (pre-flight checks then `gh pr create`) and `QueueCheck`
/// (merge-queue stop-condition evaluation).
/// Test: `cli_parses_pr_open`, `cli_parses_pr_queue_check` in `tests.rs`.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum PrCmd {
    /// Validate a PR body against the seven-field contract, then open the PR.
    Open(PrOpenArgs),
    /// Report, per open PR on a base branch, whether it is mergeable.
    QueueCheck(PrQueueCheckArgs),
}

/// Flags for `tm pr open`.
///
/// Why: every field here replaces a judgment the agent otherwise makes from
/// prose — the body contract, the `Refs` vs `Closes` rule, the test-ladder
/// rung claimed, and whether the changelog gate applies.
/// What: the `gh pr create` inputs plus the four pre-flight switches.
/// Test: `cli_parses_pr_open`, `cli_rejects_pr_open_rung_out_of_range`.
#[derive(Debug, clap::Args)]
pub(crate) struct PrOpenArgs {
    /// PR title.
    #[arg(long)]
    pub(crate) title: String,

    /// Path to the PR body (Markdown), checked against the seven-field contract.
    #[arg(long = "body-file")]
    pub(crate) body_file: PathBuf,

    /// Issue this PR references. Emits `Refs #N` unless `--closes` is given.
    #[arg(long)]
    pub(crate) issue: Option<u64>,

    /// Emit `Closes #N` instead of `Refs #N`. Fix PRs never auto-close.
    #[arg(long)]
    pub(crate) closes: bool,

    /// Test-ladder rung this PR claims (1-6).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=6))]
    pub(crate) rung: Option<u8>,

    /// Base branch to open against.
    #[arg(long, default_value = "main")]
    pub(crate) base: String,

    /// Skip the changelog-fragment gate — docs-only / CI-only PRs may.
    #[arg(long = "docs-only")]
    pub(crate) docs_only: bool,

    /// Workstream label source. Defaults to `$TM_SESSION_NAME`, else tmux.
    #[arg(long)]
    pub(crate) session: Option<String>,

    /// `owner/repo` (defaults to the cwd's git remote).
    #[arg(long)]
    pub(crate) repo: Option<String>,

    /// Print the assembled `gh` argv and exit 0 without calling `gh`.
    #[arg(long = "dry-run")]
    pub(crate) dry_run: bool,
}

/// Flags for `tm pr queue-check`.
///
/// Why: the merge-queue procedure names a base branch and, optionally, one PR
/// to check rather than the whole queue.
/// What: the base branch, an optional single PR, `--repo`, and `--json`.
/// Test: `cli_parses_pr_queue_check`.
#[derive(Debug, clap::Args)]
pub(crate) struct PrQueueCheckArgs {
    /// Check only this PR instead of every open PR on `--base`.
    pub(crate) pr: Option<u64>,

    /// Base branch whose open PRs are checked.
    #[arg(long, default_value = "main")]
    pub(crate) base: String,

    /// `owner/repo` (defaults to the cwd's git remote).
    #[arg(long)]
    pub(crate) repo: Option<String>,

    /// Emit the same verdicts as a JSON array instead of one line per PR.
    #[arg(long)]
    pub(crate) json: bool,
}
