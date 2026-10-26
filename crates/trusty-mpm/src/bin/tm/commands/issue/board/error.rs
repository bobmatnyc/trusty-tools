//! `BoardError` — every refusal and failure of the `tm issue board` verbs
//! (#10276).
//!
//! Why: each refusal the Phase 2 acceptance criteria name must carry the value
//! it is about (the option and its item count, the same-title project number,
//! the dirty file, the tracker), and a test asserts that value. One variant per
//! refusal makes the assertion exact.
//! What: [`BoardError`] (`thiserror`). The verbs return it; the CLI glue turns
//! it into an `anyhow` error, which is how `tm` exits non-zero.
//! Test: each variant is produced by a test in `board/verb_tests.rs` or
//! `board/attach_tests.rs`; `every_board_error_variant_renders_its_value_10276`
//! renders them all.

use super::validate::BoardSpecError;

/// A `tm issue board` refusal or failure.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BoardError {
    /// The board spec or the `project:` block failed its load-time rules.
    #[error(transparent)]
    Spec(#[from] BoardSpecError),
    /// A `gh` or `git` call failed.
    #[error("`{what}` failed: {detail}")]
    Command {
        /// The call, as `gh api graphql (<operation>)` or the argv head.
        what: String,
        /// The captured stderr, or the spawn error.
        detail: String,
    },
    /// A `gh` answer did not have the expected shape.
    #[error("could not read the answer to `{what}`: {detail}")]
    Parse {
        /// The call whose answer is unreadable.
        what: String,
        /// What was wrong with it.
        detail: String,
    },
    /// D9: the repo's tracker is not GitHub.
    #[error(
        "tm issue board works only with a GitHub tracker; this repo's tracker is `{tracker}` (epic #10274, D9)"
    )]
    NotGitHub {
        /// The tracker the repo uses.
        tracker: String,
    },
    /// The repo's `issue-state.yaml` has no `project:` block.
    #[error(
        "no `project:` block in this repo's issue-state.yaml{fallback} — run `tm issue board init` \
         and commit the block it prints (a block in the user-level issue-state.yaml is not inherited)"
    )]
    NoProjectBlock {
        /// A note on the `default_project` fallback, or empty.
        fallback: String,
    },
    /// `init --apply`: the owner already has a project with this title.
    #[error(
        "`{owner}` already has project #{number} titled `{title}` — refusing to create a second one"
    )]
    SameTitle {
        /// The owner login.
        owner: String,
        /// The title asked for.
        title: String,
        /// The existing project's number.
        number: u64,
    },
    /// `init --apply`: the repo is already linked to a project.
    #[error(
        "`{repo}` is already linked to project(s) {numbers:?} — refusing to create another board; \
         name the existing one in the `project:` block and run `tm issue board apply`"
    )]
    RepoLinked {
        /// The repo slug.
        repo: String,
        /// The linked projects' numbers.
        numbers: Vec<u64>,
    },
    /// `init --apply`: `issue-state.yaml` has uncommitted changes.
    #[error(
        "`{path}` has uncommitted changes — commit or discard them first, so the printed `project:` \
         block goes into a clean file"
    )]
    DirtyIssueState {
        /// The file.
        path: String,
    },
    /// Owner decision 2: an option an item still uses would be removed.
    #[error(
        "refusing to remove option(s) from `{field}` that items still use: {} — removing an option \
         clears every item that uses it, and re-adding it does not restore them",
        render_in_use(.in_use)
    )]
    OptionInUse {
        /// The field.
        field: String,
        /// Each option to remove with the number of items using it.
        in_use: Vec<(String, usize)>,
    },
    /// A standard field exists on the board with a different data type.
    #[error("board field `{field}` is {found}; the spec declares {expected} — fix it by hand")]
    FieldTypeMismatch {
        /// The field.
        field: String,
        /// The GitHub data type the spec needs.
        expected: String,
        /// The data type on the board.
        found: String,
    },
    /// The named project does not exist or this token cannot see it.
    #[error("project #{number} of `{owner}` was not found, or this token cannot see it")]
    ProjectNotFound {
        /// The owner login.
        owner: String,
        /// The project number.
        number: u64,
    },
    /// `add`: neither a `project:` block nor a usable `default_project`.
    #[error("no board to attach to: {0}")]
    NoAttachTarget(String),
    /// D10: an attach failure that is not the token's missing project scope.
    #[error(
        "could not add #{issue} to project #{number} ({kind}), and the failure is not the \
         token's project scope — refusing to record it as a permanent `no-project:` comment; \
         the issue is filed: {detail}"
    )]
    AttachFailed {
        /// The issue.
        issue: u64,
        /// The project number.
        number: u64,
        /// The failure class.
        kind: &'static str,
        /// The `gh` error text.
        detail: String,
    },
    /// The backfill cursor file could not be read or written.
    #[error("backfill cursor `{path}`: {detail}")]
    Cursor {
        /// The cursor file.
        path: String,
        /// The I/O or parse error.
        detail: String,
    },
}

/// `` `Todo` (3 items), `Done` (1 item) ``.
fn render_in_use(in_use: &[(String, usize)]) -> String {
    in_use
        .iter()
        .map(|(option, n)| {
            let unit = if *n == 1 { "item" } else { "items" };
            format!("`{option}` ({n} {unit})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}
