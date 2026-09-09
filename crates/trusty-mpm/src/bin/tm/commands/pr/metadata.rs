//! The PR side of the labels/project/milestone standard (#7274).
//!
//! Why: until the owner ruling of 2026-09-09 the standard reached issues only.
//! A PR `tm pr open` created carried the assignee, `trusty-mpm` and
//! `ws/<session>` and nothing else — no component label for the crates its own
//! diff touched, and no project or milestone from the issue its `Refs #N`
//! named. The two artifacts then sorted differently on every board they shared.
//! Neither value is typed by anyone: the component labels are read off the
//! diff and the project and milestone are read off the issue, so both belong
//! in one derivation the command runs rather than in a checklist an agent
//! remembers.
//!
//! What: [`first_refs_issue`] finds the issue the body links, and [`plan`] is
//! the whole decision — a pure function of (that issue's milestone and
//! projects, the changed paths, the workspace's crate ownership) with no `gh`
//! and no filesystem in it. Every gap it finds becomes a note the caller
//! prints, never a failure: a PR that cannot inherit a milestone still opens.
//! [`edit_argv`] renders the single `gh pr edit` that applies the result.
//!
//! Test: the sibling `tests.rs` — `metadata_*`, `open_applies_pr_metadata`,
//! `open_without_refs_says_so`.

use trusty_mpm::core::component_labels::CrateOwnership;
use trusty_mpm::core::issue_audit::IssueFacts;

use super::argv;

/// What the `Refs #N` issue contributes to the PR.
///
/// Why: `plan` must be testable without a `gh`, so the issue arrives as data.
/// What: the issue number, its single milestone, and its project titles.
/// Test: `metadata_inherits_milestone_and_projects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefsIssue {
    /// The issue number the body's first `Refs #N` names.
    pub(crate) number: u64,
    /// That issue's milestone title, or `None` when it carries none.
    pub(crate) milestone: Option<String>,
    /// That issue's project titles, in the order `gh` reported them.
    pub(crate) projects: Vec<String>,
}

impl RefsIssue {
    /// Read the milestone and projects out of a `gh issue view --json` payload.
    ///
    /// Why: `IssueFacts` is already this crate's model of that payload (the
    /// issue audit's), so parsing it a second way here would be a second
    /// implementation of the same read.
    /// What: drops empty titles, which is how `gh` renders an absent milestone
    /// once serde has defaulted the node.
    /// Test: `metadata_parses_a_gh_issue_view_payload`.
    pub(crate) fn from_facts(number: u64, facts: &IssueFacts) -> Self {
        Self {
            number,
            milestone: facts
                .milestone
                .as_ref()
                .map(|m| m.title.trim().to_string())
                .filter(|t| !t.is_empty()),
            projects: facts
                .project_items
                .iter()
                .map(|p| p.title.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
        }
    }
}

/// The labels, milestone and projects a PR earns, plus what it could not get.
///
/// Why: the caller needs both halves — what to apply, and what to say it could
/// not apply — and computing them together is what keeps a silent gap from
/// looking like a satisfied requirement.
/// What: `labels` are component labels only (the shipped `trusty-mpm` and
/// `ws/<session>` are already on the create call); `notes` are one line each,
/// printed verbatim.
/// Test: `metadata_without_refs_applies_nothing`, `metadata_multi_crate_diff`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PrMetadata {
    /// Component labels derived from the changed paths.
    pub(crate) labels: Vec<String>,
    /// The milestone inherited from the `Refs` issue.
    pub(crate) milestone: Option<String>,
    /// The projects inherited from the `Refs` issue.
    pub(crate) projects: Vec<String>,
    /// One line per thing the standard wanted and this PR could not get.
    pub(crate) notes: Vec<String>,
}

impl PrMetadata {
    /// Whether anything at all would be applied.
    pub(crate) fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.milestone.is_none() && self.projects.is_empty()
    }
}

/// Decide a PR's component labels, milestone and projects.
///
/// Why: this is the standard itself, stated once as code. Both inputs are
/// derived — the labels from the diff, the milestone and projects from the
/// linked issue — so there is nothing here for a caller to override, and
/// nothing that needs a network to decide.
/// What: component labels are the crates `changed_paths` touch. A `refs` issue
/// contributes its milestone and its projects; `None` contributes neither and
/// says so. Each absent value adds a note rather than an error — a PR whose
/// diff no crate owns, or whose issue has no milestone, still opens.
/// Test: `metadata_inherits_milestone_and_projects`,
/// `metadata_without_refs_applies_nothing`, `metadata_multi_crate_diff`,
/// `metadata_notes_an_issue_with_no_milestone`.
pub(crate) fn plan<S: AsRef<str>>(
    refs: Option<&RefsIssue>,
    changed_paths: &[S],
    ownership: &CrateOwnership,
) -> PrMetadata {
    let mut out = PrMetadata {
        labels: ownership.labels_for_paths(changed_paths),
        ..PrMetadata::default()
    };
    if out.labels.is_empty() {
        out.notes
            .push("no component label: no workspace crate owns the changed paths".to_string());
    }
    let Some(issue) = refs else {
        out.notes.push(
            "no project or milestone: the body carries no `Refs #N`, so there is no issue \
             to inherit them from"
                .to_string(),
        );
        return out;
    };
    match issue.milestone.clone() {
        Some(m) => out.milestone = Some(m),
        None => out.notes.push(format!(
            "no milestone: issue #{} carries none",
            issue.number
        )),
    }
    if issue.projects.is_empty() {
        out.notes
            .push(format!("no project: issue #{} joins none", issue.number));
    } else {
        out.projects = issue.projects.clone();
    }
    out
}

/// The single `gh pr edit` that applies a [`PrMetadata`].
///
/// Why: one call rather than three keeps the apply atomic from the operator's
/// point of view — either the PR ends up carrying the standard or the one
/// warning names what failed. `gh pr edit` takes `--add-label`, `--milestone`
/// and `--add-project` together (gh 2.98), so the split would buy nothing.
/// What: `gh pr edit <pr> [--repo r] [--add-label l]… [--milestone m]
/// [--add-project p]…`. Callers must not call it on an empty plan.
/// Test: `metadata_edit_argv_carries_every_field`.
pub(crate) fn edit_argv(pr: &str, repo: Option<&str>, meta: &PrMetadata) -> Vec<String> {
    let mut out = argv(&["pr", "edit", pr]);
    if let Some(repo) = repo.map(str::trim).filter(|r| !r.is_empty()) {
        out.push("--repo".to_string());
        out.push(repo.to_string());
    }
    for label in &meta.labels {
        out.push("--add-label".to_string());
        out.push(label.clone());
    }
    if let Some(milestone) = &meta.milestone {
        out.push("--milestone".to_string());
        out.push(milestone.clone());
    }
    for project in &meta.projects {
        out.push("--add-project".to_string());
        out.push(project.clone());
    }
    out
}

/// The issue number of the body's first `Refs #N` line.
///
/// Why: `Refs #N` IS a PR's relationship — the issue side keeps parent/child
/// and blocked-by, and a PR inherits from exactly one issue — so the FIRST
/// reference decides, and a body naming several is not an error.
/// What: scans lines for a leading `Refs`, tolerating an `owner/repo#N`
/// qualifier, and returns the number. Case-insensitive on the keyword; a
/// `Refs` appearing mid-sentence is ignored, since the contract puts the link
/// on its own line.
/// Test: `metadata_finds_the_first_refs`, `metadata_finds_a_qualified_refs`,
/// `metadata_ignores_refs_mid_sentence`.
pub(crate) fn first_refs_issue(body: &str) -> Option<u64> {
    for line in body.lines() {
        let line = line.trim();
        let Some(rest) = line
            .get(..4)
            .filter(|k| k.eq_ignore_ascii_case("refs"))
            .and_then(|_| line.get(4..))
        else {
            continue;
        };
        // `Refsomething #5` is not a link line; the keyword ends the word.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let rest = rest.trim_start();
        let Some((_, number)) = rest.split_once('#') else {
            continue;
        };
        let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            return Some(n);
        }
    }
    None
}
