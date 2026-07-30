//! Built-in skills for the L0-only GitHub PR/CI inspection surface (#4170).
//!
//! Why: The owner's ruling is one skill per tool — *"There can be other skills
//! without tools, but each tool gets its own skill"* — and
//! `every_tool_declared_in_source_has_a_skill` enforces it. These five rows
//! wrap `crate::tools::gh_tools`. They live in their OWN family file rather
//! than being appended to `ops.rs` so the GitHub surface can grow (or a
//! mutating tier be added later under a separate decision) without touching a
//! table three other tool families already share.
//!
//! A card here does NOT imply reachability: `gh_tools` refuses to construct
//! these executors for any tier other than L0 orchestration, so an L1 persona
//! naming one of these skill ids resolves to a tool that its registry never
//! contains and is granted nothing. See `tools::gh_tools`'s module doc.
//! What: A `const` table of one-tool [`SkillDef`] rows, all `Action` kind —
//! these are things a user asks for by name ("check PR 4200's CI"), not
//! internal plumbing to collapse away like the `System` rows in `ops.rs`.
//! Test: `super::super::tests::every_tool_declared_in_source_has_a_skill`,
//! `crate::tools::gh_tools::tests::every_gh_tool_has_a_skill_in_the_builtin_catalog`.

use super::super::{ProviderReq, SkillDef, SkillKind::Action, tool_skill};

/// The authenticated GitHub CLI these five skills all shell out to.
///
/// Why: #3945's honesty discipline — a PR-checks card must not imply it works
/// when `gh` was never authenticated. `env_var` is `None` because `gh auth
/// login` stores its grant in the system keyring, not an environment variable:
/// there is nothing this process can probe, so the status stays unclaimed
/// rather than being guessed. (`GH_TOKEN` is an alternative `gh` honours, but
/// its ABSENCE says nothing about whether `gh` is authenticated, so reporting
/// on it would be worse than reporting nothing.)
pub(super) static GH_CLI: ProviderReq = ProviderReq {
    provider: "GitHub CLI",
    requirement: "An installed, authenticated GitHub CLI (`gh auth login`). \
                  Not verified by this endpoint.",
    env_var: None,
};

pub(super) static TABLE: &[SkillDef] = &[
    tool_skill(
        "github-pr-list",
        "Browse Pull Requests",
        "List a repository's pull requests by state or author.",
        "gh_pr_list",
        Action,
        Some(&GH_CLI),
    ),
    tool_skill(
        "github-pr-read",
        "Read a Pull Request",
        "Show one pull request's title, state, description and metadata.",
        "gh_pr_view",
        Action,
        Some(&GH_CLI),
    ),
    tool_skill(
        "github-pr-checks",
        "Pull Request Check Status",
        "Report every CI check run on one pull request, including red and pending ones.",
        "gh_pr_checks",
        Action,
        Some(&GH_CLI),
    ),
    tool_skill(
        "github-run-list",
        "Browse CI Runs",
        "List recent GitHub Actions runs for a repository, branch or workflow.",
        "gh_run_list",
        Action,
        Some(&GH_CLI),
    ),
    tool_skill(
        "github-run-read",
        "Read a CI Run",
        "Show one GitHub Actions run's jobs, their conclusions, and the failing steps' logs.",
        "gh_run_view",
        Action,
        Some(&GH_CLI),
    ),
];
