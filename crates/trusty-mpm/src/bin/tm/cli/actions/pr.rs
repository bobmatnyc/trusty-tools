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
//! What: [`PrCmd`] — `open` (validate, then spawn `gh pr create`), `merge`
//! (re-validate, then squash-merge from that body), and `queue-check` (one
//! verdict line per open PR on a base branch).
//! Test: `cli_parses_pr_*` in `tests.rs`; the semantics live in
//! `commands::pr`.

use std::path::PathBuf;

/// Verbs for the `tm pr` command group.
///
/// Why: the verbs share a `gh` seam and the repo's PR conventions but have
/// disjoint flag sets, so a sub-subcommand enum keeps each parseable on its
/// own and keeps `Command` under the 500-SLOC production cap.
/// What: `Open` (pre-flight checks then `gh pr create`), `Merge` (re-validate
/// the body, then `gh pr merge --squash` from it), and `QueueCheck`
/// (merge-queue stop-condition evaluation).
/// Test: `cli_parses_pr_open`, `cli_parses_pr_merge`,
/// `cli_parses_pr_queue_check` in `tests.rs`.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum PrCmd {
    /// Validate a PR body against the seven-field contract, then open the PR.
    Open(PrOpenArgs),
    /// Squash-merge a PR with its validated body as the commit message.
    #[command(long_about = MERGE_LONG_ABOUT)]
    Merge(PrMergeArgs),
    /// Report, per open PR on a base branch, whether it is mergeable.
    QueueCheck(PrQueueCheckArgs),
    /// Reclaim everything a merged PR made obsolete: branches, worktrees, claims.
    #[command(long_about = CLEANUP_LONG_ABOUT)]
    Cleanup(PrCleanupArgs),
}

/// `tm pr cleanup --help` text.
///
/// Why (#7275): this command deletes branches and worktrees, so the five steps
/// and the one refusal are what a caller must know before running it, and
/// `--help` is where they look.
/// Test: `cli_pr_cleanup_help_states_the_steps`.
const CLEANUP_LONG_ABOUT: &str = "\
Reclaim everything a merged pull request made obsolete.

Runs as the final step after merge confirmation, and refuses outright unless \
`gh pr view <n>` reports state MERGED. Five steps, one line of output each:

  pr             confirm the PR is MERGED
  remote-branch  delete origin/<head> when the remote still lists it
  worktree       end any session claim, then remove each worktree holding the head
  local-branch   delete the head branch and each `worktree-agent-*` at the head commit
  prune          `git worktree prune`, then `git fetch --prune origin`

Exits 0 only when every step succeeded; a step that could not do its job is a \
failure, never a warning. A worktree holding uncommitted or unpushed work is \
the one refusal — cleanup never passes `--force`, so it stops with a nonzero \
exit and leaves the work alone.

A merged PR makes its worktree's session claim obsolete, so cleanup ends that \
claim (record-only — it never kills a process it does not own) and proceeds.

--dry-run runs every read and prints the plan without issuing a single \
mutating command.";

/// `tm pr merge --help` text.
///
/// Why (#6808): the refusal conditions and the `--auto` semantics are the two
/// things a caller must know before running this, and `--help` is where they
/// look. Keeping them in one const keeps the enum readable.
/// What: what the command does, the five refusals, the BEHIND carve-out, and
/// what `--auto` defers to GitHub.
/// Test: `cli_pr_merge_help_states_the_refusals`.
const MERGE_LONG_ABOUT: &str = "\
Squash-merge a PR with its validated body as the landing commit message.

Reads the PR, re-validates its body with the same seven-field-and-footer check \
`tm pr open` runs, then merges with `--squash --delete-branch --subject \
\"<title> (#<n>)\" --body-file <tmp>`, so the validated body IS the squash \
commit message rather than a concatenation of the branch's raw commit messages.

Refuses with a one-line reason, exits non-zero, and calls no merge when:
  - the PR body fails that validation
  - the PR is a draft
  - the PR carries a `do-not-merge` label (any case)
  - the review decision is CHANGES_REQUESTED
  - the PR has merge conflicts — `mergeable` CONFLICTING or `mergeStateStatus`
    DIRTY (resolve them with `gh pr update-branch <n>`)

A mergeStateStatus of BEHIND is NOT a refusal — a behind branch merges fine here. \
Every other mergeStateStatus — BLOCKED, UNSTABLE, HAS_HOOKS, UNKNOWN — and every \
reviewDecision other than CHANGES_REQUESTED are left to `gh pr merge` to accept or \
reject.

With --auto the merge is queued instead of performed, and GitHub applies the \
supplied subject and body when auto-merge fires. Under a merge queue GitHub \
ignores the supplied subject and body entirely; this repo has no merge queue.";

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

    /// Head branch to open FROM; requires --docs-only. Defaults to the
    /// checkout's current branch.
    ///
    /// Why (#7282): `gh pr create` infers the head from the checkout's current
    /// branch, so a caller that built its branch with git plumbing — the
    /// session-pause publisher does exactly that — had `gh` read `main` and
    /// abort with "you must first push the current branch to a remote, or use
    /// the --head flag". Naming the head makes the caller's checkout state
    /// irrelevant. `--docs-only` is required with it because the changelog gate
    /// can only diff the CHECKOUT's HEAD, so a source PR opened this way would
    /// clear the gate against a diff it does not contain (#7282 round 5). The
    /// pairing is enforced in `commands::pr::open::head_docs_only_conflict`,
    /// which names the obligation instead of a bare clap message.
    #[arg(long)]
    pub(crate) head: Option<String>,

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

/// Flags for `tm pr merge`.
///
/// Why (#6808): the merge needs only the PR number — the title and body come
/// from the PR itself, so nothing here can drift from what was reviewed. The
/// three switches cover the cases the merge-queue procedure actually names:
/// queue the merge rather than perform it, keep the remote branch, and target
/// a repo other than the cwd's.
/// What: the PR number plus `--auto`, `--no-delete-branch`, and `--repo`.
/// Test: `cli_parses_pr_merge`.
#[derive(Debug, clap::Args)]
pub(crate) struct PrMergeArgs {
    /// PR number to squash-merge.
    pub(crate) pr: u64,

    /// Arm GitHub auto-merge instead of merging now.
    #[arg(long)]
    pub(crate) auto: bool,

    /// Keep the remote branch instead of deleting it at merge time.
    #[arg(long = "no-delete-branch")]
    pub(crate) no_delete_branch: bool,

    /// `owner/repo` (defaults to the cwd's git remote).
    #[arg(long)]
    pub(crate) repo: Option<String>,
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

/// Flags for `tm pr cleanup` (#7275).
///
/// Why: the command is deliberately almost argument-free — the PR number is
/// the whole input, because the merged PR itself determines every branch,
/// worktree and claim the run touches. `--dry-run` is the only behavioural
/// switch, and there is no `--force`: forcing is what the dirty-worktree
/// refusal exists to prevent.
/// What: the PR number, `--repo`, and `--dry-run`.
/// Test: `cli_parses_pr_cleanup`, `cli_pr_cleanup_help_states_the_steps`.
#[derive(Debug, clap::Args)]
pub(crate) struct PrCleanupArgs {
    /// Merged PR number to clean up after.
    pub(crate) pr: u64,

    /// `owner/repo` (defaults to the cwd's git remote).
    #[arg(long)]
    pub(crate) repo: Option<String>,

    /// Print the plan and issue no mutating command.
    #[arg(long = "dry-run")]
    pub(crate) dry_run: bool,
}
