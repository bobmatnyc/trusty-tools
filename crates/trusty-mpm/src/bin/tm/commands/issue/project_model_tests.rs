//! Tests binding `tm issue` to THIS repo's own lifecycle model.
//!
//! Why: the four `status:*` labels are mutually exclusive, and the only thing
//! that made that true was an agent remembering to pass `--add-label` and
//! `--remove-label` in one `gh issue edit`. These tests pin the repo-root
//! `issue-state.yaml` to the exact edge set CLAUDE.md documents (an extra or a
//! missing edge FAILS), and prove the label swap reaches `gh` as ONE call.
//! What: [`project_model`] loads the real YAML; the `project_*` tests assert the
//! edge set, the single-call swap through a fake `gh` (no network), the
//! two-label refusal, and the evidence-required close.
//! Test: this IS the test module (classified test file: 3000-SLOC cap).

use std::cell::RefCell;

use super::config::StateModel;
use super::ops;
use super::state::{CurrentState, StateMachine};
use crate::commands::ticket::runner::{CommandOutput, CommandRunner};
use crate::commands::ticket::system::GhTicketSystem;

/// Load + validate the repo-root `issue-state.yaml` — the file `tm issue`'s
/// CWD tier reads.
///
/// A deleted, renamed, unparseable, or invalid model fails every test in this
/// file rather than skipping them.
fn project_model() -> StateModel {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("issue-state.yaml");
    let yaml =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let model: StateModel = serde_yaml::from_str(&yaml).expect("repo-root issue-state.yaml parses");
    super::validate::validate_model(&model).expect("repo-root issue-state.yaml is valid");
    model
}

/// The lifecycle CLAUDE.md documents, as `(from, to)` pairs.
///
/// `open` and `closed` are label-less; the middle four are the mutually
/// exclusive `status:*` labels.
const EXPECTED_EDGES: &[(&str, &str)] = &[
    // Forward, one rung at a time.
    ("open", "status:in-progress"),
    ("status:in-progress", "status:coded"),
    ("status:coded", "status:merged"),
    ("status:merged", "status:tested"),
    ("status:tested", "closed"),
    // Release a claim, and reopen from anywhere.
    ("status:in-progress", "open"),
    ("status:coded", "open"),
    ("status:merged", "open"),
    ("status:tested", "open"),
    ("closed", "open"),
];

#[test]
fn project_model_declares_the_documented_states() {
    let m = project_model();
    let names: Vec<&str> = m.states.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "open",
            "status:in-progress",
            "status:coded",
            "status:merged",
            "status:tested",
            "closed",
        ]
    );
    // `open` and `closed` are the absence of a label; the rest carry theirs.
    let sm = StateMachine::new(&m);
    assert_eq!(sm.state_label("open"), None);
    assert_eq!(sm.state_label("closed"), None);
    assert_eq!(sm.state_label("status:coded"), Some("status:coded"));
    assert!(sm.closes_issue("closed"));
    assert!(!sm.closes_issue("open"));
}

#[test]
fn project_model_declares_exactly_the_documented_edges() {
    let m = project_model();
    let mut actual: Vec<(&str, &str)> = m
        .transitions
        .iter()
        .map(|t| (t.from.as_deref().unwrap_or("null"), t.to.as_str()))
        .collect();
    let mut expected: Vec<(&str, &str)> = EXPECTED_EDGES.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    // An EXTRA or a MISSING edge fails here, naming both sides.
    assert_eq!(
        actual, expected,
        "issue-state.yaml's edge set drifted from the lifecycle in CLAUDE.md"
    );
}

#[test]
fn project_model_rejects_skipping_a_rung() {
    let m = project_model();
    let sm = StateMachine::new(&m);
    // Every non-adjacent forward move is absent from the graph.
    assert!(!sm.transition_allowed(Some("open"), "status:coded"));
    assert!(!sm.transition_allowed(Some("status:in-progress"), "status:merged"));
    assert!(!sm.transition_allowed(Some("status:coded"), "status:tested"));
    assert!(!sm.transition_allowed(Some("status:merged"), "closed"));
}

#[test]
fn project_model_resolves_an_unlabelled_open_issue_as_open() {
    let m = project_model();
    let sm = StateMachine::new(&m);
    let labels = vec!["bug".to_string(), "P1".to_string()];
    assert_eq!(
        sm.resolve_current_state(&labels, true),
        CurrentState::One("open")
    );
}

// ---- injected `gh` (no network) --------------------------------------------

/// A scripted [`CommandRunner`] standing in for the `gh` binary.
///
/// Returns queued outputs in order and records every `(program, args)` so a
/// test can assert what reached `gh` — including that a label swap was ONE
/// invocation carrying both flags.
struct FakeGh {
    outputs: RefCell<Vec<CommandOutput>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeGh {
    fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            outputs: RefCell::new(outputs),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for FakeGh {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let mut argv = vec![program.to_string()];
        argv.extend(args.iter().map(|a| (*a).to_string()));
        self.calls.borrow_mut().push(argv);
        let mut queued = self.outputs.borrow_mut();
        if queued.is_empty() {
            return Ok(ok_out(""));
        }
        Ok(queued.remove(0))
    }
}

fn ok_out(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

/// The `gh issue view --json …` payload for an open issue carrying `labels`.
fn issue_json(number: u64, labels: &[&str]) -> String {
    let labels: Vec<String> = labels
        .iter()
        .map(|l| format!("{{\"name\":\"{l}\"}}"))
        .collect();
    format!(
        "{{\"number\":{number},\"title\":\"t\",\"body\":\"\",\"state\":\"OPEN\",\
         \"labels\":[{}],\"assignees\":[]}}",
        labels.join(",")
    )
}

#[test]
fn project_transition_coded_to_merged_is_one_gh_edit_with_both_flags() {
    let m = project_model();
    // 1: `gh issue view`; the rest default to a bare success.
    let gh = FakeGh::new(vec![ok_out(&issue_json(1234, &["status:coded"]))]);
    let sys = GhTicketSystem::new(gh);

    let report = ops::transition(&sys, &m, 1234, "status:merged", None).expect("transition");
    assert_eq!(report.from.as_deref(), Some("status:coded"));
    assert_eq!(report.to, "status:merged");

    let edits: Vec<Vec<String>> = sys
        .runner()
        .calls()
        .into_iter()
        .filter(|c| {
            c.get(1).map(String::as_str) == Some("issue")
                && c.get(2).map(String::as_str) == Some("edit")
        })
        .collect();
    assert_eq!(
        edits.len(),
        1,
        "the swap must be ONE gh issue edit: {edits:?}"
    );
    let edit = &edits[0];
    let pos = |flag: &str, value: &str| edit.windows(2).any(|w| w[0] == flag && w[1] == value);
    assert!(
        pos("--add-label", "status:merged"),
        "missing --add-label status:merged in {edit:?}"
    );
    assert!(
        pos("--remove-label", "status:coded"),
        "missing --remove-label status:coded in {edit:?}"
    );
    // No separate add/remove call could have left two labels visible.
    assert!(
        !sys.runner()
            .calls()
            .iter()
            .any(|c| c.get(2).map(String::as_str) == Some("close")),
        "merging does not close the issue"
    );
}

#[test]
fn project_transition_refuses_two_status_labels_and_names_repair() {
    let m = project_model();
    let gh = FakeGh::new(vec![ok_out(&issue_json(
        1234,
        &["status:coded", "status:merged"],
    ))]);
    let sys = GhTicketSystem::new(gh);

    let err = ops::transition(&sys, &m, 1234, "status:tested", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("multiple state labels"), "got: {err}");
    assert!(
        err.contains("tm issue repair 1234"),
        "must name the repair command, got: {err}"
    );
    // Nothing was mutated: the only gh call is the read.
    let calls = sys.runner().calls();
    assert_eq!(calls.len(), 1, "must not mutate on refusal: {calls:?}");
    assert_eq!(calls[0].get(2).map(String::as_str), Some("view"));
}

#[test]
fn project_repair_resolves_two_status_labels_to_the_later_one() {
    let m = project_model();
    let gh = FakeGh::new(vec![ok_out(&issue_json(
        1234,
        &["status:coded", "status:merged"],
    ))]);
    let sys = GhTicketSystem::new(gh);

    let kept = ops::repair(&sys, &m, 1234).expect("repair");
    assert_eq!(kept, "status:merged", "keeps the more advanced state");
    let removed: Vec<Vec<String>> = sys
        .runner()
        .calls()
        .into_iter()
        .filter(|c| c.iter().any(|a| a == "--remove-label"))
        .collect();
    assert_eq!(removed.len(), 1, "one removal: {removed:?}");
    assert!(
        removed[0].iter().any(|a| a == "status:coded"),
        "must remove the stale label, got {:?}",
        removed[0]
    );
}

#[test]
fn project_close_requires_evidence_and_then_closes() {
    let m = project_model();

    // Without --note the edge is refused before any mutation.
    let gh = FakeGh::new(vec![ok_out(&issue_json(1234, &["status:tested"]))]);
    let sys = GhTicketSystem::new(gh);
    let err = ops::transition(&sys, &m, 1234, "closed", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires evidence"), "got: {err}");
    assert!(err.contains("--note"), "got: {err}");
    assert_eq!(sys.runner().calls().len(), 1, "read only");

    // With --note it drops the label, comments the evidence, then closes.
    let gh = FakeGh::new(vec![ok_out(&issue_json(1234, &["status:tested"]))]);
    let sys = GhTicketSystem::new(gh);
    ops::transition(&sys, &m, 1234, "closed", Some("tm 0.9.1 live run: OK"))
        .expect("transition with evidence");
    let verbs: Vec<String> = sys
        .runner()
        .calls()
        .iter()
        .filter_map(|c| c.get(2).cloned())
        .collect();
    assert_eq!(
        verbs,
        vec!["view", "edit", "comment", "close"],
        "got {verbs:?}"
    );
    let comment = sys
        .runner()
        .calls()
        .into_iter()
        .find(|c| c.get(2).map(String::as_str) == Some("comment"))
        .expect("comment call");
    assert!(
        comment.iter().any(|a| a.contains("tm 0.9.1 live run: OK")),
        "the evidence must reach the issue, got {comment:?}"
    );
}

#[test]
fn project_release_a_claim_removes_the_label_without_closing() {
    let m = project_model();
    let gh = FakeGh::new(vec![ok_out(&issue_json(1234, &["status:in-progress"]))]);
    let sys = GhTicketSystem::new(gh);

    let report = ops::transition(&sys, &m, 1234, "open", Some("claim stale")).expect("release");
    assert_eq!(report.to, "open");
    let verbs: Vec<String> = sys
        .runner()
        .calls()
        .iter()
        .filter_map(|c| c.get(2).cloned())
        .collect();
    assert_eq!(verbs, vec!["view", "edit", "comment"], "got {verbs:?}");
    let edit = sys
        .runner()
        .calls()
        .into_iter()
        .find(|c| c.get(2).map(String::as_str) == Some("edit"))
        .expect("edit call");
    assert!(
        edit.windows(2)
            .any(|w| w[0] == "--remove-label" && w[1] == "status:in-progress"),
        "got {edit:?}"
    );
    assert!(
        !edit.iter().any(|a| a == "--add-label"),
        "`open` has no label to add, got {edit:?}"
    );
}

#[test]
fn project_seed_labels_skips_the_labelless_states() {
    let m = project_model();
    let gh = FakeGh::new(vec![ok_out("[]")]);
    let sys = GhTicketSystem::new(gh);
    let report = ops::seed_labels(&sys, &m, true).expect("seed dry-run");
    assert_eq!(
        report.created,
        vec![
            "status:in-progress".to_string(),
            "status:coded".to_string(),
            "status:merged".to_string(),
            "status:tested".to_string(),
        ],
        "only the four labelled states are seeded"
    );
}
