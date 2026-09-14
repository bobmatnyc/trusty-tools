//! Putting the derived metadata ON the PR, and naming whatever did not land
//! (#7646, #7786, #7869).
//!
//! Why: `metadata::plan` decides what a PR earns; this module is the half that
//! talks to `gh`, and every bug in the B7 batch lived here rather than in the
//! decision. One combined `gh pr edit` meant a single unresolvable value — a
//! milestone closed two releases ago — dropped the component labels with it
//! (#7646). A link line naming a pull request made `gh issue view` 404 and the
//! inherited half was abandoned after one attempt (#7786). And an apply that
//! failed printed `gh`'s raw stderr, which names neither the field nor the
//! value nor the issue it came from.
//!
//! What: [`apply`] reads the link target ([`lookup_ref`], which falls back to
//! `gh pr view`), runs [`metadata::plan`], and applies the result — combined
//! first, then ONE retry field by field so a value that cannot resolve costs
//! only itself. The answer is an [`ApplyOutcome`]: a step that failed is never
//! reported as applied, and what is still missing is named.
//!
//! Test: the sibling `tests.rs` — `pr_7646_*`, `pr_7786_*`, `pr_7869_*`,
//! `open_applies_pr_metadata`, `open_survives_a_failed_metadata_edit`.

use anyhow::Context as _;

use trusty_mpm::core::issue_audit::IssueFacts;
use trusty_mpm::core::issue_audit_gh::view_argv;

use super::metadata::{self, ChangedPaths, PrMetadata, RefKind, RefsIssue, RefsLookup};
use super::open::{self, Preflight};
use super::{GhRunner, argv};
use crate::cli::PrOpenArgs;

/// What applying a PR's metadata actually achieved.
///
/// Why (#7869): `tm pr open` used to exit 0 whatever the apply did, so silent
/// metadata loss was indistinguishable from success. The outcome is a value so
/// the caller can exit non-zero AFTER reporting the PR — the PR exists either
/// way, and a caller that has to hunt for its number is the failure mode this
/// batch started from.
/// What: `Applied` when everything derived is on the PR; `Partial` carrying one
/// description per thing that is not, each already naming its value and the
/// issue it was inherited from.
/// Test: `pr_7646_a_failed_edit_names_the_field_and_retries_per_field`,
/// `pr_7869_a_create_that_fails_after_creating_reports_the_pr_and_retries`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    /// Every derived field is on the PR.
    Applied,
    /// The PR exists; these are the fields that are still missing.
    Partial(Vec<String>),
}

impl ApplyOutcome {
    /// The missing-field descriptions, empty when everything applied.
    pub(crate) fn missing(self) -> Vec<String> {
        match self {
            Self::Applied => Vec::new(),
            Self::Partial(missing) => missing,
        }
    }
}

/// Derive and apply the PR's component labels, milestone and projects.
///
/// Why (#7274): the standard is one rule over issues and PRs, and a PR's share
/// of it is entirely derived — labels from its own diff, project and milestone
/// from the issue its link line names. None of the derivation is worth failing
/// the CREATE over, which is why it runs after the PR exists.
/// What: reads the diff and the link target, calls [`metadata::plan`], then
/// [`apply_plan`]. Prints what applied and one line per thing the standard
/// wanted and this PR could not get.
/// Test: `open_applies_pr_metadata`, `open_without_refs_says_so`,
/// `open_survives_a_failed_metadata_edit`, `open_notes_an_unreadable_diff`,
/// `open_notes_an_unreadable_refs_issue`.
pub(crate) fn apply<R: GhRunner, P: Preflight>(
    gh: &R,
    args: &PrOpenArgs,
    pre: &P,
    pr: &str,
    body: &str,
) -> ApplyOutcome {
    // #7274 round 2: a failed read reaches `plan` as its own state, so the note
    // it prints names the failure rather than blaming an empty answer.
    let read = pre.changed_paths(&args.base, open::diff_head(args));
    let changed = match &read {
        Ok(paths) => ChangedPaths::Read(&paths[..]),
        Err(e) => {
            eprintln!("  warning: the diff could not be read: {e:#}");
            ChangedPaths::Unreadable
        }
    };
    // #7869: `Closes #N` links the issue exactly as `Refs #N` does.
    let number = metadata::first_linked_issue(body);
    let found = number.and_then(|n| lookup_ref(gh, args, n));
    let refs = match (number, found.as_ref()) {
        (_, Some(issue)) => RefsLookup::Found(issue),
        (Some(n), None) => RefsLookup::Unreadable(n),
        (None, None) => RefsLookup::Absent,
    };
    let meta = metadata::plan(refs, changed, &pre.ownership());
    let outcome = apply_plan(gh, args, pr, &meta);
    for note in &meta.notes {
        println!("  {note}");
    }
    outcome
}

/// Read the link target's milestone and projects through `gh`.
///
/// Why (#7786): `--issue N` and a body's link line both take a bare number, and
/// a number naming a PR made `gh issue view` 404 — after which the inherited
/// half was abandoned with no second attempt, so the PR shipped with neither
/// project nor milestone. `view_argv` is the issue audit's own field list, so
/// an issue is read exactly the way `tm issue audit` reads it.
/// What: `gh issue view` first; on the specific `Could not resolve to an Issue`
/// answer, `gh pr view` with [`metadata::PR_VIEW_JSON_FIELDS`]. Any other
/// failure is a warning and `None`, which the caller turns into
/// [`RefsLookup::Unreadable`].
/// Test: `pr_7786_a_pr_ref_inherits_through_gh_pr_view`,
/// `open_notes_an_unreadable_refs_issue`.
fn lookup_ref<R: GhRunner>(gh: &R, args: &PrOpenArgs, number: u64) -> Option<RefsIssue> {
    let mut view = view_argv(number);
    push_repo(&mut view, args);
    match read_facts(gh, &view) {
        Ok(facts) => return Some(RefsIssue::from_facts(number, RefKind::Issue, &facts)),
        Err(e) if is_not_an_issue(&e) => println!(
            "  note: #{number} is a pull request, not an issue; reading its metadata with \
             `gh pr view`"
        ),
        Err(e) => {
            eprintln!("  warning: issue #{number} could not be read: {e:#}");
            return None;
        }
    }
    let mut view = metadata::pr_view_argv(number);
    push_repo(&mut view, args);
    match read_facts(gh, &view) {
        Ok(facts) => Some(RefsIssue::from_facts(number, RefKind::PullRequest, &facts)),
        Err(e) => {
            eprintln!("  warning: pull request #{number} could not be read either: {e:#}");
            None
        }
    }
}

/// Run one `gh … view --json` and parse it as [`IssueFacts`].
fn read_facts<R: GhRunner>(gh: &R, view: &[String]) -> anyhow::Result<IssueFacts> {
    let json = gh.run(view)?.stdout_ok(view)?;
    serde_json::from_str(&json).context("`gh view --json` returned unreadable JSON")
}

/// Is this the GraphQL answer that means "that number is not an issue"?
///
/// Why (#7786): only THAT failure earns a `gh pr view` retry. A network error
/// or a missing `gh` would otherwise be retried against an endpoint that cannot
/// answer it either, turning one honest warning into two.
/// What: the verbatim GraphQL wording, matched case-insensitively.
/// Test: `pr_7786_a_pr_ref_inherits_through_gh_pr_view`,
/// `open_notes_an_unreadable_refs_issue`.
fn is_not_an_issue(err: &anyhow::Error) -> bool {
    format!("{err:#}")
        .to_ascii_lowercase()
        .contains("could not resolve to an issue")
}

/// Apply a [`PrMetadata`], retrying once field by field if the combined edit
/// fails.
///
/// Why (#7646): `gh pr edit` applies labels, milestone and projects in one
/// call, so a milestone whose title `gh` cannot resolve — a closed one, which
/// is exactly what PR #7639 inherited — failed the WHOLE edit and the PR ended
/// up with no component label either. The retry costs one call per field and is
/// bounded at one round, so a genuinely broken `gh` cannot become a loop.
/// What: the combined edit first. On failure, a warning naming every field, its
/// value and the issue it was inherited from, then one edit per field. A field
/// that fails twice is reported missing, never applied (the fail-open rule).
/// Test: `pr_7646_a_failed_edit_names_the_field_and_retries_per_field`,
/// `open_applies_pr_metadata`, `open_survives_a_failed_metadata_edit`.
fn apply_plan<R: GhRunner>(gh: &R, args: &PrOpenArgs, pr: &str, meta: &PrMetadata) -> ApplyOutcome {
    if meta.is_empty() {
        return ApplyOutcome::Applied;
    }
    let edit = metadata::edit_argv(pr, args.repo.as_deref(), meta);
    let reason = match gh.run(&edit) {
        Ok(out) if out.success => {
            report_applied(meta);
            return ApplyOutcome::Applied;
        }
        Ok(out) => out.stderr.trim().to_string(),
        Err(e) => format!("{e:#}"),
    };
    // #7646: the raw `gh` stderr alone names neither the field nor the value.
    eprintln!(
        "  warning: `gh pr edit` failed applying {}: {reason}",
        describe(meta).join(", ")
    );
    eprintln!("  retrying once, one field at a time");
    let mut missing = Vec::new();
    for (what, one) in steps(meta) {
        let a = metadata::edit_argv(pr, args.repo.as_deref(), &one);
        match gh.run(&a) {
            Ok(out) if out.success => println!("  applied on retry: {what}"),
            Ok(out) => {
                eprintln!("  warning: {what} still failed: {}", out.stderr.trim());
                missing.push(what);
            }
            Err(e) => {
                eprintln!("  warning: {what} could not be retried: {e:#}");
                missing.push(what);
            }
        }
    }
    if missing.is_empty() {
        ApplyOutcome::Applied
    } else {
        ApplyOutcome::Partial(missing)
    }
}

/// One human description per field the plan would apply (#7646).
///
/// Why: the failure warning has to say WHICH field carried WHICH value and
/// where the value came from — `milestone "tm 1.3.5" (inherited from #4642)`,
/// not `'tm 1.3.5' not found`.
/// What: labels as one entry, then the milestone, then one entry per project.
/// The inherited ones name their source issue.
/// Test: `pr_7646_a_failed_edit_names_the_field_and_retries_per_field`.
fn describe(meta: &PrMetadata) -> Vec<String> {
    steps(meta).into_iter().map(|(what, _)| what).collect()
}

/// The plan split into one independently appliable step per field.
///
/// Why: the retry must not re-send the field that just failed alongside the
/// ones that did not, which is the whole point of splitting (#7646).
/// What: `(description, single-field plan)` pairs — all labels together (they
/// share one `--add-label` repetition and cannot fail independently), then the
/// milestone, then each project.
/// Test: `pr_7646_a_failed_edit_names_the_field_and_retries_per_field`.
fn steps(meta: &PrMetadata) -> Vec<(String, PrMetadata)> {
    let source = meta
        .inherited_from
        .map(|n| format!(" (inherited from #{n})"))
        .unwrap_or_default();
    let mut out = Vec::new();
    if !meta.labels.is_empty() {
        out.push((
            format!("component labels {}", meta.labels.join(", ")),
            PrMetadata {
                labels: meta.labels.clone(),
                ..PrMetadata::default()
            },
        ));
    }
    if let Some(milestone) = &meta.milestone {
        out.push((
            format!("milestone \"{milestone}\"{source}"),
            PrMetadata {
                milestone: Some(milestone.clone()),
                ..PrMetadata::default()
            },
        ));
    }
    for project in &meta.projects {
        out.push((
            format!("project \"{project}\"{source}"),
            PrMetadata {
                projects: vec![project.clone()],
                ..PrMetadata::default()
            },
        ));
    }
    out
}

/// Print what the edit actually applied.
fn report_applied(meta: &PrMetadata) {
    if !meta.labels.is_empty() {
        println!("  component labels: {}", meta.labels.join(", "));
    }
    if let Some(milestone) = &meta.milestone {
        println!("  milestone: {milestone}");
    }
    if !meta.projects.is_empty() {
        println!("  projects: {}", meta.projects.join(", "));
    }
}

/// Re-apply the assignee and labels `gh pr create` was supposed to set.
///
/// Why (#7869): `gh pr create` creates the PR and THEN applies the assignee and
/// labels over separate API calls. When one of those returns 502 the command
/// exits non-zero having already created the PR — observed on PR #7918, where
/// `tm pr open` printed no number at all and the caller had to find the PR with
/// `gh pr list --head`. The PR exists, so the right answer is to report it and
/// retry the part that failed, exactly once.
/// What: one `gh pr edit <n> --add-assignee <a> --add-label <l>…`. The labels
/// and the assignee come from the same [`open::OpenPlan`] the create used, so
/// the retry cannot apply a different set.
/// Test: `pr_7869_a_create_that_fails_after_creating_reports_the_pr_and_retries`,
/// `pr_7869_a_create_retry_that_also_fails_exits_non_zero`.
pub(crate) fn retry_create_defaults<R: GhRunner>(
    gh: &R,
    args: &PrOpenArgs,
    pr: &str,
    plan: &open::OpenPlan,
) -> ApplyOutcome {
    let mut a = argv(&["pr", "edit", pr]);
    push_repo(&mut a, args);
    a.push("--add-assignee".to_string());
    a.push(plan.assignee.clone());
    for label in plan.create_labels() {
        a.push("--add-label".to_string());
        a.push(label);
    }
    let what = format!(
        "assignee {} and labels {}",
        plan.assignee,
        plan.create_labels().join(", ")
    );
    match gh.run(&a) {
        Ok(out) if out.success => {
            println!("  re-applied after the failed create: {what}");
            ApplyOutcome::Applied
        }
        Ok(out) => {
            eprintln!(
                "  warning: {what} could not be re-applied: {}",
                out.stderr.trim()
            );
            ApplyOutcome::Partial(vec![what])
        }
        Err(e) => {
            eprintln!("  warning: {what} could not be re-applied: {e:#}");
            ApplyOutcome::Partial(vec![what])
        }
    }
}

/// Append `--repo <slug>` when one was passed explicitly.
fn push_repo(a: &mut Vec<String>, args: &PrOpenArgs) {
    if let Some(repo) = args
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        a.push("--repo".to_string());
        a.push(repo.to_string());
    }
}
