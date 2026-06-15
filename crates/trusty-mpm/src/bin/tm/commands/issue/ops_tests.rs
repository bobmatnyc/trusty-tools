//! Unit tests for the `tm issue` operations, driven by a fake [`TicketSystem`].
//!
//! Why: every operation must be verifiable without a live `gh`/GitHub. A
//! scripted [`FakeSystem`] records each backend call so tests assert the exact
//! label/assignee/comment mutations (and, crucially, that invalid transitions
//! error BEFORE any mutation).
//! What: [`FakeSystem`] (a `TicketSystem` impl recording calls + returning a
//! scripted issue) and the `ops_*` tests for seed-labels, transition, current,
//! and repair.
//! Test: this IS the test module (classified test file: 1500-SLOC cap).

use std::cell::RefCell;

use super::*;
use crate::commands::ticket::labels::{AssigneeTarget, RepoLabel};
use crate::commands::ticket::system::{Issue, TicketSystem};

/// A recorded backend call (for assertion).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    ListRepoLabels,
    CreateLabel(String),
    AddLabel(u64, String),
    RemoveLabel(u64, String),
    SwapLabels(u64, String, String),
    SetAssignee(u64, AssigneeTarget, Vec<String>),
    Comment(u64, String),
    Validate(u64),
}

/// A scripted [`TicketSystem`] recording every call.
struct FakeSystem {
    issue: Issue,
    repo_labels: Vec<RepoLabel>,
    calls: RefCell<Vec<Call>>,
}

impl FakeSystem {
    fn new(issue: Issue) -> Self {
        Self {
            issue,
            repo_labels: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn with_repo_labels(mut self, labels: Vec<RepoLabel>) -> Self {
        self.repo_labels = labels;
        self
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
    fn record(&self, c: Call) {
        self.calls.borrow_mut().push(c);
    }
}

impl TicketSystem for FakeSystem {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn validate(&self, issue_number: u64) -> anyhow::Result<Issue> {
        self.record(Call::Validate(issue_number));
        Ok(self.issue.clone())
    }
    fn comment(&self, issue_number: u64, body: &str) -> anyhow::Result<()> {
        self.record(Call::Comment(issue_number, body.to_string()));
        Ok(())
    }
    fn list_repo_labels(&self) -> anyhow::Result<Vec<RepoLabel>> {
        self.record(Call::ListRepoLabels);
        Ok(self.repo_labels.clone())
    }
    fn create_label(&self, label: &RepoLabel) -> anyhow::Result<()> {
        self.record(Call::CreateLabel(label.name.clone()));
        Ok(())
    }
    fn add_label(&self, issue: u64, label: &str) -> anyhow::Result<()> {
        self.record(Call::AddLabel(issue, label.to_string()));
        Ok(())
    }
    fn remove_label(&self, issue: u64, label: &str) -> anyhow::Result<()> {
        self.record(Call::RemoveLabel(issue, label.to_string()));
        Ok(())
    }
    fn swap_labels(&self, issue: u64, add: &str, remove: &str) -> anyhow::Result<()> {
        self.record(Call::SwapLabels(issue, add.to_string(), remove.to_string()));
        Ok(())
    }
    fn set_assignee(
        &self,
        issue: u64,
        who: &AssigneeTarget,
        current: &[String],
    ) -> anyhow::Result<()> {
        self.record(Call::SetAssignee(issue, who.clone(), current.to_vec()));
        Ok(())
    }
}

fn model() -> StateModel {
    serde_yaml::from_str(super::super::config::DEFAULT_MODEL_YAML).expect("default parses")
}

fn issue_with_labels(number: u64, labels: &[&str]) -> Issue {
    Issue {
        number,
        title: "t".to_string(),
        body: String::new(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        assignees: Vec::new(),
        open: true,
    }
}

// ---- seed-labels ----------------------------------------------------------

#[test]
fn ops_seed_creates_only_missing() {
    let m = model();
    // Repo already has the base `unicorn` label and `unicorn:queued`.
    let existing = vec![
        RepoLabel {
            name: "unicorn".to_string(),
            color: "7B68EE".to_string(),
            description: String::new(),
        },
        RepoLabel {
            name: "unicorn:queued".to_string(),
            color: "BFD4F2".to_string(),
            description: String::new(),
        },
    ];
    let sys = FakeSystem::new(issue_with_labels(1, &[])).with_repo_labels(existing);
    let report = seed_labels(&sys, &m, false).expect("seed");

    // Present ones not re-created.
    assert!(report.already_present.contains(&"unicorn".to_string()));
    assert!(
        report
            .already_present
            .contains(&"unicorn:queued".to_string())
    );
    // Missing ones created.
    assert!(report.created.contains(&"unicorn:approved".to_string()));
    assert!(report.created.contains(&"blast:high".to_string()));
    assert!(report.created.contains(&"approval:level-1".to_string()));
    // No create call for the already-present labels.
    let create_names: Vec<String> = sys
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateLabel(n) => Some(n),
            _ => None,
        })
        .collect();
    assert!(!create_names.contains(&"unicorn".to_string()));
    assert!(!create_names.contains(&"unicorn:queued".to_string()));
}

#[test]
fn ops_seed_dry_run_creates_nothing() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(1, &[]));
    let report = seed_labels(&sys, &m, true).expect("seed dry");
    assert!(report.dry_run);
    // Everything reported as created (would-be), but ZERO create calls.
    assert!(!report.created.is_empty());
    let create_calls = sys
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::CreateLabel(_)))
        .count();
    assert_eq!(create_calls, 0, "dry-run must make zero create_label calls");
}

#[test]
fn ops_seed_idempotent_when_all_present() {
    let m = model();
    let all: Vec<RepoLabel> = {
        let mut v: Vec<RepoLabel> = m
            .states
            .iter()
            .map(|s| RepoLabel {
                name: s.label.name.clone(),
                color: s.label.color.clone(),
                description: s.label.description.clone(),
            })
            .collect();
        v.extend(m.extra_labels.iter().map(|l| RepoLabel {
            name: l.name.clone(),
            color: l.color.clone(),
            description: l.description.clone(),
        }));
        v
    };
    let sys = FakeSystem::new(issue_with_labels(1, &[])).with_repo_labels(all);
    let report = seed_labels(&sys, &m, false).expect("seed");
    assert!(report.created.is_empty(), "second run creates nothing");
}

// ---- transition -----------------------------------------------------------

#[test]
fn ops_transition_happy_path() {
    let m = model();
    // Issue is currently `queued`; transition to `approved`.
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn", "unicorn:queued"]));
    let report = transition(&sys, &m, 5, "approved", None).expect("transition");
    assert_eq!(report.from.as_deref(), Some("queued"));
    assert_eq!(report.to, "approved");
    assert!(!report.assignee_changed, "factory rule is unchanged");

    // Exactly one swap call with the right labels.
    let swaps: Vec<Call> = sys
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::SwapLabels(..)))
        .collect();
    assert_eq!(swaps.len(), 1);
    assert_eq!(
        swaps[0],
        Call::SwapLabels(
            5,
            "unicorn:approved".to_string(),
            "unicorn:queued".to_string()
        )
    );
}

#[test]
fn ops_transition_records_single_swap_and_no_assignee() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:approved"]));
    transition(&sys, &m, 5, "active-development", None).expect("transition");
    let calls = sys.calls();
    // No set_assignee call (factory `unchanged`).
    assert!(
        !calls.iter().any(|c| matches!(c, Call::SetAssignee(..))),
        "no assignee mutation for unchanged rule"
    );
    // Exactly one swap.
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::SwapLabels(..)))
            .count(),
        1
    );
}

#[test]
fn ops_transition_creation_edge_adds_label() {
    let m = model();
    // No state label present → from = None; null → queued is allowed.
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn"]));
    let report = transition(&sys, &m, 5, "queued", None).expect("transition");
    assert_eq!(report.from, None);
    // Creation edge uses add_label (not swap).
    let calls = sys.calls();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::AddLabel(5, l) if l == "unicorn:queued"))
    );
    assert!(!calls.iter().any(|c| matches!(c, Call::SwapLabels(..))));
}

#[test]
fn ops_transition_rejects_invalid_terminal() {
    let m = model();
    // done → active-development is not a valid edge (terminal source).
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:done"]));
    let err = transition(&sys, &m, 5, "active-development", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid transition"), "got: {err}");
    // NO mutation recorded (only the read).
    assert!(
        !sys.calls().iter().any(|c| matches!(
            c,
            Call::SwapLabels(..) | Call::AddLabel(..) | Call::RemoveLabel(..)
        )),
        "must not mutate on invalid transition"
    );
}

#[test]
fn ops_transition_rejects_invalid_skip_gate() {
    let m = model();
    // queued → done has no edge.
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:queued"]));
    let err = transition(&sys, &m, 5, "done", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid transition"), "got: {err}");
    assert!(
        !sys.calls()
            .iter()
            .any(|c| matches!(c, Call::SwapLabels(..)))
    );
}

#[test]
fn ops_transition_rejects_unknown_target() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:queued"]));
    let err = transition(&sys, &m, 5, "nonsense", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown target state"), "got: {err}");
    // Rejected before even reading the issue.
    assert!(
        sys.calls().is_empty(),
        "must reject unknown target before any gh call"
    );
}

#[test]
fn ops_transition_rejects_zero_state() {
    let m = model();
    // No state label, but target is not reachable via the creation edge.
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn"]));
    let err = transition(&sys, &m, 5, "approved", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid transition"), "got: {err}");
}

#[test]
fn ops_transition_rejects_multi_state() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(
        5,
        &["unicorn:queued", "unicorn:approved"],
    ));
    let err = transition(&sys, &m, 5, "active-development", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("multiple state labels"), "got: {err}");
    assert!(err.contains("repair"), "should hint repair, got: {err}");
}

#[test]
fn ops_transition_posts_audit_comment_with_note() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:queued"]));
    transition(&sys, &m, 5, "approved", Some("approved by reviewer")).expect("transition");
    let comment = sys.calls().into_iter().find_map(|c| match c {
        Call::Comment(_, b) => Some(b),
        _ => None,
    });
    let body = comment.expect("audit comment posted");
    assert!(body.contains("queued"));
    assert!(body.contains("approved"));
    assert!(body.contains("approved by reviewer"));
}

#[test]
fn ops_transition_applies_assignee_for_self_rule() {
    // Synthetic model whose target state uses `assignees: self`.
    let yaml = r#"
version: 1
label_config: { base: x, approved: x:a, blast_prefix: "b:", status_prefix: "x:" }
states:
  - { name: open, label: { name: "x:open", color: "AABBCC" } }
  - { name: working, label: { name: "x:working", color: "AABBCC" } }
transitions:
  - { from: null, to: open, trigger: issue_created }
  - { from: open, to: working, trigger: executor_start }
assignee_model:
  strategy: self
  per_state:
    working:
      assignees: self
"#;
    let m: StateModel = serde_yaml::from_str(yaml).expect("synthetic parses");
    super::super::validate::validate_model(&m).expect("valid");
    let sys = FakeSystem::new(issue_with_labels(9, &["x:open"]));
    let report = transition(&sys, &m, 9, "working", None).expect("transition");
    assert!(report.assignee_changed);
    assert!(
        sys.calls()
            .iter()
            .any(|c| matches!(c, Call::SetAssignee(9, AssigneeTarget::SelfUser, _)))
    );
}

// ---- current --------------------------------------------------------------

#[test]
fn ops_current_reports_state() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(
        5,
        &["unicorn", "unicorn:active-development"],
    ));
    assert_eq!(current(&sys, &m, 5).expect("current"), "active-development");
}

#[test]
fn ops_current_errors_on_none() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn"]));
    let err = current(&sys, &m, 5).unwrap_err().to_string();
    assert!(err.contains("no recognised state label"), "got: {err}");
}

#[test]
fn ops_current_errors_on_many() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:queued", "unicorn:done"]));
    let err = current(&sys, &m, 5).unwrap_err().to_string();
    assert!(err.contains("multiple state labels"), "got: {err}");
}

// ---- repair ---------------------------------------------------------------

#[test]
fn ops_repair_resolves_two_labels() {
    let m = model();
    // Mid-transition: both queued (order 1) and approved (order 2) present.
    // repair keeps the most-advanced (approved) and removes queued.
    let sys = FakeSystem::new(issue_with_labels(
        5,
        &["unicorn:queued", "unicorn:approved"],
    ));
    let kept = repair(&sys, &m, 5).expect("repair");
    assert_eq!(kept, "approved");
    let removed: Vec<String> = sys
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::RemoveLabel(_, l) => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(removed, vec!["unicorn:queued".to_string()]);
}

#[test]
fn ops_repair_noop_when_single() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn:approved"]));
    let kept = repair(&sys, &m, 5).expect("repair");
    assert_eq!(kept, "approved");
    // No remove calls.
    assert!(
        !sys.calls()
            .iter()
            .any(|c| matches!(c, Call::RemoveLabel(..)))
    );
}

#[test]
fn ops_repair_errors_on_zero() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(5, &["unicorn"]));
    let err = repair(&sys, &m, 5).unwrap_err().to_string();
    assert!(err.contains("no state label"), "got: {err}");
}
