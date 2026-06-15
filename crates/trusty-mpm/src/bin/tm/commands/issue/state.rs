//! The state-machine view over a validated [`StateModel`] (#1246).
//!
//! Why: the operations (`transition`, `current`, `repair`) need answers to
//! "what state is this issue in?", "is this edge allowed?", and "what assignee
//! rule applies to the target state?". Centralising those derivations here keeps
//! `ops.rs` focused on the `gh` orchestration and keeps the pure logic unit-
//! testable without a runner.
//! What: [`StateMachine`] wrapping a `&StateModel` with `transition_allowed`,
//! `resolve_current_state` (from an issue's labels), `state_label`,
//! `assignee_target_for`, and `allowed_targets_from`.
//! Test: the `sm_*` tests in this file.

use super::config::StateModel;
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

    /// The GitHub label for a state name, if the state exists.
    ///
    /// Why: the transition swap and seeding need the visible label for a state.
    /// What: linear lookup by state name → `&label.name`.
    /// Test: `sm_state_label`.
    pub(crate) fn state_label(&self, state: &str) -> Option<&'a str> {
        self.model
            .states
            .iter()
            .find(|s| s.name == state)
            .map(|s| s.label.name.as_str())
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

    /// Resolve the issue's current state from the labels present on it.
    ///
    /// Why: the visibility north star requires exactly one state label; this
    /// reconstructs state from GitHub artifacts alone. Zero or multiple matches
    /// is surfaced as an error (the caller turns `Multiple` into a `repair`
    /// hint).
    /// What: intersects the issue's labels with the state labels and classifies
    /// the result as `None` / `One` / `Many`.
    /// Test: `sm_resolve_one`, `sm_resolve_none`, `sm_resolve_many`.
    pub(crate) fn resolve_current_state(&self, issue_labels: &[String]) -> CurrentState<'a> {
        let matches: Vec<&'a str> = self
            .model
            .states
            .iter()
            .filter(|s| issue_labels.iter().any(|l| l == &s.label.name))
            .map(|s| s.name.as_str())
            .collect();
        match matches.len() {
            0 => CurrentState::None,
            1 => CurrentState::One(matches[0]),
            _ => CurrentState::Many(matches),
        }
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
            sm.resolve_current_state(&labels),
            CurrentState::One("approved")
        );
    }

    #[test]
    fn sm_resolve_none() {
        let m = model();
        let sm = StateMachine::new(&m);
        let labels = vec!["unicorn".to_string(), "blast:high".to_string()];
        assert_eq!(sm.resolve_current_state(&labels), CurrentState::None);
    }

    #[test]
    fn sm_resolve_many() {
        let m = model();
        let sm = StateMachine::new(&m);
        let labels = vec!["unicorn:queued".to_string(), "unicorn:approved".to_string()];
        match sm.resolve_current_state(&labels) {
            CurrentState::Many(v) => {
                assert!(v.contains(&"queued") && v.contains(&"approved"));
            }
            other => panic!("expected Many, got {other:?}"),
        }
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
