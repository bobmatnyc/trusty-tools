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

/// Collect every label the model declares (state labels + extra labels).
///
/// Why: seeding ensures both families exist; gathering them once keeps the diff
/// simple.
/// What: maps state labels and extra labels into [`RepoLabel`]s.
/// Test: exercised via `ops_seed_creates_only_missing`.
fn declared_labels(model: &StateModel) -> Vec<RepoLabel> {
    let mut out: Vec<RepoLabel> = model
        .states
        .iter()
        .map(|s| RepoLabel {
            name: s.label.name.clone(),
            color: s.label.color.clone(),
            description: s.label.description.clone(),
        })
        .collect();
    out.extend(model.extra_labels.iter().map(|l| RepoLabel {
        name: l.name.clone(),
        color: l.color.clone(),
        description: l.description.clone(),
    }));
    out
}

/// `tm issue seed-labels` — idempotent create-missing of all model labels.
///
/// Why: a repo must have the state + family labels before transitions can apply
/// them; create-missing is non-destructive so re-runs are safe.
/// What: lists existing repo labels, then for each declared label not present
/// creates it (unless `dry_run`); leaves existing labels (incl. drifted
/// color/description) untouched (RFC §5.1). Returns a [`SeedReport`].
/// Test: `ops_seed_creates_only_missing`, `ops_seed_dry_run_creates_nothing`,
/// `ops_seed_idempotent_when_all_present`.
pub(crate) fn seed_labels<S: TicketSystem>(
    sys: &S,
    model: &StateModel,
    dry_run: bool,
) -> anyhow::Result<SeedReport> {
    let existing = sys.list_repo_labels()?;
    let existing_names: std::collections::BTreeSet<&str> =
        existing.iter().map(|l| l.name.as_str()).collect();

    let mut report = SeedReport {
        dry_run,
        ..Default::default()
    };
    for label in declared_labels(model) {
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
/// current state from its labels (erroring clearly on zero/multiple); checks the
/// `from → to` edge; performs the single-call swap (`swap_labels`); applies the
/// assignee rule via `set_assignee` (no-op for the factory `unchanged` rules);
/// and posts an audit comment with any `note`. Returns a [`TransitionReport`].
/// Test: `ops_transition_happy_path`, `ops_transition_rejects_invalid`,
/// `ops_transition_rejects_zero_state`, `ops_transition_rejects_multi_state`,
/// `ops_transition_assignee_unchanged`.
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
    let from: Option<String> = match sm.resolve_current_state(&issue_obj.labels) {
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

    // 4. Atomic label swap (single-call default). The creation edge (from=None)
    //    only adds the entry label.
    let to_label = sm
        .state_label(to)
        .ok_or_else(|| anyhow::anyhow!("internal: state `{to}` has no label"))?;
    match from.as_deref().and_then(|f| sm.state_label(f)) {
        Some(from_label) => sys.swap_labels(issue, to_label, from_label)?,
        None => sys.add_label(issue, to_label)?,
    }

    // 5. Apply the per-state assignee rule (no-op for factory `unchanged`).
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

    // 6. Audit comment (visibility): record from → to + any note.
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
    match sm.resolve_current_state(&issue_obj.labels) {
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
    let present: Vec<&str> = match sm.resolve_current_state(&issue_obj.labels) {
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
