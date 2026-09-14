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
//! What: [`first_linked_issue`] finds the issue the body links, and [`plan`] is
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

/// Whether the linked number turned out to name an issue or a pull request.
///
/// Why (#7786): `tm pr open --issue N` and a body's own link line both take a
/// bare number, and a number that names a PR made `gh issue view` 404 — so the
/// milestone and projects that PR carried were never read. Knowing which of
/// the two was read is what lets the notes name the right artifact instead of
/// calling a pull request an issue.
/// What: the noun the notes and warnings print.
/// Test: `pr_7786_a_pr_ref_inherits_through_gh_pr_view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefKind {
    /// `gh issue view` answered.
    Issue,
    /// `gh issue view` 404'd and `gh pr view` answered.
    PullRequest,
}

impl RefKind {
    /// The noun this kind is called in a note or warning.
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::PullRequest => "pull request",
        }
    }
}

/// What the linked issue (or pull request) contributes to the PR.
///
/// Why: `plan` must be testable without a `gh`, so the issue arrives as data.
/// What: the number, which artifact it turned out to be, its single milestone,
/// and its project titles.
/// Test: `metadata_inherits_milestone_and_projects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefsIssue {
    /// The number the body's first link line names.
    pub(crate) number: u64,
    /// Which artifact that number turned out to name (#7786).
    pub(crate) kind: RefKind,
    /// That artifact's milestone title, or `None` when it carries none.
    pub(crate) milestone: Option<String>,
    /// That artifact's project titles, in the order `gh` reported them.
    pub(crate) projects: Vec<String>,
}

impl RefsIssue {
    /// Read the milestone and projects out of a `gh … view --json` payload.
    ///
    /// Why: `IssueFacts` is already this crate's model of that payload (the
    /// issue audit's), so parsing it a second way here would be a second
    /// implementation of the same read. A `gh pr view` payload restricted to
    /// [`PR_VIEW_JSON_FIELDS`] deserializes into the same type, every other
    /// field defaulting (#7786).
    /// What: drops empty titles, which is how `gh` renders an absent milestone
    /// once serde has defaulted the node.
    /// Test: `metadata_parses_a_gh_issue_view_payload`.
    pub(crate) fn from_facts(number: u64, kind: RefKind, facts: &IssueFacts) -> Self {
        Self {
            number,
            kind,
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

/// What the caller learned about the `Refs #N` issue.
///
/// Why (#7274 round 2): a body with no `Refs` line and a body whose `Refs`
/// issue could not be read both used to arrive as `None`, so [`plan`] printed
/// "the body carries no `Refs #N`" for a body that carried one. The two cases
/// have different causes and different fixes, so they arrive as different
/// values.
/// What: `Absent` when no `Refs` line exists, `Unreadable` when one exists and
/// the `gh issue view` behind it failed, `Found` when the read succeeded.
/// Test: `metadata_notes_an_unreadable_refs_issue`,
/// `open_notes_an_unreadable_refs_issue`.
pub(crate) enum RefsLookup<'a> {
    /// The body names no `Refs #N`.
    Absent,
    /// The body names `Refs #N` and that issue was read.
    Found(&'a RefsIssue),
    /// The body names `Refs #N` and that issue could not be read.
    Unreadable(u64),
}

/// What the caller learned about the diff.
///
/// Why (#7274 round 2): a diff `git` refused to produce and a diff that no
/// crate owns both used to arrive as an empty slice, so [`plan`] blamed the
/// workspace for a `git` failure.
/// What: `Read` carries the paths, however few; `Unreadable` says the read
/// itself failed.
/// Test: `metadata_notes_an_unreadable_diff`, `open_notes_an_unreadable_diff`.
pub(crate) enum ChangedPaths<'a, S: AsRef<str>> {
    /// The diff was read; these are its paths, possibly none.
    Read(&'a [S]),
    /// The diff could not be read at all.
    Unreadable,
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
    /// The issue (or PR) the milestone and projects were inherited from.
    ///
    /// Why (#7646): a `gh pr edit` that fails on an inherited value has to name
    /// where the value came from — a milestone closed two releases ago is only
    /// diagnosable from the warning when the warning says which issue supplied
    /// it. The number is read at derivation time because nothing downstream can
    /// recover it.
    /// Test: `pr_7646_a_failed_edit_names_the_field_and_retries_per_field`.
    pub(crate) inherited_from: Option<u64>,
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
/// What: component labels are the crates the changed paths touch. A `Found`
/// issue contributes its milestone and its projects. Each gap adds a note
/// rather than an error — a PR whose diff no crate owns, or whose issue has no
/// milestone, still opens — and the note names the actual cause, so a failed
/// read never reads as an empty answer (#7274 round 2).
/// Test: `metadata_inherits_milestone_and_projects`,
/// `metadata_without_refs_applies_nothing`, `metadata_multi_crate_diff`,
/// `metadata_notes_an_issue_with_no_milestone`,
/// `metadata_notes_an_unreadable_diff`, `metadata_notes_an_unreadable_refs_issue`.
pub(crate) fn plan<S: AsRef<str>>(
    refs: RefsLookup<'_>,
    changed: ChangedPaths<'_, S>,
    ownership: &CrateOwnership,
) -> PrMetadata {
    let mut out = PrMetadata::default();
    match changed {
        ChangedPaths::Read(paths) => {
            out.labels = ownership.labels_for_paths(paths);
            if out.labels.is_empty() {
                out.notes.push(
                    "no component label: no workspace crate owns the changed paths".to_string(),
                );
            }
        }
        ChangedPaths::Unreadable => out
            .notes
            .push("no component label: the diff could not be read".to_string()),
    }
    let issue = match refs {
        RefsLookup::Found(issue) => issue,
        RefsLookup::Absent => {
            out.notes.push(
                "no project or milestone: the body carries no `Refs #N`, so there is no issue \
                 to inherit them from"
                    .to_string(),
            );
            return out;
        }
        RefsLookup::Unreadable(number) => {
            out.notes.push(format!(
                "no project or milestone: issue #{number} could not be read"
            ));
            return out;
        }
    };
    // #7646: the warning on a failed apply names where the value came from.
    out.inherited_from = Some(issue.number);
    match issue.milestone.clone() {
        Some(m) => out.milestone = Some(m),
        None => out.notes.push(format!(
            "no milestone: {} #{} carries none",
            issue.kind.noun(),
            issue.number
        )),
    }
    if issue.projects.is_empty() {
        out.notes.push(format!(
            "no project: {} #{} joins none",
            issue.kind.noun(),
            issue.number
        ));
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

/// The reference keyword that does NOT auto-close, lowercased.
const REFS_KEYWORD: &str = "refs";

/// Does `word` open a line that links this PR to an issue?
///
/// Why (#7869): the inheritance must not depend on which sanctioned keyword
/// the body used. `Closes #N` (what `--closes` writes) links the issue exactly
/// as `Refs #N` does, and reading only `Refs` opened every `--closes` PR with
/// no project and no milestone. GitHub's own closing keywords come from
/// [`super::body::CLOSING_KEYWORDS`], so the two rules cannot drift apart.
/// What: `refs` or any closing keyword, compared lowercase.
/// Test: `pr_7869_a_closes_link_inherits_the_issues_metadata`,
/// `metadata_finds_the_first_refs`.
fn is_link_keyword(word: &str) -> bool {
    let lowered = word.to_ascii_lowercase();
    lowered == REFS_KEYWORD || super::body::CLOSING_KEYWORDS.contains(&lowered.as_str())
}

/// The issue number of the body's first link line.
///
/// Why: that line IS a PR's relationship — the issue side keeps parent/child
/// and blocked-by, and a PR inherits from exactly one issue — so the FIRST
/// reference decides, and a body naming several is not an error.
/// What: scans lines for a leading link keyword ([`is_link_keyword`]),
/// tolerating an `owner/repo#N` qualifier, and returns the number.
/// Case-insensitive on the keyword; a keyword appearing mid-sentence is
/// ignored, since the contract puts the link on its own line. Lines inside a
/// fenced code block are skipped: a body that quotes the convention in a sample
/// commit message before stating its own link line would otherwise inherit the
/// sample's issue (#7274 round 2).
/// Test: `metadata_finds_the_first_refs`, `metadata_finds_a_qualified_refs`,
/// `metadata_ignores_refs_mid_sentence`, `metadata_ignores_refs_inside_a_fence`,
/// `pr_7869_a_closes_link_inherits_the_issues_metadata`.
pub(crate) fn first_linked_issue(body: &str) -> Option<u64> {
    let mut fenced = false;
    for line in body.lines() {
        let line = line.trim();
        // A fence opens with ``` plus an optional info string and closes with
        // ```; toggling on either spelling is enough to skip the block.
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        // `Refsomething #5` is not a link line: the keyword ends the word, so
        // the split on whitespace is also the word-boundary check.
        let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if !is_link_keyword(keyword) {
            continue;
        }
        let Some((_, number)) = rest.trim_start().split_once('#') else {
            continue;
        };
        let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            return Some(n);
        }
    }
    None
}

/// The `--json` fields a linked PULL REQUEST is read with (#7786).
///
/// Why: `issue_audit::AUDIT_JSON_FIELDS` cannot be reused for a PR —
/// `parent`, `blockedBy` and `subIssues` are issue-only, and `gh pr view`
/// rejects the whole call when asked for one. These three are exactly what
/// [`RefsIssue::from_facts`] reads.
/// Test: `metadata_pr_view_argv_asks_only_for_pr_fields`.
pub(crate) const PR_VIEW_JSON_FIELDS: &str = "number,milestone,projectItems";

/// The `gh pr view` that reads a linked pull request's inheritable metadata.
///
/// Why (#7786): a link line may name a PR rather than an issue, and
/// `gh issue view` answers that with `Could not resolve to an Issue` — so the
/// milestone and projects that PR carried were dropped with no second attempt.
/// What: `pr view <n> --json <PR_VIEW_JSON_FIELDS>`.
/// Test: `metadata_pr_view_argv_asks_only_for_pr_fields`,
/// `pr_7786_a_pr_ref_inherits_through_gh_pr_view`.
pub(crate) fn pr_view_argv(number: u64) -> Vec<String> {
    argv(&[
        "pr",
        "view",
        &number.to_string(),
        "--json",
        PR_VIEW_JSON_FIELDS,
    ])
}
