//! The two re-read commands a resume slot's `verify` may hold (#9390).
//!
//! Why: the pause writes a slot's `verify` lines and the resume executes them.
//! The executed text comes back from a store, so anything not built by the
//! writer must never run. One definition of the allowed shapes serves both
//! sides: the writer formats with [`AllowedCommand::line`], and the reader runs
//! only what [`AllowedCommand::parse`] accepts.
//! What: [`AllowedCommand`] is `gh pr view <digits> --json headRefOid,state` or
//! `git rev-parse <branch>`, where the branch passes [`is_safe_branch`]. The
//! reader gets an argv ([`AllowedCommand::program`], [`AllowedCommand::args`]),
//! never a shell string.
//! Test: `the_allowlist_refuses_every_other_shape`,
//! `the_allowlist_accepts_exactly_what_the_writer_formats`,
//! `an_unsafe_branch_gets_no_command`.

/// The most PR numbers one slot names; a longer list is noise, not a plan.
pub(crate) const MAX_PRS: usize = 10;

/// The most digits a PR number may carry, so it always fits a `u64`.
const MAX_PR_DIGITS: usize = 9;

/// The `--json` field list the `gh` shape asks for, verbatim.
const GH_FIELDS: &str = "headRefOid,state";

/// One allowlisted re-read command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AllowedCommand {
    /// `gh pr view <n> --json headRefOid,state`.
    GhPrView(u64),
    /// `git rev-parse <branch>`, the branch already checked.
    GitRevParse(String),
}

impl AllowedCommand {
    /// The `git rev-parse` command for `branch`, when the branch is safe.
    pub(crate) fn git_rev_parse(branch: &str) -> Option<Self> {
        is_safe_branch(branch).then(|| Self::GitRevParse(branch.to_string()))
    }

    /// The `verify` line the writer stores for this command.
    pub(crate) fn line(&self) -> String {
        match self {
            Self::GhPrView(n) => format!("gh pr view {n} --json {GH_FIELDS}"),
            Self::GitRevParse(branch) => format!("git rev-parse {branch}"),
        }
    }

    /// Parse one stored `verify` line; `None` for every non-allowlisted line.
    ///
    /// What: the line must be printable ASCII (no control character, so no
    /// embedded newline or carriage return), split on single spaces into
    /// exactly one of the two token sequences. The PR number is 1 to
    /// [`MAX_PR_DIGITS`] ASCII digits and not zero; the branch passes
    /// [`is_safe_branch`]. Anything else, including an extra argument, a
    /// doubled space or a leading option, is refused.
    /// Test: `the_allowlist_refuses_every_other_shape`.
    pub(crate) fn parse(line: &str) -> Option<Self> {
        if !line.bytes().all(|b| (b' '..=b'~').contains(&b)) {
            return None;
        }
        let tokens: Vec<&str> = line.split(' ').collect();
        match tokens.as_slice() {
            ["gh", "pr", "view", number, "--json", fields] if *fields == GH_FIELDS => {
                pr_number(number).map(Self::GhPrView)
            }
            ["git", "rev-parse", branch] => Self::git_rev_parse(branch),
            _ => None,
        }
    }

    /// The program to execute: `gh` or `git`, resolved on `PATH`.
    pub(crate) fn program(&self) -> &'static str {
        match self {
            Self::GhPrView(_) => "gh",
            Self::GitRevParse(_) => "git",
        }
    }

    /// The argv after the program, one element per token.
    pub(crate) fn args(&self) -> Vec<String> {
        match self {
            Self::GhPrView(n) => ["pr", "view", &n.to_string(), "--json", GH_FIELDS]
                .map(str::to_string)
                .to_vec(),
            // `--verify --end-of-options` would be stricter, but the branch is
            // already proven option-free and the shape is the writer's own.
            Self::GitRevParse(branch) => vec!["rev-parse".to_string(), branch.clone()],
        }
    }
}

/// A PR number token: digits only, no sign, no leading zero-only value.
fn pr_number(token: &str) -> Option<u64> {
    let digits_only = !token.is_empty()
        && token.len() <= MAX_PR_DIGITS
        && token.bytes().all(|b| b.is_ascii_digit());
    digits_only
        .then(|| token.parse::<u64>().ok())
        .flatten()
        .filter(|n| *n > 0)
}

/// A branch name safe to put in a command: no shell metacharacter, no option.
///
/// What: not `HEAD`, at most 200 bytes, no `..`, starts with an ASCII
/// alphanumeric, and holds only ASCII alphanumerics and `._-/`.
/// Test: `an_unsafe_branch_gets_no_command`.
pub(crate) fn is_safe_branch(branch: &str) -> bool {
    branch != "HEAD"
        && branch.len() <= 200
        && !branch.contains("..")
        && branch
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
}
