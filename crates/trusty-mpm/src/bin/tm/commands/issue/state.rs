//! The state-machine view over a validated [`StateModel`] (#1246).
//!
//! Why: the operations (`transition`, `current`, `repair`) need answers to
//! "what state is this issue in?", "is this edge allowed?", and "what assignee
//! rule applies to the target state?". Centralising those derivations here keeps
//! `ops.rs` focused on the `gh` orchestration and keeps the pure logic unit-
//! testable without a runner.
//! What: [`StateMachine`] wrapping a `&StateModel` with `transition_allowed`,
//! `resolve_current_state` (from an issue's labels and open/closed flag),
//! `state_label`, `closes_issue`, `requires_note`, `assignee_target_for`, and
//! `allowed_targets_from`.
//! Test: the `sm_*` tests in this file.

use super::config::{GhState, StateModel};
use crate::commands::ticket::labels::AssigneeTarget;

/// A read-only state-machine view over a validated model.
///
/// Why: borrowing the model (rather than owning a copy) keeps the operations
/// cheap and makes it obvious the machine never mutates config.
/// What: holds `&StateModel`; all methods are pure derivations over it.
/// Test: constructed in every `sm_*` test.
pub(crate) struct StateMachine<'a> {
    model: &'a StateModel,
}

impl<'a> StateMachine<'a> {
    /// Build a machine over a validated model.
    ///
    /// Why: validation is the caller's responsibility (done at load); this is a
    /// zero-cost wrapper.
    /// What: stores the borrow.
    /// Test: `sm_transition_allowed`.
    pub(crate) fn new(model: &'a StateModel) -> Self {
        Self { model }
    }

    /// The GitHub label for a state name, if the state exists AND is labelled.
    ///
    /// Why: the transition swap and seeding need the visible label for a state.
    /// What: linear lookup by state name → `&label.name`. Returns `None` both
    /// for an unknown state and for a label-less one; callers that need to tell
    /// those apart pair it with [`Self::is_state`].
    /// Test: `sm_state_label`, `sm_state_label_none_for_labelless`.
    pub(crate) fn state_label(&self, state: &str) -> Option<&'a str> {
        self.model
            .states
            .iter()
            .find(|s| s.name == state)
            .and_then(|s| s.label.as_ref())
            .map(|l| l.name.as_str())
    }

    /// Whether reaching `state` means the GitHub issue must be closed.
    ///
    /// Why: a lifecycle can end by closing the issue rather than by labelling
    /// it, and `tm issue transition` performs that close.
    /// What: looks the state up and compares its `gh_state` to `Closed`; an
    /// unknown state is not closing.
    /// Test: `sm_closes_issue`.
    pub(crate) fn closes_issue(&self, state: &str) -> bool {
        self.model
            .states
            .iter()
            .any(|s| s.name == state && s.gh_state == GhState::Closed)
    }

    /// Whether the `from → to` edge refuses to run without a `--note`.
    ///
    /// Why: some edges exist to RECORD something (trusty-tools closes an issue
    /// only with live-verification evidence). Making the note a model property
    /// keeps that rule in the YAML instead of in prose.
    /// What: finds the matching edge and returns its `requires_note`; an edge
    /// that is not in the graph never requires a note (the edge check rejects
    /// it first anyway).
    /// Test: `sm_requires_note`.
    pub(crate) fn requires_note(&self, from: Option<&str>, to: &str) -> bool {
        self.model
            .transitions
            .iter()
            .any(|t| t.from.as_deref() == from && t.to == to && t.requires_note)
    }

    /// Whether `state` is a known state name.
    ///
    /// Why: `tm issue transition` must reject an unknown target with the list of
    /// valid states.
    /// What: returns `true` iff a state with that name exists.
    /// Test: `sm_is_state`.
    pub(crate) fn is_state(&self, state: &str) -> bool {
        self.model.states.iter().any(|s| s.name == state)
    }

    /// All state names, in declared order (for error messages / `states` verb).
    ///
    /// Why: a clear "valid states: [...]" hint when a target is unknown.
    /// What: maps the states to their names.
    /// Test: `sm_state_names`.
    pub(crate) fn state_names(&self) -> Vec<&'a str> {
        self.model.states.iter().map(|s| s.name.as_str()).collect()
    }

    /// Whether the `from → to` edge is in the transition graph.
    ///
    /// Why: the core guard — illegal moves are rejected before any `gh` call.
    /// What: returns `true` iff a transition with matching `from`/`to` exists.
    /// `from = None` matches the `null → entry` creation edge.
    /// Test: `sm_transition_allowed`, `sm_transition_rejects_unlisted`.
    pub(crate) fn transition_allowed(&self, from: Option<&str>, to: &str) -> bool {
        self.model
            .transitions
            .iter()
            .any(|t| t.from.as_deref() == from && t.to == to)
    }

    /// The allowed destination states from a given source.
    ///
    /// Why: when a transition is rejected, the error lists what *is* allowed.
    /// What: collects `to` for every transition whose `from` matches.
    /// Test: `sm_allowed_targets_from`.
    pub(crate) fn allowed_targets_from(&self, from: Option<&str>) -> Vec<&'a str> {
        self.model
            .transitions
            .iter()
            .filter(|t| t.from.as_deref() == from)
            .map(|t| t.to.as_str())
            .collect()
    }

    /// Resolve the issue's current state from its labels and open/closed flag.
    ///
    /// Why: the visibility north star requires exactly one state; this
    /// reconstructs it from GitHub artifacts alone. Multiple matches is
    /// surfaced as an error (the caller turns `Many` into a `repair` hint).
    /// What: intersects the issue's labels with the state labels. On zero
    /// matches it falls back to the model's label-less state for the issue's
    /// open/closed flag (trusty-tools' `open`), and only reports `None` when
    /// the model declares no such state.
    /// Test: `sm_resolve_one`, `sm_resolve_none`, `sm_resolve_many`,
    /// `sm_resolve_falls_back_to_labelless`.
    pub(crate) fn resolve_current_state(
        &self,
        issue_labels: &[String],
        gh_open: bool,
    ) -> CurrentState<'a> {
        let matches: Vec<&'a str> = self
            .model
            .states
            .iter()
            .filter(|s| {
                s.label
                    .as_ref()
                    .is_some_and(|lbl| issue_labels.iter().any(|l| l == &lbl.name))
            })
            .map(|s| s.name.as_str())
            .collect();
        match matches.len() {
            0 => self
                .labelless_state(gh_open)
                .map_or(CurrentState::None, CurrentState::One),
            1 => CurrentState::One(matches[0]),
            _ => CurrentState::Many(matches),
        }
    }

    /// The label-less state for an open (or closed) issue, if the model has one.
    ///
    /// Why: `open` and `closed` have no label of their own, so they are named
    /// by the issue's own open/closed flag. Validation guarantees at most one
    /// such state per flag, so the first match is the only match.
    /// What: finds the first state with no label whose `gh_state` matches.
    /// Test: `sm_resolve_falls_back_to_labelless`.
    fn labelless_state(&self, gh_open: bool) -> Option<&'a str> {
        let want = if gh_open {
            GhState::Open
        } else {
            GhState::Closed
        };
        self.model
            .states
            .iter()
            .find(|s| s.label.is_none() && s.gh_state == want)
            .map(|s| s.name.as_str())
    }

    /// The effective assignee rule for a target state.
    ///
    /// Why: `tm issue transition` applies the per-state assignee rule. For the
    /// factory model every per-state rule is `unchanged` (a no-op); other models
    /// may map to `self`/`bot`/`none`.
    /// What: reads `assignee_model.per_state[state].assignees`. `unchanged` (or a
    /// missing rule, or a non-recognised template) yields `None` (no mutation);
    /// `self`/`me` → `SelfUser`; `none`/`unassigned` → `AssigneeTarget::None`;
    /// any other bare string is treated as a literal login.
    ///
    /// Templated values like `{manifest.github.review_assignees}` are NOT
    /// resolvable by `tm issue` (it has no manifest), so they are treated as
    /// `unchanged` (no-op) rather than guessed — the consuming harness owns
    /// creation-time assignment.
    /// Test: `sm_assignee_unchanged`, `sm_assignee_self`, `sm_assignee_none`,
    /// `sm_assignee_template_is_noop`.
    pub(crate) fn assignee_target_for(&self, state: &str) -> Option<AssigneeTarget> {
        let rule = self.model.assignee_model.per_state.get(state)?;
        // The rule is a YAML mapping `{ assignees: <value>, description?: ... }`.
        let value = rule.get("assignees")?;
        let s = value.as_str()?.trim();
        match s {
            "unchanged" => None,
            "self" | "me" | "@me" => Some(AssigneeTarget::SelfUser),
            "none" | "unassigned" | "" => Some(AssigneeTarget::None),
            // A `{template}` we cannot resolve → no-op (harness owns it).
            t if t.starts_with('{') => None,
            // A bare login literal.
            literal => Some(AssigneeTarget::Login(literal.to_string())),
        }
    }
}

/// The outcome of resolving an issue's current state from its labels.
///
/// Why: the three cases drive different behavior (proceed / error / repair-hint).
/// What: `None` (no state label), `One(state)`, `Many(states)`.
/// Test: produced by `resolve_current_state` tests.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CurrentState<'a> {
    /// No state label present on the issue.
    None,
    /// Exactly one state label present (the healthy case).
    One(&'a str),
    /// Multiple state labels present (mid-transition; needs `repair`).
    Many(Vec<&'a str>),
}

#[cfg(test)]
mod tests {
    use super::super::config::DEFAULT_MODEL_YAML;
    use super::*;

    fn model() -> StateModel {
        serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("default parses")
    }

    #[test]
    fn sm_state_label() {
        let m = model();
        let sm = StateMachine::new(&m);
        assert_eq!(sm.state_label("queued"), Some("unicorn:queued"));
        assert_eq!(sm.state_label("nope"), None);
    }

    #[test]
    fn sm_is_state() {
        let m = model();
        let sm = StateMachine::new(&m);
        assert!(sm.is_state("approved"));
        assert!(!sm.is_state("in-review"));
    }

    #[test]
    fn sm_state_names() {
        let m = model();
        let sm = StateMachine::new(&m);
        assert!(sm.state_names().contains(&"failed"));
        assert_eq!(sm.state_names().len(), 7);
    }

    #[test]
    fn sm_transition_allowed() {
        let m = model();
        let sm = StateMachine::new(&m);
        // Creation edge.
        assert!(sm.transition_allowed(None, "queued"));
        // Happy path.
        assert!(sm.transition_allowed(Some("queued"), "approved"));
        assert!(sm.transition_allowed(Some("approved"), "active-development"));
        assert!(sm.transition_allowed(Some("active-development"), "done"));
        assert!(sm.transition_allowed(Some("active-development"), "failed"));
        // Halt edges.
        assert!(sm.transition_allowed(Some("active-development"), "paused"));
        assert!(sm.transition_allowed(Some("active-development"), "blocked"));
    }

    #[test]
    fn sm_transition_rejects_unlisted() {
        let m = model();
        let sm = StateMachine::new(&m);
        // Terminal → anything is rejected.
        assert!(!sm.transition_allowed(Some("done"), "active-development"));
        // Skipping the gate is rejected.
        assert!(!sm.transition_allowed(Some("queued"), "done"));
        // Backwards is rejected.
        assert!(!sm.transition_allowed(Some("approved"), "queued"));
    }

    #[test]
    fn sm_allowed_targets_from() {
        let m = model();
        let sm = StateMachine::new(&m);
        let mut from_active = sm.allowed_targets_from(Some("active-development"));
        from_active.sort_unstable();
        assert_eq!(from_active, vec!["blocked", "done", "failed", "paused"]);
        assert!(sm.allowed_targets_from(Some("done")).is_empty());
    }

    #[test]
    fn sm_resolve_one() {
        let m = model();
        let sm = StateMachine::new(&m);
        let labels = vec!["unicorn".to_string(), "unicorn:approved".to_string()];
        assert_eq!(
            sm.resolve_current_state(&labels, true),
            CurrentState::One("approved")
        );
    }

    #[test]
    fn sm_resolve_none() {
        let m = model();
        let sm = StateMachine::new(&m);
        let labels = vec!["unicorn".to_string(), "blast:high".to_string()];
        // The factory model has no label-less state, so zero matches is `None`.
        assert_eq!(sm.resolve_current_state(&labels, true), CurrentState::None);
    }

    #[test]
    fn sm_resolve_many() {
        let m = model();
        let sm = StateMachine::new(&m);
        let labels = vec!["unicorn:queued".to_string(), "unicorn:approved".to_string()];
        match sm.resolve_current_state(&labels, true) {
            CurrentState::Many(v) => {
                assert!(v.contains(&"queued") && v.contains(&"approved"));
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    /// A two-label-less-state model mirroring trusty-tools' `open`/`closed`.
    fn labelless_model() -> StateModel {
        let yaml = r#"
version: 1
label_config: { base: "", approved: "", blast_prefix: "", status_prefix: "s:" }
states:
  - { name: open, gh_state: open, order: 0 }
  - { name: "s:working", label: { name: "s:working", color: "AABBCC" }, order: 1 }
  - { name: closed, gh_state: closed, order: 2 }
transitions:
  - { from: open, to: "s:working", trigger: executor_start }
  - { from: "s:working", to: closed, trigger: human_label, requires_note: true }
  - { from: "s:working", to: open, trigger: human_label }
assignee_model: { strategy: unchanged, per_state: {} }
"#;
        serde_yaml::from_str(yaml).expect("label-less model parses")
    }

    #[test]
    fn sm_state_label_none_for_labelless() {
        let m = labelless_model();
        let sm = StateMachine::new(&m);
        assert_eq!(sm.state_label("open"), None);
        assert!(sm.is_state("open"), "label-less states are still states");
        assert_eq!(sm.state_label("s:working"), Some("s:working"));
    }

    #[test]
    fn sm_resolve_falls_back_to_labelless() {
        let m = labelless_model();
        let sm = StateMachine::new(&m);
        // No state label + issue open → the label-less `open` state.
        assert_eq!(
            sm.resolve_current_state(&["chore".to_string()], true),
            CurrentState::One("open")
        );
        // No state label + issue closed → the label-less `closed` state.
        assert_eq!(
            sm.resolve_current_state(&["chore".to_string()], false),
            CurrentState::One("closed")
        );
        // A state label still wins over the flag.
        assert_eq!(
            sm.resolve_current_state(&["s:working".to_string()], true),
            CurrentState::One("s:working")
        );
    }

    #[test]
    fn sm_closes_issue() {
        let m = labelless_model();
        let sm = StateMachine::new(&m);
        assert!(sm.closes_issue("closed"));
        assert!(!sm.closes_issue("open"));
        assert!(!sm.closes_issue("s:working"));
        assert!(!sm.closes_issue("nonexistent"));
    }

    #[test]
    fn sm_requires_note() {
        let m = labelless_model();
        let sm = StateMachine::new(&m);
        assert!(sm.requires_note(Some("s:working"), "closed"));
        assert!(!sm.requires_note(Some("s:working"), "open"));
        assert!(!sm.requires_note(Some("open"), "s:working"));
    }

    #[test]
    fn sm_assignee_unchanged() {
        let m = model();
        let sm = StateMachine::new(&m);
        // Every non-initial factory state is `unchanged` → no mutation.
        assert_eq!(sm.assignee_target_for("approved"), None);
        assert_eq!(sm.assignee_target_for("done"), None);
    }

    #[test]
    fn sm_assignee_template_is_noop() {
        let m = model();
        let sm = StateMachine::new(&m);
        // `queued` carries a `{manifest...}` template — tm cannot resolve it,
        // so it is a no-op rather than a guess.
        assert_eq!(sm.assignee_target_for("queued"), None);
    }

    #[test]
    fn sm_assignee_self_and_none() {
        // Build a tiny synthetic model to exercise the non-factory rules.
        let yaml = r#"
version: 1
label_config: { base: x, approved: x:a, blast_prefix: "b:", status_prefix: "x:" }
states:
  - { name: open, label: { name: "x:open", color: "AABBCC" } }
  - { name: closed, label: { name: "x:closed", color: "AABBCC" }, terminal: true }
transitions:
  - { from: null, to: open, trigger: issue_created }
  - { from: open, to: closed, trigger: human_label }
assignee_model:
  strategy: self
  per_state:
    open:
      assignees: self
    closed:
      assignees: none
"#;
        let m: StateModel = serde_yaml::from_str(yaml).expect("synthetic parses");
        let sm = StateMachine::new(&m);
        assert_eq!(
            sm.assignee_target_for("open"),
            Some(AssigneeTarget::SelfUser)
        );
        assert_eq!(sm.assignee_target_for("closed"), Some(AssigneeTarget::None));
    }
}
