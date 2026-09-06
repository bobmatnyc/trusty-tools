//! The `tm issue` operations: seed-labels, transition, current, repair (#1246).
//!
//! Why: these are the GitHub-mutating verbs that interpret the YAML state model.
//! Each is written generically over the extended [`TicketSystem`] trait so it is
//! fully unit-testable behind a fake backend (no live `gh`), and each maps every
//! action to a concrete label/assignee/comment mutation so issue state stays
//! reconstructable from GitHub artifacts (the visibility north star).
//! What: [`seed_labels`], [`transition`], [`current`], and [`repair`], plus the
//! [`SeedReport`]/[`TransitionReport`] result types the dispatcher prints.
//! Test: the `ops_*` tests in `ops_tests.rs` drive a `FakeSystem`.

use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use crate::commands::ticket::labels::{AssigneeTarget, RepoLabel};
use crate::commands::ticket::system::TicketSystem;

use super::config::StateModel;
use super::state::{CurrentState, StateMachine};

/// Outcome of a `seed-labels` run (for printing + assertion).
///
/// Why: separating "what happened" from "how it's printed" makes the operation
/// pure-ish and lets tests assert the created/present split exactly.
/// What: the labels created and the labels already present, plus whether the run
/// was a dry-run.
/// Test: `ops_seed_creates_only_missing`, `ops_seed_dry_run_creates_nothing`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SeedReport {
    /// Labels that were (or would be) created.
    pub(crate) created: Vec<String>,
    /// Labels already present in the repo (left untouched).
    pub(crate) already_present: Vec<String>,
    /// Whether this was a dry-run (no `create_label` calls made).
    pub(crate) dry_run: bool,
    /// `true` when no session name was known, so the `ws/<session>` policy
    /// label was left out of the seed (#6914). The caller says so in its
    /// output rather than letting the omission pass silently.
    pub(crate) workstream_skipped: bool,
}

/// Outcome of a `transition` (for printing + assertion).
///
/// Why: records the resolved from/to and whether an assignee mutation applied,
/// so the audit comment and tests can reflect exactly what changed.
/// What: the resolved `from` (or `None` for the creation edge), the `to`, and
/// whether an assignee change was applied.
/// Test: `ops_transition_happy_path`, `ops_transition_assignee_unchanged`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TransitionReport {
    /// Resolved source state, or `None` for the `null → entry` creation edge.
    pub(crate) from: Option<String>,
    /// Destination state.
    pub(crate) to: String,
    /// Whether an assignee mutation was applied.
    pub(crate) assignee_changed: bool,
}

/// Every label the harness applies by policy, in seed order.
///
/// Why: #6914 — `gh issue edit --add-label` and `gh issue create --label` fail
/// on a label the repo has never seen, so `seed-labels` has to cover EVERY
/// label the harness applies, not just the model's. Two families make that up:
/// the project-configured lifecycle labels from `issue-state.yaml`, and the
/// framework's own set from `trusty_mpm::core::policy_labels` (`trusty-mpm`,
/// plus `ws/<session>` when a session name is known). One list, so a family cannot
/// be seeded by one caller and forgotten by another.
/// What: the model's state labels and extra labels first (a label-less state
/// like `open`/`closed` has nothing to seed), then the policy set. A name
/// already contributed by the model is not repeated.
///
/// #6918: the policy half now comes from
/// [`trusty_mpm::core::policy_labels::policy_labels_configured`], so a label an
/// operator declared in `agents.ticketing.extra_labels` is seeded by the same
/// call the built-in ones are. A default `ticketing` — what an absent block
/// resolves to — yields the identical set #6914 seeded.
/// Test: `ops_seed_creates_only_missing`,
/// `ops_seed_includes_policy_labels`,
/// `ops_seed_without_session_skips_workstream_label`,
/// `ops_seed_includes_configured_extra_labels`,
/// `ops_seed_absent_block_matches_builtin_output`.
fn desired_labels(
    model: &StateModel,
    ticketing: &ResolvedTicketing,
    session_name: Option<&str>,
) -> Vec<RepoLabel> {
    let mut out: Vec<RepoLabel> = model
        .states
        .iter()
        // A label-less state (`open`, `closed`) has nothing to seed.
        .filter_map(|s| s.label.as_ref())
        .map(|l| RepoLabel::new(l.name.clone(), l.color.clone(), l.description.clone()))
        .collect();
    out.extend(
        model
            .extra_labels
            .iter()
            .map(|l| RepoLabel::new(l.name.clone(), l.color.clone(), l.description.clone())),
    );
    // #6914: the framework's own labels come from the shared policy table that
    // session launch also uses — never a second list spelled out here.
    // #6918: read through the config-aware call so `agents.ticketing` applies.
    for label in trusty_mpm::core::policy_labels::policy_labels_configured(ticketing, session_name)
    {
        if !out.iter().any(|existing| existing.name == label.name) {
            out.push(label);
        }
    }
    out
}

/// `tm issue seed-labels` — idempotent create-missing of every label the
/// harness applies.
///
/// Why: a repo must carry the state, family, and policy labels before any
/// transition or `gh issue create --label` can apply them; create-missing is
/// non-destructive so re-runs are safe, and re-running is the documented
/// recovery when a `--add-label` fails on an unknown label.
/// What: lists existing repo labels, then for each label in
/// [`desired_labels`] not already present, creates it (unless `dry_run`).
/// Existing labels are left untouched, drifted colour/description included
/// (RFC §5.1) — seeding never rewrites what a project already styled. Records
/// `workstream_skipped` when `session_name` is `None`, so the caller can say
/// the `ws/<session>` label was left out. Returns a [`SeedReport`].
/// Test: `ops_seed_creates_only_missing`, `ops_seed_dry_run_creates_nothing`,
/// `ops_seed_idempotent_when_all_present`, `ops_seed_includes_policy_labels`,
/// `ops_seed_without_session_skips_workstream_label`,
/// `ops_seed_leaves_present_policy_labels_untouched`,
/// `ops_seed_includes_configured_extra_labels`,
/// `ops_seed_absent_block_matches_builtin_output`.
pub(crate) fn seed_labels<S: TicketSystem>(
    sys: &S,
    model: &StateModel,
    // #6918: the resolved `agents.ticketing` standard. A default value is the
    // built-in policy table, so an absent block seeds exactly what #6914 did.
    ticketing: &ResolvedTicketing,
    session_name: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<SeedReport> {
    let existing = sys.list_repo_labels()?;
    let existing_names: std::collections::BTreeSet<&str> =
        existing.iter().map(|l| l.name.as_str()).collect();

    let mut report = SeedReport {
        dry_run,
        workstream_skipped: session_name.is_none_or(|n| n.trim().is_empty()),
        ..Default::default()
    };
    for label in desired_labels(model, ticketing, session_name) {
        if existing_names.contains(label.name.as_str()) {
            report.already_present.push(label.name.clone());
            continue;
        }
        if !dry_run {
            sys.create_label(&label)?;
        }
        report.created.push(label.name.clone());
    }
    Ok(report)
}

/// `tm issue transition <issue#> <to-state>` — validated, atomic state change.
///
/// Why: the core verb. It must reject illegal edges BEFORE any `gh` mutation
/// (visibility + safety), perform the swap in one call, apply the per-state
/// assignee rule, and post an audit comment so the change is reconstructable.
/// What: resolves `<to>` to a known state; fetches the issue; resolves the
/// current state from its labels and open/closed flag (erroring clearly on
/// multiple); checks the `from → to` edge; refuses an edge whose
/// `requires_note` is set when no `--note` was given; performs the single-call
/// swap (`swap_labels`), or a plain add/remove when one end is label-less;
/// applies the assignee rule via `set_assignee` (no-op for the factory
/// `unchanged` rules); posts an audit comment with any `note`; and closes the
/// issue when the target state IS GitHub's closed state. Returns a
/// [`TransitionReport`].
/// Test: `ops_transition_happy_path`, `ops_transition_rejects_invalid_terminal`,
/// `ops_transition_rejects_zero_state`, `ops_transition_rejects_multi_state`,
/// plus `project_*` in `project_model_tests.rs`.
pub(crate) fn transition<S: TicketSystem>(
    sys: &S,
    model: &StateModel,
    issue: u64,
    to: &str,
    note: Option<&str>,
) -> anyhow::Result<TransitionReport> {
    let sm = StateMachine::new(model);

    // 1. Target must be a known state.
    if !sm.is_state(to) {
        anyhow::bail!(
            "unknown target state `{to}`; valid states: [{}]",
            sm.state_names().join(", ")
        );
    }

    // 2. Fetch the issue and resolve its current state from labels.
    let issue_obj = sys.validate(issue)?;
    let from: Option<String> = match sm.resolve_current_state(&issue_obj.labels, issue_obj.open) {
        CurrentState::One(s) => Some(s.to_string()),
        CurrentState::None => None,
        CurrentState::Many(states) => {
            anyhow::bail!(
                "issue #{issue} carries multiple state labels {states:?}; \
                 run `tm issue repair {issue}` to resolve to a single state first"
            );
        }
    };

    // 3. Validate the edge BEFORE any gh mutation.
    if !sm.transition_allowed(from.as_deref(), to) {
        let from_disp = from.as_deref().unwrap_or("null");
        let allowed = sm.allowed_targets_from(from.as_deref());
        anyhow::bail!(
            "invalid transition {from_disp} → {to}; allowed from {from_disp}: [{}]",
            allowed.join(", ")
        );
    }

    // 4. The edge requires evidence and none was given → refuse before any
    //    mutation (trusty-tools closes an issue only with live-verification
    //    evidence in the comment).
    let note = note.filter(|n| !n.trim().is_empty());
    if sm.requires_note(from.as_deref(), to) && note.is_none() {
        let from_disp = from.as_deref().unwrap_or("null");
        anyhow::bail!(
            "transition {from_disp} → {to} requires evidence; \
             re-run with `--note \"<what proves it>\"`"
        );
    }

    // 5. The label mutation, always ONE call when both ends are labelled, so an
    //    issue can never be observed carrying two state labels.
    let to_label = sm.state_label(to);
    let from_label = from.as_deref().and_then(|f| sm.state_label(f));
    match (to_label, from_label) {
        (Some(add), Some(remove)) => sys.swap_labels(issue, add, remove)?,
        (Some(add), None) => sys.add_label(issue, add)?,
        // Moving to a label-less state (`open`, `closed`) drops the old label.
        (None, Some(remove)) => sys.remove_label(issue, remove)?,
        (None, None) => {}
    }

    // 6. Apply the per-state assignee rule (no-op for factory `unchanged`).
    let mut assignee_changed = false;
    if let Some(target) = sm.assignee_target_for(to) {
        // The `None` clear-all rule needs the current assignee set (read side).
        let current = if matches!(target, AssigneeTarget::None) {
            issue_obj.assignees.clone()
        } else {
            Vec::new()
        };
        sys.set_assignee(issue, &target, &current)?;
        assignee_changed = true;
    }

    // 7. Audit comment (visibility): record from → to + any note. Posted BEFORE
    //    any close so the evidence lands on an issue that is still open.
    let from_disp = from.as_deref().unwrap_or("(none)");
    let mut body = format!("tm issue transition: `{from_disp}` → `{to}`");
    if assignee_changed {
        body.push_str(" (assignee rule applied)");
    }
    if let Some(n) = note {
        body.push_str("\n\n");
        body.push_str(n);
    }
    sys.comment(issue, &body)?;

    // 8. A state that IS GitHub's closed state closes the issue.
    if sm.closes_issue(to) {
        sys.close_issue(issue)?;
    }

    Ok(TransitionReport {
        from,
        to: to.to_string(),
        assignee_changed,
    })
}

/// `tm issue current <issue#>` — report the issue's current state from labels.
///
/// Why: the read side of the visibility north star — reconstruct state from
/// GitHub artifacts alone.
/// What: fetches the issue, resolves the single state label; returns the state
/// name, or an error for zero/multiple (with a `repair` hint for multiple).
/// Test: `ops_current_reports_state`, `ops_current_errors_on_none`.
pub(crate) fn current<S: TicketSystem>(
    sys: &S,
    model: &StateModel,
    issue: u64,
) -> anyhow::Result<String> {
    let sm = StateMachine::new(model);
    let issue_obj = sys.validate(issue)?;
    match sm.resolve_current_state(&issue_obj.labels, issue_obj.open) {
        CurrentState::One(s) => Ok(s.to_string()),
        CurrentState::None => anyhow::bail!(
            "issue #{issue} carries no recognised state label; valid states: [{}]",
            sm.state_names().join(", ")
        ),
        CurrentState::Many(states) => anyhow::bail!(
            "issue #{issue} carries multiple state labels {states:?}; \
             run `tm issue repair {issue}` to resolve"
        ),
    }
}

/// `tm issue repair <issue#>` — resolve a multi-state issue to a single state.
///
/// Why: a crash mid-transition can leave two state labels on an issue, violating
/// the "exactly one state" invariant. `repair` removes the stale label(s),
/// keeping the most-advanced state (highest `order`) so recovery is deterministic.
/// What: fetches the issue; if exactly one (or zero) state label is present it
/// reports nothing to do (zero is an explicit clear error); if multiple are
/// present it keeps the highest-`order` state and removes the others' labels.
/// Returns the kept state name.
/// Test: `ops_repair_resolves_two_labels`, `ops_repair_noop_when_single`,
/// `ops_repair_errors_on_zero`.
pub(crate) fn repair<S: TicketSystem>(
    sys: &S,
    model: &StateModel,
    issue: u64,
) -> anyhow::Result<String> {
    let sm = StateMachine::new(model);
    let issue_obj = sys.validate(issue)?;
    let present: Vec<&str> = match sm.resolve_current_state(&issue_obj.labels, issue_obj.open) {
        CurrentState::One(s) => {
            // Nothing to repair — already a single, unambiguous state.
            return Ok(s.to_string());
        }
        CurrentState::None => anyhow::bail!(
            "issue #{issue} carries no state label; nothing to repair — \
             apply a state with `tm issue transition` instead"
        ),
        CurrentState::Many(states) => states,
    };

    // Keep the most-advanced state (highest declared `order`; fall back to the
    // first-declared on ties / missing order), remove the rest.
    let keep = present
        .iter()
        .max_by_key(|name| {
            model
                .states
                .iter()
                .find(|s| &s.name == *name)
                .and_then(|s| s.order)
                .unwrap_or(0)
        })
        .copied()
        .ok_or_else(|| anyhow::anyhow!("internal: empty multi-state set"))?;

    for name in present.iter().filter(|n| **n != keep) {
        if let Some(label) = sm.state_label(name) {
            sys.remove_label(issue, label)?;
        }
    }
    sys.comment(
        issue,
        &format!("tm issue repair: resolved multiple state labels to `{keep}`"),
    )?;
    Ok(keep.to_string())
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod ops_tests;
