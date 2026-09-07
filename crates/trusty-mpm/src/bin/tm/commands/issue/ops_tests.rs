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
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

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

/// The tmux session name the `ws/` policy label is derived from in these tests.
const SESSION: &str = "tm-tcode-01";

/// Every `create_label` name the fake recorded, in call order.
fn create_names(sys: &FakeSystem) -> Vec<String> {
    sys.calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateLabel(n) => Some(n),
            _ => None,
        })
        .collect()
}

#[test]
fn ops_seed_creates_only_missing() {
    let m = model();
    // Repo already has the base `unicorn` label and `unicorn:queued`.
    let existing = vec![
        RepoLabel::new("unicorn", "7B68EE", ""),
        RepoLabel::new("unicorn:queued", "BFD4F2", ""),
    ];
    let sys = FakeSystem::new(issue_with_labels(1, &[])).with_repo_labels(existing);
    let report = seed_labels(
        &sys,
        &m,
        &ResolvedTicketing::default(),
        Some(SESSION),
        false,
    )
    .expect("seed");

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
    let created = create_names(&sys);
    assert!(!created.contains(&"unicorn".to_string()));
    assert!(!created.contains(&"unicorn:queued".to_string()));
}

/// #6914: seeding covers the FRAMEWORK's own labels, not just the model's.
///
/// Before the fix, `trusty-mpm` and `ws/<session>` were absent from
/// `seed-labels` entirely — the first `gh issue create --label trusty-mpm` in a
/// fresh repo failed on an unknown label.
#[test]
fn ops_seed_includes_policy_labels() {
    let m = model();
    // Zero existing labels: everything the harness applies must be created.
    let sys = FakeSystem::new(issue_with_labels(1, &[]));
    let report = seed_labels(
        &sys,
        &m,
        &ResolvedTicketing::default(),
        Some(SESSION),
        false,
    )
    .expect("seed");

    let created = create_names(&sys);
    assert!(
        created.contains(&"trusty-mpm".to_string()),
        "the convention label must be seeded; got {created:?}"
    );
    assert!(
        created.contains(&"ws/tm-tcode-01".to_string()),
        "the workstream label must be seeded; got {created:?}"
    );
    assert_eq!(created, report.created, "report must match the calls made");
    assert!(!report.workstream_skipped);

    // The retired pair session launch used to seed must never come back.
    assert!(
        !created.iter().any(|n| n == "in-progress" || n == "blocked"),
        "the retired lifecycle pair must not be seeded; got {created:?}"
    );
}

/// #6918: an `agents.ticketing.extra_labels` entry is seeded by the same call
/// the built-in policy labels are, with no second table.
#[test]
fn ops_seed_includes_configured_extra_labels() {
    let m = model();
    let ticketing = ResolvedTicketing::default().with_extra_labels(vec![RepoLabel::new(
        "area/cli",
        "0E8A16",
        "CLI surface",
    )]);
    let sys = FakeSystem::new(issue_with_labels(1, &[]));
    let report = seed_labels(&sys, &m, &ticketing, Some(SESSION), false).expect("seed");

    let created = create_names(&sys);
    assert!(
        created.contains(&"area/cli".to_string()),
        "a configured label must be seeded; got {created:?}"
    );
    assert!(created.contains(&"trusty-mpm".to_string()));
    assert_eq!(created, report.created);
}

/// #6918: THE regression this slice owes — an absent `agents.ticketing` block
/// must seed byte-for-byte what 6ee2b182b (#6914) seeded.
///
/// A default [`ResolvedTicketing`] is exactly what an absent block resolves to,
/// so comparing its seed output against the built-in policy table proves the
/// config surface changed nothing observable for every project that has no
/// block — which is all of them until someone writes one.
#[test]
fn ops_seed_absent_block_matches_builtin_output() {
    let m = model();
    for session in [None, Some(SESSION)] {
        let sys = FakeSystem::new(issue_with_labels(1, &[]));
        let report = seed_labels(&sys, &m, &ResolvedTicketing::default(), session, false)
            .expect("seed with the default standard");

        // The built-in expectation, assembled without touching the config path.
        let mut expected: Vec<String> = m
            .states
            .iter()
            .filter_map(|s| s.label.as_ref())
            .map(|l| l.name.clone())
            .collect();
        expected.extend(m.extra_labels.iter().map(|l| l.name.clone()));
        for label in trusty_mpm::core::policy_labels::policy_labels(session) {
            if !expected.contains(&label.name) {
                expected.push(label.name);
            }
        }
        assert_eq!(report.created, expected, "session={session:?}");
        assert_eq!(create_names(&sys), expected, "session={session:?}");
    }
}

/// #6914: with no session name there is no `ws/` label — and the report says so.
#[test]
fn ops_seed_without_session_skips_workstream_label() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(1, &[]));
    let report = seed_labels(&sys, &m, &ResolvedTicketing::default(), None, false).expect("seed");

    let created = create_names(&sys);
    assert!(created.contains(&"trusty-mpm".to_string()));
    assert!(
        !created.iter().any(|n| n.starts_with("ws/")),
        "no session name means no ws/ label; got {created:?}"
    );
    assert!(
        report.workstream_skipped,
        "the omission must be reported, not silent"
    );
}

/// #6914: a policy label the repo already carries is left untouched — no
/// re-create, and therefore no colour or description rewrite.
#[test]
fn ops_seed_leaves_present_policy_labels_untouched() {
    let m = model();
    let existing = vec![
        RepoLabel::new("trusty-mpm", "111111", "a project's own wording"),
        RepoLabel::new("ws/tm-tcode-01", "222222", "already styled"),
    ];
    let sys = FakeSystem::new(issue_with_labels(1, &[])).with_repo_labels(existing);
    let report = seed_labels(
        &sys,
        &m,
        &ResolvedTicketing::default(),
        Some(SESSION),
        false,
    )
    .expect("seed");

    let created = create_names(&sys);
    assert!(!created.contains(&"trusty-mpm".to_string()));
    assert!(!created.contains(&"ws/tm-tcode-01".to_string()));
    assert!(report.already_present.contains(&"trusty-mpm".to_string()));
    assert!(
        report
            .already_present
            .contains(&"ws/tm-tcode-01".to_string())
    );
}

#[test]
fn ops_seed_dry_run_creates_nothing() {
    let m = model();
    let sys = FakeSystem::new(issue_with_labels(1, &[]));
    let report = seed_labels(&sys, &m, &ResolvedTicketing::default(), Some(SESSION), true)
        .expect("seed dry");
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
            .filter_map(|s| s.label.as_ref())
            .map(|l| RepoLabel::new(l.name.clone(), l.color.clone(), l.description.clone()))
            .collect();
        v.extend(
            m.extra_labels
                .iter()
                .map(|l| RepoLabel::new(l.name.clone(), l.color.clone(), l.description.clone())),
        );
        // #6914: the policy set is part of what a second run must find present.
        v.extend(trusty_mpm::core::policy_labels::policy_labels(Some(
            SESSION,
        )));
        v
    };
    let sys = FakeSystem::new(issue_with_labels(1, &[])).with_repo_labels(all);
    let report = seed_labels(
        &sys,
        &m,
        &ResolvedTicketing::default(),
        Some(SESSION),
        false,
    )
    .expect("seed");
    assert!(
        report.created.is_empty(),
        "second run creates nothing; got {:?}",
        report.created
    );
}

/// A `CommandRunner` that emulates gh's paging for `gh label list`: it returns
/// the first `--limit` labels of `repo_labels`, defaulting to gh's own 30 when
/// the caller passes no `--limit`, and never says that it truncated.
///
/// Why: #6914 — the seed probe asked for no limit, so on a repo with more than
/// 30 labels it read a partial set, judged every policy label missing, and the
/// run died on the first `gh label create` of a label that already existed.
/// Only a runner that reproduces gh's silent cap can catch that.
struct PagedGhRunner {
    repo_labels: Vec<RepoLabel>,
}

impl crate::commands::ticket::runner::CommandRunner for PagedGhRunner {
    fn run(
        &self,
        _program: &str,
        args: &[&str],
    ) -> anyhow::Result<crate::commands::ticket::runner::CommandOutput> {
        assert_eq!(args.first(), Some(&"label"), "only `gh label` is scripted");
        assert_eq!(args.get(1), Some(&"list"), "only `label list` is scripted");
        let limit: usize = args
            .iter()
            .position(|a| *a == "--limit")
            .and_then(|i| args.get(i + 1))
            .map_or(30, |v| v.parse().expect("--limit must be a number"));
        let page: Vec<serde_json::Value> = self
            .repo_labels
            .iter()
            .take(limit)
            .map(|l| {
                serde_json::json!({
                    "name": l.name,
                    "color": l.color,
                    "description": l.description,
                })
            })
            .collect();
        Ok(crate::commands::ticket::runner::CommandOutput {
            success: true,
            stdout: serde_json::to_string(&page).expect("serialize"),
            stderr: String::new(),
        })
    }
}

/// Why: #6914's live failure — every policy label existed on the repo, yet the
/// run reported all six missing and exited 1 on `gh label create`. This drives
/// the real gh-backed system over a runner that reproduces gh's silent 30-label
/// page, with the desired labels deliberately past that page.
/// What: 40 filler labels ahead of the desired set; a correct probe reads the
/// whole list and creates nothing.
#[test]
fn ops_seed_reads_past_the_default_label_page() {
    let m = model();
    let desired = desired_labels(&m, &ResolvedTicketing::default(), Some(SESSION));
    // Filler ahead of the desired labels, so a 30-label read sees none of them.
    let mut repo_labels: Vec<RepoLabel> = (0..40)
        .map(|i| RepoLabel::new(format!("filler-{i:02}"), "CCCCCC", ""))
        .collect();
    repo_labels.extend(desired.iter().cloned());

    let sys = crate::commands::ticket::system::GhTicketSystem::new(PagedGhRunner { repo_labels });
    let report = seed_labels(&sys, &m, &ResolvedTicketing::default(), Some(SESSION), true)
        .expect("seed reads the whole label set");

    assert!(
        report.created.is_empty(),
        "every desired label already exists; got created {:?}",
        report.created
    );
    assert_eq!(
        report.already_present.len(),
        desired.len(),
        "all {} desired labels must be reported present",
        desired.len()
    );
}

/// Why: a read that comes back exactly as long as the requested page may be
/// truncated, and seeding against a truncated set is the #6914 failure. It is
/// an error, never a silently-accepted "complete" set.
#[test]
fn ops_seed_errors_when_the_label_page_is_full() {
    let m = model();
    let limit = trusty_mpm::core::policy_labels::LABEL_LIST_LIMIT;
    let repo_labels: Vec<RepoLabel> = (0..limit + 5)
        .map(|i| RepoLabel::new(format!("filler-{i:04}"), "CCCCCC", ""))
        .collect();

    let sys = crate::commands::ticket::system::GhTicketSystem::new(PagedGhRunner { repo_labels });
    let err = seed_labels(&sys, &m, &ResolvedTicketing::default(), Some(SESSION), true)
        .expect_err("a full page must not pass as a complete label set");
    assert!(
        err.to_string().contains("truncated"),
        "error must name the truncation; got {err}"
    );
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
