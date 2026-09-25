//! Unit tests for the `tm issue epic` verb family (#8447).
//!
//! Why: every operation here mutates several live GitHub issues in sequence and
//! can fail partway, so the arms that matter most are the ones a live run would
//! only reach by accident — an interrupted create, a project attach the token
//! refuses, a body that lost a marker. A scripted [`FakeBackend`] that can be
//! told to fail exactly one call is what makes those arms assertable.
//! What: the fake backend and plan fixture, then one section per acceptance
//! criterion of [#8447](https://github.com/bobmatnyc/trusty-tools/issues/8447).
//! Test: itself.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::audit_rows::{self, REQ_PHASE_LINKAGE, REQ_PHASES_BLOCK};
use super::backend::{
    ChildIssue, EpicBackend, FoundTracker, NewIssue, PhaseCandidate, issue_number_from_url,
};
use super::close::{self, CLOSE_COMMENT_PREFIX};
use super::create::{self, CreateOptions, NO_PROJECT_PREFIX};
use super::defer::{self, DeferOptions};
use super::hook;
use super::plan::{self, PlanError};
use super::render::{
    self, DEFERRED_END, DEFERRED_START, FOLLOWUPS_END, FOLLOWUPS_START, PHASES_END, PHASES_START,
};
use super::sync;
use crate::commands::issue::config::{DEFAULT_MODEL_YAML, StateModel};
use crate::commands::ticket::runner::{CommandOutput, CommandRunner};
use crate::commands::ticket::system::{Issue, TicketSystem};
use trusty_mpm::core::issue_audit::Verdict;

/// The repository-relative path the fake backend reports for any plan path.
const REL_PATH: &str = "docs/research/tm-epic-cli/epic-plan.md";
/// This repository's status-label prefix (`issue-state.yaml`); the crate
/// default model's is `unicorn:`, which the hook tests run under (#8448).
const STATUS: &str = "status:";
/// A plausible 40-hex commit for the permalink assertions.
const PUBLISHED_SHA: &str = "581cfb4da254f7448c5c042c8d9bea50ebe84828";
/// gh's handled wording when the token lacks the `project` scope — the ONE
/// attach failure that degrades to a `no-project:` comment.
const SCOPE_REFUSAL: &str = "error: your authentication token is missing required scopes [project]. \
     To add them, run `gh auth refresh -s project`";

/// A two-phase plan document exercising every heading the schema declares,
/// including the document's own `phases` and `deferred` marker blocks.
const PLAN_DOC: &str = r"# Automate tracker and phase-issue authoring

Prose above the section root, which the parser ignores.

## Epic plan

One paragraph of summary that the tracker body carries.

### Outcomes

- **O1** The first outcome.
- **O2** The second outcome.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | The first decision |

### Ordering

Phase 1 must be used against a real epic before phase 2 is written.

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | `first` | TBD | not started | none |
<!-- phases:end -->

### Phase: tm issue epic create|sync

Gate: none — the shape is ratified and on `main`.

What the first phase does.

#### Acceptance criteria

- **AC1** It works.

#### Risk

Multi-issue mutation.

### Phase: defer, close and the transition hook

Gate: phase 1 used live against one real epic.

What the second phase does.

#### Acceptance criteria

- **AC1** It also works.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| A thing | Later | unscheduled |
<!-- deferred:end -->

## Maintenance

Outside the epic-plan region entirely.
";

/// One issue in the fake backend's world.
#[derive(Debug, Clone, Default)]
struct FakeIssue {
    title: String,
    body: String,
    labels: Vec<String>,
    milestone: String,
    parent: Option<u64>,
    state: String,
    comments: Vec<String>,
    projects: Vec<u64>,
}

/// An in-memory GitHub that can be told to fail exactly one call.
///
/// Why: the failure arms are the point. `fail` holds keys of the form
/// `create_issue:2` (the second create fails) or a bare `set_body` (every call
/// fails), so a test can break one link of the chain and assert what the run
/// left behind.
/// Test: used by every orchestration test in this file.
struct FakeBackend {
    repo: String,
    tracked: bool,
    publish: Option<String>,
    local: Option<String>,
    issues: RefCell<BTreeMap<u64, FakeIssue>>,
    next_number: Cell<u64>,
    counts: RefCell<HashMap<String, usize>>,
    fail: RefCell<Vec<(String, String)>>,
    /// The label set the last `find_tracker` call narrowed by (#8447 HIGH).
    find_labels: RefCell<Vec<String>>,
    /// Issues the title SEARCH does not return yet — GitHub's index lagging a
    /// just-created issue (#8448 HIGH 2). Every direct read still sees them.
    hidden_from_search: RefCell<BTreeSet<u64>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            repo: "bobmatnyc/trusty-tools".to_string(),
            tracked: true,
            publish: Some(PUBLISHED_SHA.to_string()),
            local: Some("aaaaaaaabbbbbbbbccccccccddddddddeeeeeeee".to_string()),
            issues: RefCell::new(BTreeMap::new()),
            next_number: Cell::new(100),
            counts: RefCell::new(HashMap::new()),
            fail: RefCell::new(Vec::new()),
            find_labels: RefCell::new(Vec::new()),
            hidden_from_search: RefCell::new(BTreeSet::new()),
        }
    }

    /// Make the search index lag `number`: `phase_titled_issues` omits it while
    /// `children` and `parent` still report it.
    fn hide_from_search(&self, number: u64) {
        self.hidden_from_search.borrow_mut().insert(number);
    }

    /// Every `EpicBackend` call made so far, across all operations.
    fn total_calls(&self) -> usize {
        self.counts.borrow().values().sum()
    }

    /// Script one call to fail with a generic message. `key` is `<op>` or
    /// `<op>:<nth>`.
    fn fails(self, key: &str) -> Self {
        self.fail
            .borrow_mut()
            .push((key.to_string(), String::new()));
        self
    }

    /// Script one call to fail with a SPECIFIC message.
    ///
    /// Why: a generic "scripted failure" cannot distinguish a scope refusal
    /// from a 502, which is the distinction `is_scope_refusal` draws — a test
    /// named for the scope arm that fed the backend a generic error passed over
    /// the broken behaviour it claimed to cover.
    /// Test: `create_waives_a_project_attach_the_token_refuses`,
    /// `create_propagates_a_non_scope_attach_error`.
    fn fails_with(self, key: &str, msg: &str) -> Self {
        self.fail
            .borrow_mut()
            .push((key.to_string(), msg.to_string()));
        self
    }

    fn clear_failures(&self) {
        self.fail.borrow_mut().clear();
    }

    /// Count the call and return the scripted failure, if any.
    fn tick(&self, op: &str) -> anyhow::Result<()> {
        let nth = {
            let mut counts = self.counts.borrow_mut();
            let n = counts.entry(op.to_string()).or_insert(0);
            *n += 1;
            *n
        };
        let fail = self.fail.borrow();
        let hit = fail
            .iter()
            .find(|(k, _)| k == op || k == &format!("{op}:{nth}"));
        if let Some((_, msg)) = hit {
            if msg.is_empty() {
                anyhow::bail!("scripted failure: {op} call {nth}");
            }
            anyhow::bail!("{msg}");
        }
        Ok(())
    }

    fn titles(&self) -> Vec<String> {
        self.issues
            .borrow()
            .values()
            .map(|i| i.title.clone())
            .collect()
    }

    fn issue(&self, number: u64) -> FakeIssue {
        self.issues
            .borrow()
            .get(&number)
            .cloned()
            .unwrap_or_else(|| panic!("no issue #{number} in the fake backend"))
    }

    fn calls(&self, op: &str) -> usize {
        self.counts.borrow().get(op).copied().unwrap_or(0)
    }

    /// Seed a child issue directly, bypassing `create_issue`.
    fn seed_child(&self, tracker: u64, number: u64, title: &str, state: &str, body: &str) {
        self.issues.borrow_mut().insert(
            number,
            FakeIssue {
                title: title.to_string(),
                body: body.to_string(),
                state: state.to_string(),
                parent: Some(tracker),
                ..FakeIssue::default()
            },
        );
    }

    /// Seed a tracker with the given body, bypassing `create_issue`.
    fn seed_tracker(&self, number: u64, title: &str, body: &str) {
        self.issues.borrow_mut().insert(
            number,
            FakeIssue {
                title: title.to_string(),
                body: body.to_string(),
                state: "OPEN".to_string(),
                ..FakeIssue::default()
            },
        );
    }
}

impl EpicBackend for FakeBackend {
    fn repo_slug(&self) -> anyhow::Result<String> {
        self.tick("repo_slug")?;
        Ok(self.repo.clone())
    }

    fn repo_relative_path(&self, _path: &str) -> anyhow::Result<Option<String>> {
        self.tick("repo_relative_path")?;
        Ok(self.tracked.then(|| REL_PATH.to_string()))
    }

    fn publish_sha(&self, _path: &str) -> anyhow::Result<Option<String>> {
        self.tick("publish_sha")?;
        Ok(self.publish.clone())
    }

    fn local_sha(&self, _path: &str) -> anyhow::Result<Option<String>> {
        self.tick("local_sha")?;
        Ok(self.local.clone())
    }

    fn create_issue(&self, spec: &NewIssue) -> anyhow::Result<u64> {
        self.tick("create_issue")?;
        let number = self.next_number.get();
        self.next_number.set(number + 1);
        self.issues.borrow_mut().insert(
            number,
            FakeIssue {
                title: spec.title.clone(),
                body: spec.body.clone(),
                labels: spec.labels.clone(),
                milestone: spec.milestone.clone(),
                parent: spec.parent,
                state: "OPEN".to_string(),
                comments: Vec::new(),
                projects: Vec::new(),
            },
        );
        Ok(number)
    }

    fn set_title(&self, issue: u64, title: &str) -> anyhow::Result<()> {
        self.tick("set_title")?;
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.title = title.to_string();
        }
        Ok(())
    }

    fn body(&self, issue: u64) -> anyhow::Result<String> {
        self.tick("body")?;
        Ok(self.issue(issue).body)
    }

    fn set_body(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        self.tick("set_body")?;
        // The production backend refuses an empty body; the fake must too, or a
        // test could pass against a backend that is safer than the real one.
        if body.trim().is_empty() {
            anyhow::bail!("refusing to write an empty body to #{issue}");
        }
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.body = body.to_string();
        }
        Ok(())
    }

    fn children(&self, tracker: u64) -> anyhow::Result<Vec<ChildIssue>> {
        self.tick("children")?;
        Ok(self
            .issues
            .borrow()
            .iter()
            .filter(|(_, i)| i.parent == Some(tracker))
            .map(|(n, i)| ChildIssue {
                number: *n,
                title: i.title.clone(),
                state: i.state.clone(),
                labels: i.labels.clone(),
                body: i.body.clone(),
            })
            .collect())
    }

    fn find_tracker(
        &self,
        outcome: &str,
        labels: &[String],
    ) -> anyhow::Result<Option<FoundTracker>> {
        self.tick("find_tracker")?;
        self.find_labels.borrow_mut().clone_from(&labels.to_vec());
        let placeholder = render::placeholder_tracker_title(outcome);
        Ok(self.issues.borrow().iter().find_map(|(n, i)| {
            if render::is_tracker_title(&i.title, *n, outcome) {
                Some(FoundTracker {
                    number: *n,
                    placeholder: false,
                })
            } else if i.title.trim() == placeholder {
                Some(FoundTracker {
                    number: *n,
                    placeholder: true,
                })
            } else {
                None
            }
        }))
    }

    fn attach_project(&self, repo: &str, issue: u64, number: u64) -> anyhow::Result<()> {
        self.tick("attach_project")?;
        assert_eq!(repo, self.repo, "the caller resolves the slug once");
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.projects.push(number);
        }
        Ok(())
    }

    fn comments(&self, issue: u64) -> anyhow::Result<Vec<String>> {
        self.tick("comments")?;
        Ok(self.issue(issue).comments)
    }

    fn comment(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        self.tick("comment")?;
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.comments.push(body.to_string());
        }
        Ok(())
    }

    fn parent(&self, issue: u64) -> anyhow::Result<Option<u64>> {
        self.tick("parent")?;
        Ok(self.issue(issue).parent)
    }

    fn close_issue(&self, issue: u64) -> anyhow::Result<()> {
        self.tick("close_issue")?;
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.state = "CLOSED".to_string();
        }
        Ok(())
    }

    fn phase_titled_issues(&self, epic: u64) -> anyhow::Result<Vec<PhaseCandidate>> {
        self.tick("phase_titled_issues")?;
        let hidden = self.hidden_from_search.borrow();
        Ok(self
            .issues
            .borrow()
            .iter()
            .filter(|(n, _)| !hidden.contains(n))
            .filter(|(_, i)| render::epic_number_of(&i.title) == Some(epic))
            .map(|(n, i)| PhaseCandidate {
                number: *n,
                title: i.title.clone(),
            })
            .collect())
    }
}

/// A minimal [`TicketSystem`] for the transition hook tests: one scripted
/// issue, every mutation recorded by name.
///
/// Why: the hook composes `ops::transition` (a `TicketSystem`) with the epic
/// backend, and AC1 needs the label to have MOVED before the sync fails — so
/// the test has to see the swap on this side and the failure on the other.
/// Test: the `transition_hook_*` tests.
struct FakeTickets {
    issue: Issue,
    calls: RefCell<Vec<String>>,
}

impl FakeTickets {
    fn new(number: u64, title: &str, labels: &[&str]) -> Self {
        Self {
            issue: Issue {
                number,
                title: title.to_string(),
                body: String::new(),
                labels: labels.iter().map(|l| (*l).to_string()).collect(),
                assignees: Vec::new(),
                open: true,
            },
            calls: RefCell::new(Vec::new()),
        }
    }

    fn count(&self, op: &str) -> usize {
        self.calls.borrow().iter().filter(|c| *c == op).count()
    }
}

impl TicketSystem for FakeTickets {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn validate(&self, _issue: u64) -> anyhow::Result<Issue> {
        self.calls.borrow_mut().push("validate".to_string());
        Ok(self.issue.clone())
    }
    fn comment(&self, _issue: u64, _body: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().push("comment".to_string());
        Ok(())
    }
    fn add_label(&self, _issue: u64, _label: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().push("add_label".to_string());
        Ok(())
    }
    fn remove_label(&self, _issue: u64, _label: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().push("remove_label".to_string());
        Ok(())
    }
    fn swap_labels(&self, _issue: u64, _add: &str, _remove: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().push("swap_labels".to_string());
        Ok(())
    }
    fn close_issue(&self, _issue: u64) -> anyhow::Result<()> {
        self.calls.borrow_mut().push("close_issue".to_string());
        Ok(())
    }
}

/// Write the plan fixture to a temp dir and build the matching options.
fn opts_for(dir: &tempfile::TempDir, project: Option<u64>) -> CreateOptions {
    let path = dir.path().join("epic-plan.md");
    std::fs::write(&path, PLAN_DOC).expect("fixture write");
    CreateOptions {
        plan_path: path,
        milestone: "Issue management".to_string(),
        components: vec!["trusty-mpm".to_string()],
        phase_type: "enhancement".to_string(),
        project,
        session: "tm-trusty-tools-15".to_string(),
        tracker: None,
        dry_run: false,
        status_prefix: STATUS.to_string(),
    }
}

// ---------------------------------------------------------------- plan parser

#[test]
fn plan_parses_the_committed_epic_plan() {
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("the fixture matches the schema");
    assert_eq!(parsed.outcome, "Automate tracker and phase-issue authoring");
    assert_eq!(parsed.summary.len(), 1, "{:?}", parsed.summary);
    assert_eq!(parsed.outcomes.len(), 2, "{:?}", parsed.outcomes);
    assert!(parsed.decisions.iter().any(|l| l.contains("D1")));
    assert_eq!(parsed.phases.len(), 2);
    assert_eq!(parsed.phases[0].title, "tm issue epic create|sync");
    assert_eq!(
        parsed.phases[0].gate,
        "none — the shape is ratified and on `main`."
    );
    // #### headings are promoted so they read as sections of the issue body.
    assert!(
        parsed.phases[0]
            .body
            .iter()
            .any(|l| l == "## Acceptance criteria"),
        "{:?}",
        parsed.phases[0].body
    );
    assert!(
        !parsed.phases[0].body.iter().any(|l| l.starts_with("Gate:")),
        "the Gate line is lifted out of the body"
    );
    assert!(parsed.deferred.iter().any(|l| l.contains("A thing")));
}

#[test]
fn plan_refuses_a_document_with_no_epic_plan_heading() {
    // AC4, second half: the refusal names the heading that is missing.
    let err = plan::parse(REL_PATH, "# Title\n\nSome prose.\n").unwrap_err();
    assert!(matches!(err, PlanError::MissingHeading { .. }), "{err}");
    assert!(err.to_string().contains("## Epic plan"), "{err}");
}

#[test]
fn plan_refuses_an_empty_ordering_section() {
    let doc = PLAN_DOC.replace(
        "Phase 1 must be used against a real epic before phase 2 is written.",
        "",
    );
    let doc = doc
        .lines()
        .filter(|l| !l.contains("| 1 | `first` |") && !l.contains("|---|-------|"))
        .filter(|l| !l.contains("| # | Phase | Issue | State | Gate |"))
        .collect::<Vec<_>>()
        .join("\n");
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::EmptyOrdering { .. }), "{err}");
    assert!(err.to_string().contains("does not need an epic"), "{err}");
}

#[test]
fn plan_refuses_a_missing_ordering_section() {
    let doc = PLAN_DOC.replace("### Ordering", "### Sequencing");
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(err.to_string().contains("### Ordering"), "{err}");
}

#[test]
fn plan_refuses_a_phase_with_no_gate_line() {
    let doc = PLAN_DOC.replace("Gate: phase 1 used live against one real epic.", "");
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::MissingGate { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("defer, close and the transition hook"),
        "{err}"
    );
}

#[test]
fn plan_refuses_a_phase_with_no_acceptance_criteria() {
    let doc = PLAN_DOC.replacen("#### Acceptance criteria", "#### Criteria", 1);
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::MissingAcceptance { .. }), "{err}");
}

#[test]
fn plan_refuses_a_document_with_no_phases() {
    let doc = PLAN_DOC.replace("### Phase: ", "### Stage: ");
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::NoPhases { .. }), "{err}");
}

#[test]
fn plan_strips_the_documents_own_phases_block() {
    // D5: the document's block is a `TBD` copy, superseded by the tracker's.
    // Copied through, it would put two phases blocks in one tracker body.
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("parses");
    let ordering = parsed.ordering.join("\n");
    assert!(!ordering.contains(PHASES_START), "{ordering}");
    assert!(!ordering.contains("TBD"), "{ordering}");
    assert!(ordering.contains("Phase 1 must be used"), "{ordering}");
}

#[test]
fn plan_drops_the_documents_own_deferred_markers() {
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("parses");
    let deferred = parsed.deferred.join("\n");
    assert!(!deferred.contains(DEFERRED_START), "{deferred}");
    assert!(!deferred.contains(DEFERRED_END), "{deferred}");
    assert!(deferred.contains("A thing"), "{deferred}");
}

/// Critic finding: no H1 yielded `outcome == ""`, a tracker titled
/// `[EPIC 123] ` with a trailing space, and a lookup keyed on nothing.
#[test]
fn plan_refuses_a_document_with_no_h1() {
    let no_h1 = PLAN_DOC.replace("# Automate tracker and phase-issue authoring\n", "");
    let err = plan::parse(REL_PATH, &no_h1).unwrap_err();
    assert!(matches!(err, PlanError::MissingTitle { .. }), "{err}");
    assert!(err.to_string().contains("# <outcome>"), "{err}");
    // A blank H1 is the same defect wearing a heading.
    let blank_h1 = PLAN_DOC.replace("# Automate tracker and phase-issue authoring", "#   ");
    assert!(matches!(
        plan::parse(REL_PATH, &blank_h1),
        Err(PlanError::MissingTitle { .. })
    ));
}

/// Critic finding: two phases with one title filed the first, skipped the
/// second as "already exists", and counted both in `phase <n> of <N>`.
#[test]
fn plan_refuses_two_phases_with_one_title() {
    let doc = PLAN_DOC.replace(
        "### Phase: defer, close and the transition hook",
        "### Phase: tm issue epic create|sync",
    );
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::DuplicatePhase { .. }), "{err}");
    assert!(err.to_string().contains("matched by title"), "{err}");
}

/// Critic finding: a `Gate:` line with nothing after it satisfied "the line
/// exists" and rendered `(no gate declared)` into the tracker's Gate column.
#[test]
fn plan_refuses_a_phase_with_an_empty_gate_line() {
    let doc = PLAN_DOC.replace("Gate: phase 1 used live against one real epic.", "Gate:   ");
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::EmptyGate { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("defer, close and the transition hook"),
        "{err}"
    );
}

/// Critic finding: a plan document DESCRIBING this CLI quotes `### Phase: …`
/// inside a fence, and a fence-blind parser filed it as a real phase.
#[test]
fn plan_ignores_a_heading_inside_a_fenced_block() {
    let fenced = concat!(
        "### Ordering\n\nPhase 1 before phase 2. For example:\n\n",
        "```markdown\n",
        "### Phase: an example nobody should file\n\n",
        "Gate: made up.\n\n",
        "#### Acceptance criteria\n\n- **AC1** invented.\n",
        "```\n"
    );
    let doc = PLAN_DOC.replace(
        "### Ordering\n\nPhase 1 must be used against a real epic before phase 2 is written.\n",
        fenced,
    );
    let parsed = plan::parse(REL_PATH, &doc).expect("the fenced heading is prose");
    assert_eq!(parsed.phases.len(), 2, "{:?}", parsed.phases);
    assert!(
        !parsed
            .phases
            .iter()
            .any(|p| p.title.contains("an example nobody should file")),
        "{:?}",
        parsed.phases
    );
    // The fence survives into the tracker's Ordering prose, verbatim.
    assert!(
        parsed.ordering.iter().any(|l| l.trim() == "```markdown"),
        "{:?}",
        parsed.ordering
    );
}

/// The same guard on the two scans inside a phase: a quoted `Gate:` line
/// cannot satisfy the schema rule the phase itself does not meet.
#[test]
fn plan_ignores_a_gate_line_inside_a_fenced_block() {
    let doc = PLAN_DOC.replace(
        "Gate: phase 1 used live against one real epic.",
        "```\nGate: quoted, not declared.\n```",
    );
    let err = plan::parse(REL_PATH, &doc).unwrap_err();
    assert!(matches!(err, PlanError::MissingGate { .. }), "{err}");
    // And the real gate line is still lifted out of the body it appears in.
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("parses");
    assert!(
        !parsed.phases[0].body.iter().any(|l| l.starts_with("Gate:")),
        "{:?}",
        parsed.phases[0].body
    );
}

// ------------------------------------------------- titles, numbers, rendering

#[test]
fn tracker_title_carries_the_number() {
    assert_eq!(
        render::placeholder_tracker_title("An outcome"),
        "[EPIC] An outcome"
    );
    assert_eq!(
        render::tracker_title(8445, "An outcome"),
        "[EPIC 8445] An outcome"
    );
}

#[test]
fn phase_title_carries_both_numbers() {
    assert_eq!(
        render::phase_title(8445, 2, "do the thing"),
        "[EPIC_8445 PHASE_2] do the thing"
    );
    assert_eq!(
        render::phase_what("[EPIC_8445 PHASE_2] do the thing"),
        "do the thing"
    );
}

#[test]
fn a_tracker_title_is_recognised_by_its_own_number() {
    assert!(render::is_tracker_title(
        "[EPIC 8445] An outcome",
        8445,
        "An outcome"
    ));
    // The embedded number must be the issue's own — an issue that merely
    // mentions the outcome is never adopted as its tracker.
    assert!(!render::is_tracker_title(
        "[EPIC 8445] An outcome",
        8446,
        "An outcome"
    ));
    assert!(!render::is_tracker_title("An outcome", 8445, "An outcome"));
}

/// AC3: numbers are max+1 over existing children and never reused, so a gapped
/// sequence keeps its gap and a deleted number never comes back.
#[test]
fn next_phase_number_never_reuses_a_deleted_number() {
    let gapped = vec![child(1, 101), child(2, 102), child(6, 106)];
    assert_eq!(render::next_phase_number(&gapped), 7);
    // PHASE_6 is deleted and PHASE_7 has been filed: the next is 8, not 6.
    let after = vec![child(1, 101), child(2, 102), child(7, 107)];
    assert_eq!(render::next_phase_number(&after), 8);
    assert_eq!(render::next_phase_number(&[]), 1);
    // A child that is not a phase contributes nothing.
    let stray = vec![ChildIssue {
        number: 200,
        title: "a follow-up".to_string(),
        state: "OPEN".to_string(),
        labels: Vec::new(),
        body: String::new(),
    }];
    assert_eq!(render::next_phase_number(&stray), 1);
}

/// A phase child with the given number, for the numbering tests.
fn child(phase: u64, number: u64) -> ChildIssue {
    ChildIssue {
        number,
        title: render::phase_title(8445, phase, &format!("phase {phase}")),
        state: "OPEN".to_string(),
        labels: Vec::new(),
        body: format!("## Gate\n\ngate {phase}\n"),
    }
}

#[test]
fn render_replaces_the_whole_phases_block() {
    let body = format!("prose\n\n{PHASES_START}\n| stale |\n{PHASES_END}\n\ntail\n");
    let table = render::phases_table(&[child(1, 101), child(2, 102)], STATUS);
    let out = render::replace_block(&body, PHASES_START, PHASES_END, &table).expect("replaces");
    assert!(!out.contains("| stale |"), "{out}");
    assert!(
        out.contains("| 1 | phase 1 | #101 | open | gate 1 |"),
        "{out}"
    );
    assert!(
        out.contains("| 2 | phase 2 | #102 | open | gate 2 |"),
        "{out}"
    );
    assert!(out.starts_with("prose\n"), "{out}");
    assert!(out.ends_with("\ntail\n"), "{out}");
}

#[test]
fn gate_of_reads_only_the_first_paragraph() {
    let body =
        "Part of #1.\n\n## Gate\n\nthe gate line.\n\nthe phase summary.\n\n## Risk\n\nnope\n";
    let rendered = render::phases_table(
        &[ChildIssue {
            number: 7,
            title: render::phase_title(1, 1, "x"),
            state: "CLOSED".to_string(),
            labels: Vec::new(),
            body: body.to_string(),
        }],
        STATUS,
    );
    assert!(rendered.contains("| the gate line. |"), "{rendered}");
    assert!(!rendered.contains("phase summary"), "{rendered}");
    assert!(rendered.contains("| closed |"), "{rendered}");
}

#[test]
fn render_escapes_a_pipe_in_a_phase_title() {
    // The committed plan's own first phase is titled `tm issue epic create|sync`.
    let rendered = render::phases_table(
        &[ChildIssue {
            number: 9,
            title: "[EPIC_1 PHASE_1] tm issue epic create|sync".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            body: "## Gate\n\nnone | really\n".to_string(),
        }],
        STATUS,
    );
    assert!(rendered.contains(r"create\|sync"), "{rendered}");
    assert!(rendered.contains(r"none \| really"), "{rendered}");
    // Exactly six pipes per row: five separators plus the escaped pair.
    let row = rendered.lines().next_back().expect("a row");
    assert_eq!(row.matches(r"\|").count(), 2, "{row}");
}

/// AC6, at the rendering layer: a body with no marker pair is refused.
#[test]
fn render_refuses_a_body_with_no_markers() {
    let err = render::replace_block("just prose\n", PHASES_START, PHASES_END, "x").unwrap_err();
    assert!(err.to_string().contains(PHASES_START), "{err}");
}

/// AC6: two `phases:start` lines are ambiguous, so the rewrite is refused.
#[test]
fn render_refuses_a_body_with_two_start_markers() {
    let body = format!("{PHASES_START}\na\n{PHASES_START}\nb\n{PHASES_END}\n");
    let err = render::replace_block(&body, PHASES_START, PHASES_END, "x").unwrap_err();
    assert!(err.to_string().contains("exactly one is required"), "{err}");
}

#[test]
fn render_refuses_an_inverted_marker_pair() {
    let body = format!("{PHASES_END}\na\n{PHASES_START}\n");
    let err = render::replace_block(&body, PHASES_START, PHASES_END, "x").unwrap_err();
    assert!(err.to_string().contains("precedes"), "{err}");
}

/// The fail-open this whole module exists to not have, stated as what
/// `replace_block` actually guarantees: an empty replacement is ACCEPTED and
/// leaves the two markers adjacent. It never empties the body and never drops a
/// marker, because both marker segments are copied through. The live refusal
/// for an empty body belongs to `EpicBackend::set_body`, proved by
/// `gh_backend_refuses_to_write_an_empty_body`.
#[test]
fn an_empty_replacement_leaves_the_markers_adjacent_and_never_wipes_the_body() {
    let body = format!("prose\n{PHASES_START}\nold\n{PHASES_END}\n");
    let out = render::replace_block(&body, PHASES_START, PHASES_END, "").expect("empty rows");
    assert_eq!(out, format!("prose\n{PHASES_START}\n{PHASES_END}\n"));
    assert!(
        out.contains(PHASES_START) && out.contains(PHASES_END),
        "{out}"
    );
}

/// AC5: the tracker's plan link is a 40-hex commit path, never a branch path.
#[test]
fn tracker_body_links_the_plan_by_sha() {
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("parses");
    let url = render::plan_permalink("bobmatnyc/trusty-tools", PUBLISHED_SHA, REL_PATH);
    let body = render::tracker_body(&parsed, &url);
    let sha_link = regex::Regex::new(r"/blob/[0-9a-f]{40}/").expect("static regex");
    assert!(sha_link.is_match(&body), "{body}");
    assert!(!body.contains("/blob/main/"), "{body}");
    assert!(body.contains(REL_PATH), "{body}");
}

#[test]
fn tracker_body_carries_all_three_marker_blocks() {
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("parses");
    let body = render::tracker_body(&parsed, "https://example.invalid/plan.md");
    for marker in [
        PHASES_START,
        PHASES_END,
        DEFERRED_START,
        DEFERRED_END,
        FOLLOWUPS_START,
        FOLLOWUPS_END,
    ] {
        assert_eq!(
            body.lines().filter(|l| l.trim() == marker).count(),
            1,
            "expected exactly one {marker} in:\n{body}"
        );
    }
    assert!(body.contains("## Outcomes"), "{body}");
    assert!(body.contains("**O1**"), "{body}");
    assert!(body.contains("## Ordering"), "{body}");
}

#[test]
fn phase_body_declares_its_gate_and_its_parent() {
    let parsed = plan::parse(REL_PATH, PLAN_DOC).expect("parses");
    let body = render::phase_body(&parsed.phases[1], 8445, 2, 2);
    assert!(body.starts_with("Part of #8445, phase 2 of 2."), "{body}");
    assert!(body.contains("## Gate\n\nphase 1 used live"), "{body}");
    assert!(body.contains("## Acceptance criteria"), "{body}");
}

// --------------------------------------------------------- `create` behaviour

#[test]
fn create_files_a_tracker_then_its_phases() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    let report = create::create(&backend, &opts_for(&dir, None)).expect("creates");

    let tracker = report.tracker.expect("a tracker");
    assert_eq!(tracker, 100);
    assert_eq!(report.filed, vec![(1, 101), (2, 102)]);
    assert!(report.skipped.is_empty());
    // D1: the tracker is renamed in the call after it is filed, before any
    // phase title is derived from its number.
    assert_eq!(
        backend.issue(tracker).title,
        "[EPIC 100] Automate tracker and phase-issue authoring"
    );
    assert_eq!(
        backend.issue(101).title,
        "[EPIC_100 PHASE_1] tm issue epic create|sync"
    );
    assert_eq!(backend.issue(101).parent, Some(tracker));
    assert_eq!(backend.issue(102).parent, Some(tracker));
    // The run finishes by regenerating the block, so the tracker is complete.
    let body = backend.issue(tracker).body;
    assert!(
        body.contains("| 1 | tm issue epic create\\|sync | #101 |"),
        "{body}"
    );
    assert!(
        body.contains("| 2 | defer, close and the transition hook | #102 |"),
        "{body}"
    );
}

/// AC1, first half: an interrupted run leaves no placeholder-titled issue. The
/// backend fails the SECOND phase create, which is exactly the interruption
/// point the criterion names.
#[test]
fn create_never_leaves_a_placeholder_title_when_a_phase_fails() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new().fails("create_issue:3");
    let err = create::create(&backend, &opts_for(&dir, None)).unwrap_err();
    assert!(err.to_string().contains("scripted failure"), "{err}");

    let titles = backend.titles();
    assert_eq!(titles.len(), 2, "{titles:?}");
    for title in &titles {
        assert!(
            !title.starts_with("[EPIC] "),
            "a placeholder title survived the interruption: {titles:?}"
        );
        assert!(!title.contains("PHASE_<"), "{titles:?}");
    }
    assert!(
        titles.iter().any(|t| t.starts_with("[EPIC 100] ")),
        "{titles:?}"
    );
    assert!(
        titles.iter().any(|t| t.starts_with("[EPIC_100 PHASE_1] ")),
        "{titles:?}"
    );
}

/// AC1, second half: re-running files only the missing phase, with no
/// duplicate tracker and no duplicate child.
#[test]
fn create_skips_a_phase_that_already_exists() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new().fails("create_issue:3");
    let opts = opts_for(&dir, None);
    create::create(&backend, &opts).expect_err("the second phase fails");

    backend.clear_failures();
    let report = create::create(&backend, &opts).expect("the re-run completes");

    assert_eq!(
        report.tracker,
        Some(100),
        "the tracker is adopted, not re-filed"
    );
    assert_eq!(
        report.skipped,
        vec!["tm issue epic create|sync".to_string()]
    );
    assert_eq!(report.filed.len(), 1, "{:?}", report.filed);
    assert_eq!(report.filed[0].0, 2, "max+1 over the one existing child");
    // Three issues total: one tracker, two phases. No duplicates.
    assert_eq!(backend.titles().len(), 3, "{:?}", backend.titles());
}

/// AC4, first half: the refusal names both the local SHA and the remote ref.
#[test]
fn create_refuses_a_plan_doc_absent_from_origin_main() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut backend = FakeBackend::new();
    backend.publish = None;
    let err = create::create(&backend, &opts_for(&dir, None)).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("aaaaaaaabbbbbbbbccccccccddddddddeeeeeeee"),
        "{text}"
    );
    assert!(text.contains("origin/main"), "{text}");
    assert_eq!(backend.calls("create_issue"), 0, "nothing was filed");
}

#[test]
fn create_refuses_a_plan_doc_git_does_not_track() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut backend = FakeBackend::new();
    backend.tracked = false;
    let err = create::create(&backend, &opts_for(&dir, None)).unwrap_err();
    assert!(err.to_string().contains("git does not track"), "{err}");
    assert_eq!(backend.calls("create_issue"), 0);
}

/// AC4, second half, through the verb rather than the parser.
#[test]
fn create_refuses_a_plan_doc_with_no_epic_plan_heading() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let opts = opts_for(&dir, None);
    std::fs::write(&opts.plan_path, "# Title\n\nNo section root here.\n").expect("write");
    let backend = FakeBackend::new();
    let err = create::create(&backend, &opts).unwrap_err();
    assert!(err.to_string().contains("## Epic plan"), "{err}");
    assert_eq!(backend.calls("create_issue"), 0);
}

/// AC7: every issue carries its type label, `ws/<session>`, its component
/// label and the milestone; a phase takes the tracker's milestone.
#[test]
fn create_labels_every_issue_it_files() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    create::create(&backend, &opts_for(&dir, None)).expect("creates");

    let tracker = backend.issue(100);
    assert!(
        tracker.labels.contains(&"epic".to_string()),
        "{:?}",
        tracker.labels
    );
    assert_eq!(tracker.milestone, "Issue management");
    for number in [101, 102] {
        let phase = backend.issue(number);
        assert!(
            phase.labels.contains(&"enhancement".to_string()),
            "#{number}: {:?}",
            phase.labels
        );
        assert!(
            phase.labels.contains(&"trusty-mpm".to_string()),
            "#{number}: {:?}",
            phase.labels
        );
        assert_eq!(
            phase.milestone, tracker.milestone,
            "phases take the tracker's"
        );
    }
    for issue in [tracker.clone(), backend.issue(101), backend.issue(102)] {
        assert!(
            issue.labels.iter().any(|l| l.starts_with("ws/")),
            "{:?}",
            issue.labels
        );
    }
}

/// AC7's live arm: the project attach fails on token scope, the run still
/// exits 0, and every issue carries the `no-project:` waiver on the record.
#[test]
fn create_waives_a_project_attach_the_token_refuses() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // The error text matters: only a SCOPE refusal degrades, so the fake must
    // speak gh's handled wording. A generic "scripted failure" here is what let
    // this test pass over a version that waived on any attach error at all.
    let backend = FakeBackend::new().fails_with("attach_project", SCOPE_REFUSAL);
    let report =
        create::create(&backend, &opts_for(&dir, Some(3))).expect("the run still succeeds");

    assert_eq!(report.waived.len(), 3, "{:?}", report.waived);
    for number in [100, 101, 102] {
        let issue = backend.issue(number);
        assert!(
            issue.projects.is_empty(),
            "#{number} attached a project anyway"
        );
        assert_eq!(issue.comments.len(), 1, "#{number}: {:?}", issue.comments);
        assert!(
            issue.comments[0].starts_with(NO_PROJECT_PREFIX),
            "#{number}: {:?}",
            issue.comments
        );
    }
}

/// The other side of the same decision: an attach failure whose waiver comment
/// ALSO fails leaves nothing on the record, so it reaches the caller.
#[test]
fn create_fails_when_the_waiver_comment_fails() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new()
        .fails_with("attach_project", SCOPE_REFUSAL)
        .fails("comment");
    let err = create::create(&backend, &opts_for(&dir, Some(3))).unwrap_err();
    assert!(
        err.to_string().contains("scripted failure: comment"),
        "{err}"
    );
}

/// Review finding: `attach_project_or_waive` degraded on ANY attach error, so
/// a 502 wrote `no-project: …502…` onto every issue and exited 0 — a permanent
/// record of a transient, retryable outage.
#[test]
fn create_propagates_a_non_scope_attach_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new().fails_with(
        "attach_project",
        "HTTP 502: Bad gateway (https://api.github.com/graphql)",
    );
    let err = create::create(&backend, &opts_for(&dir, Some(3))).unwrap_err();
    assert!(
        err.to_string().contains("not the token's project scope"),
        "{err}"
    );
    assert!(
        backend.issue(100).comments.is_empty(),
        "a retryable failure must leave no waiver: {:?}",
        backend.issue(100).comments
    );
}

/// Both wordings a scope refusal reaches us in: gh's handled form, and the raw
/// GraphQL message it wraps.
#[test]
fn a_scope_refusal_is_recognised_in_both_wordings() {
    assert!(create::is_scope_refusal(SCOPE_REFUSAL));
    assert!(create::is_scope_refusal(
        "your token has not been granted the required scopes to execute this query. The \
         'id' field requires one of the following scopes: ['read:project']"
    ));
    assert!(!create::is_scope_refusal("HTTP 502: Bad gateway"));
    assert!(!create::is_scope_refusal(
        "could not resolve to a ProjectV2"
    ));
}

/// Review finding: the skip branch posted a waiver unconditionally, so a second
/// run with the scope still missing left two `no-project:` comments per issue,
/// a third after the third run.
#[test]
fn create_leaves_one_waiver_per_issue_across_two_runs() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new().fails_with("attach_project", SCOPE_REFUSAL);
    let opts = opts_for(&dir, Some(3));
    create::create(&backend, &opts).expect("first run waives");
    let report = create::create(&backend, &opts).expect("second run waives again");

    // The report still names all three, because all three still lack a project.
    assert_eq!(report.waived.len(), 3, "{:?}", report.waived);
    for number in [100, 101, 102] {
        assert_eq!(
            backend.issue(number).comments.len(),
            1,
            "#{number} collected a second waiver: {:?}",
            backend.issue(number).comments
        );
    }
}

/// Review finding: the tracker attach lived inside the FILING branch, so an
/// adopted tracker — and every `--tracker <n>` run — got neither a project nor
/// a waiver recording why.
#[test]
fn create_attaches_an_adopted_tracker_to_the_project() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // Die at the tracker's own attach, after it is filed and retitled.
    let backend = FakeBackend::new()
        .fails("attach_project:1")
        .fails("comment:1");
    let opts = opts_for(&dir, Some(3));
    create::create(&backend, &opts).expect_err("the run dies at the tracker's attach");
    assert_eq!(
        backend.issue(100).title,
        "[EPIC 100] Automate tracker and phase-issue authoring",
        "the retitle had already happened"
    );
    assert!(backend.issue(100).projects.is_empty());

    backend.clear_failures();
    create::create(&backend, &opts).expect("the re-run adopts the tracker");
    assert_eq!(
        backend.issue(100).projects,
        vec![3],
        "the adopted tracker is attached exactly once"
    );
}

/// Review finding: `create` filed the tracker and attached it in one branch, so
/// the freshly filed path must not attach twice now that the call moved out.
#[test]
fn create_attaches_a_freshly_filed_tracker_exactly_once() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    create::create(&backend, &opts_for(&dir, Some(3))).expect("creates");
    assert_eq!(
        backend.issue(100).projects,
        vec![3],
        "attached once, not twice"
    );
}

/// Review finding: a SIGKILL between `create_issue` and `set_title` leaves
/// `[EPIC] <outcome>`, which the exact-match lookup could not see — so the
/// re-run filed a duplicate tracker. It now adopts and finishes the retitle.
#[test]
fn create_retitles_a_placeholder_tracker_on_re_run() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    // Exactly what the killed process left: created, never renamed. Seeded
    // below the fake's allocator so the phases this run files take fresh
    // numbers rather than landing on the tracker's.
    backend.seed_tracker(
        90,
        "[EPIC] Automate tracker and phase-issue authoring",
        &tracker_fixture(
            "| # | Phase | Issue | State | Gate |\n|---|-------|-------|-------|------|",
        ),
    );
    let report = create::create(&backend, &opts_for(&dir, None)).expect("adopts and finishes");

    assert_eq!(report.tracker, Some(90), "no duplicate tracker was filed");
    assert_eq!(
        backend.issue(90).title,
        "[EPIC 90] Automate tracker and phase-issue authoring",
        "the re-run finished the retitle the killed run owed"
    );
    assert_eq!(report.filed.len(), 2, "{:?}", report.filed);
    assert_eq!(
        backend.titles().len(),
        3,
        "one tracker, two phases: {:?}",
        backend.titles()
    );
}

/// Critic HIGH: a phase skipped as already-existing never re-ran the project
/// attach, so a run that died between `create_issue` and the attach left a
/// child with no project AND no waiver, and no re-run could repair it.
#[test]
fn create_repairs_a_missing_project_waiver_on_a_skipped_phase() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // Die after the first phase is filed but before its attach: the attach is
    // the call right after `create_issue`, so failing `attach_project:2` and
    // `comment` together aborts exactly there.
    let backend = FakeBackend::new()
        .fails("attach_project:2")
        .fails("comment:1");
    let opts = opts_for(&dir, Some(3));
    create::create(&backend, &opts).expect_err("the run dies at the first phase's attach");
    assert!(
        backend.issue(101).projects.is_empty() && backend.issue(101).comments.is_empty(),
        "the interruption left #101 with neither a project nor a waiver"
    );

    backend.clear_failures();
    let report = create::create(&backend, &opts).expect("the re-run completes");
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert_eq!(
        backend.issue(101).projects,
        vec![3],
        "the skip branch repaired the missing attach"
    );
}

/// Critic HIGH: `find_tracker` must enumerate a label-filtered listing, not
/// query the eventually-consistent search index. The label set it narrows by
/// is the tracker's own.
#[test]
fn create_looks_up_the_tracker_by_its_label_set() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    create::create(&backend, &opts_for(&dir, None)).expect("creates");
    let labels = backend.find_labels.borrow().clone();
    assert!(labels.contains(&"epic".to_string()), "{labels:?}");
    assert!(labels.contains(&"trusty-mpm".to_string()), "{labels:?}");
    assert!(labels.iter().any(|l| l.starts_with("ws/")), "{labels:?}");
}

#[test]
fn create_attaches_the_project_when_the_token_allows_it() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    let report = create::create(&backend, &opts_for(&dir, Some(3))).expect("creates");
    assert!(report.waived.is_empty(), "{:?}", report.waived);
    assert_eq!(backend.issue(101).projects, vec![3]);
    assert!(backend.issue(101).comments.is_empty());
}

/// A failed tracker search is not "no tracker exists" — treating it that way
/// files a duplicate tracker, the one mistake this verb cannot undo.
#[test]
fn create_refuses_when_the_tracker_search_fails() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new().fails("find_tracker");
    let err = create::create(&backend, &opts_for(&dir, None)).unwrap_err();
    assert!(err.to_string().contains("--tracker"), "{err}");
    assert_eq!(
        backend.calls("create_issue"),
        0,
        "no duplicate tracker was filed"
    );
}

/// A retitle that fails leaves a placeholder-titled tracker, so the error says
/// so and names the number to resume with.
#[test]
fn create_names_the_tracker_when_the_retitle_fails() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new().fails("set_title");
    let err = create::create(&backend, &opts_for(&dir, None)).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("placeholder title"), "{text}");
    assert!(text.contains("--tracker 100"), "{text}");
}

#[test]
fn create_dry_run_files_nothing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let backend = FakeBackend::new();
    let mut opts = opts_for(&dir, None);
    opts.dry_run = true;
    let report = create::create(&backend, &opts).expect("dry run");
    assert!(report.dry_run);
    assert_eq!(report.filed.len(), 2);
    assert!(
        report.plan_url.contains(PUBLISHED_SHA),
        "{}",
        report.plan_url
    );
    assert_eq!(backend.calls("create_issue"), 0);
    assert_eq!(backend.calls("set_body"), 0);
}

// ----------------------------------------------------------- `sync` behaviour

/// A tracker body carrying authored prose and all three marker blocks — the
/// shape AC2 asks the byte-identity assertion to be made against.
fn tracker_fixture(phases_block: &str) -> String {
    format!(
        "Plan: https://github.com/o/r/blob/{PUBLISHED_SHA}/{REL_PATH}\n\
         \n\
         Authored prose nobody may touch. It has a trailing space here. \n\
         \n\
         ## Phases\n\
         \n\
         {PHASES_START}\n{phases_block}\n{PHASES_END}\n\
         \n\
         ## Deferred\n\
         \n\
         {DEFERRED_START}\n| Item | Why | Where |\n|---|---|---|\n| a thing | later | #1 |\n{DEFERRED_END}\n\
         \n\
         ## Follow-ups\n\
         \n\
         {FOLLOWUPS_START}\n| Finding | Severity | Where |\n|---|---|---|\n{FOLLOWUPS_END}\n"
    )
}

/// AC2: `sync` replaces the WHOLE region, so a hand-edited row inside the
/// block is discarded rather than merged.
#[test]
fn sync_replaces_the_whole_block_and_discards_a_hand_edited_row() {
    let backend = FakeBackend::new();
    let body = tracker_fixture(
        "| # | Phase | Issue | State | Gate |\n\
         |---|-------|-------|-------|------|\n\
         | 1 | phase 1 | #101 | HAND EDITED | who knows |\n\
         | 9 | a phase that does not exist | #999 | open | invented |",
    );
    backend.seed_tracker(100, "[EPIC 100] An outcome", &body);
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "phase 1"),
        "OPEN",
        "## Gate\n\ngate 1\n",
    );

    let report = sync::sync(&backend, 100, STATUS).expect("syncs");
    assert!(!report.unchanged);
    assert_eq!(report.rows, 1);

    let after = backend.issue(100).body;
    assert!(!after.contains("HAND EDITED"), "{after}");
    assert!(!after.contains("a phase that does not exist"), "{after}");
    assert!(
        after.contains("| 1 | phase 1 | #101 | open | gate 1 |"),
        "{after}"
    );
}

/// AC2: every byte outside the two markers is identical before and after.
#[test]
fn sync_leaves_every_byte_outside_the_markers_identical() {
    let backend = FakeBackend::new();
    let before = tracker_fixture(
        "| # | Phase | Issue | State | Gate |\n|---|-------|-------|-------|------|",
    );
    backend.seed_tracker(100, "[EPIC 100] An outcome", &before);
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "phase 1"),
        "CLOSED",
        "## Gate\n\ngate 1\n",
    );

    sync::sync(&backend, 100, STATUS).expect("syncs");
    let after = backend.issue(100).body;
    assert_ne!(after, before, "the block itself must change");

    let split = |text: &str| -> (String, String) {
        let start = text.find(PHASES_START).expect("start marker");
        let end = text.find(PHASES_END).expect("end marker");
        (
            text[..start + PHASES_START.len()].to_string(),
            text[end..].to_string(),
        )
    };
    let (head_before, tail_before) = split(&before);
    let (head_after, tail_after) = split(&after);
    assert_eq!(
        head_before, head_after,
        "bytes before the start marker changed"
    );
    assert_eq!(
        tail_before, tail_after,
        "bytes after the end marker changed"
    );
    assert!(tail_after.contains(DEFERRED_START), "{tail_after}");
    assert!(tail_after.contains(FOLLOWUPS_START), "{tail_after}");
}

/// AC6: no marker pair — nonzero exit, and the body is byte-identical after.
#[test]
fn sync_refuses_a_body_with_no_markers_and_writes_nothing() {
    let backend = FakeBackend::new();
    let before = "Authored prose with no markers at all.\n";
    backend.seed_tracker(100, "[EPIC 100] An outcome", before);
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "OPEN",
        "## Gate\n\ng\n",
    );

    let err = sync::sync(&backend, 100, STATUS).unwrap_err();
    assert!(err.to_string().contains(PHASES_START), "{err}");
    assert_eq!(
        backend.issue(100).body,
        before,
        "the body must be untouched"
    );
    assert_eq!(backend.calls("set_body"), 0);
}

/// AC6: two `phases:start` lines — nonzero exit, body byte-identical after.
#[test]
fn sync_refuses_a_body_with_two_start_markers_and_writes_nothing() {
    let backend = FakeBackend::new();
    let before =
        format!("prose\n{PHASES_START}\n| a |\n{PHASES_START}\n| b |\n{PHASES_END}\nmore prose\n");
    backend.seed_tracker(100, "[EPIC 100] An outcome", &before);
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "OPEN",
        "## Gate\n\ng\n",
    );

    let err = sync::sync(&backend, 100, STATUS).unwrap_err();
    assert!(err.to_string().contains("exactly one is required"), "{err}");
    assert_eq!(
        backend.issue(100).body,
        before,
        "the body must be untouched"
    );
    assert_eq!(backend.calls("set_body"), 0);
}

#[test]
fn sync_is_a_no_op_when_the_block_already_matches() {
    let backend = FakeBackend::new();
    let child_title = render::phase_title(100, 1, "phase 1");
    backend.seed_tracker(
        100,
        "[EPIC 100] An outcome",
        &tracker_fixture(
            "| # | Phase | Issue | State | Gate |\n|---|-------|-------|-------|------|",
        ),
    );
    backend.seed_child(100, 101, &child_title, "OPEN", "## Gate\n\ngate 1\n");

    sync::sync(&backend, 100, STATUS).expect("first sync writes");
    assert_eq!(backend.calls("set_body"), 1);
    let report = sync::sync(&backend, 100, STATUS).expect("second sync is a no-op");
    assert!(report.unchanged, "a matching block must not be rewritten");
    assert_eq!(backend.calls("set_body"), 1, "no second write");
}

/// The error arm: a body write that fails reaches the caller rather than being
/// downgraded to a warning that leaves a stale tracker behind.
#[test]
fn sync_propagates_a_failed_body_write() {
    let backend = FakeBackend::new().fails("set_body");
    backend.seed_tracker(
        100,
        "[EPIC 100] An outcome",
        &tracker_fixture(
            "| # | Phase | Issue | State | Gate |\n|---|-------|-------|-------|------|",
        ),
    );
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "OPEN",
        "## Gate\n\ng\n",
    );
    let err = sync::sync(&backend, 100, STATUS).unwrap_err();
    assert!(
        err.to_string().contains("scripted failure: set_body"),
        "{err}"
    );
}

/// A child body that cannot be read would render a blank Gate column, so the
/// whole sync refuses rather than writing a table with a hole in it.
#[test]
fn sync_propagates_a_failed_child_read() {
    let backend = FakeBackend::new().fails("children");
    backend.seed_tracker(
        100,
        "[EPIC 100] An outcome",
        &tracker_fixture(
            "| # | Phase | Issue | State | Gate |\n|---|-------|-------|-------|------|",
        ),
    );
    let err = sync::sync(&backend, 100, STATUS).unwrap_err();
    assert!(
        err.to_string().contains("scripted failure: children"),
        "{err}"
    );
    assert_eq!(backend.calls("set_body"), 0);
}

// ------------------------------------------------------- the `gh`/`git` layer

/// A scripted [`CommandRunner`] returning queued outputs in order.
struct FakeRunner {
    outputs: RefCell<Vec<CommandOutput>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeRunner {
    fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            outputs: RefCell::new(outputs),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let mut call = vec![program.to_string()];
        call.extend(args.iter().map(|a| (*a).to_string()));
        self.calls.borrow_mut().push(call);
        let mut outs = self.outputs.borrow_mut();
        if outs.is_empty() {
            anyhow::bail!("FakeRunner exhausted")
        }
        Ok(outs.remove(0))
    }
}

fn ok_out(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

#[test]
fn gh_backend_creates_an_issue_and_reads_its_number() {
    let runner = FakeRunner::new(vec![ok_out(
        "https://github.com/bobmatnyc/trusty-tools/issues/8451\n",
    )]);
    let spec = NewIssue {
        title: "[EPIC_8445 PHASE_1] do it".to_string(),
        body: "Part of #8445.".to_string(),
        labels: vec!["enhancement".to_string(), "ws/x".to_string()],
        milestone: "Issue management".to_string(),
        parent: Some(8445),
    };
    let backend = super::backend::GhEpicBackend::new(runner);
    assert_eq!(backend.create_issue(&spec).expect("creates"), 8451);
}

#[test]
fn gh_backend_refuses_a_create_whose_url_carries_no_number() {
    let runner = FakeRunner::new(vec![ok_out("Creating issue in bobmatnyc/trusty-tools\n")]);
    let spec = NewIssue {
        title: "t".to_string(),
        body: "b".to_string(),
        labels: vec![],
        milestone: "m".to_string(),
        parent: None,
    };
    let backend = super::backend::GhEpicBackend::new(runner);
    let err = backend.create_issue(&spec).unwrap_err();
    assert!(err.to_string().contains("nothing filed"), "{err}");
    assert!(issue_number_from_url("").is_err());
    assert_eq!(
        issue_number_from_url("https://x/issues/7").expect("parses"),
        7
    );
}

#[test]
fn gh_backend_parses_the_sub_issue_connection() {
    // gh 2.96 renders `subIssues` as an OBJECT with a `nodes` array, never a
    // bare array — see the tm-epic manual procedure.
    let runner = FakeRunner::new(vec![
        ok_out(
            r#"{"subIssues":{"nodes":[{"number":8447,"title":"[EPIC_8445 PHASE_1] a","state":"OPEN"}],"totalCount":1}}"#,
        ),
        ok_out("{\"body\":\"## Gate\\n\\nnone\\n\"}"),
    ]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let children = backend.children(8445).expect("parses");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].number, 8447);
    assert_eq!(children[0].state, "OPEN");
    assert!(
        children[0].body.contains("## Gate"),
        "{:?}",
        children[0].body
    );
}

/// Critic HIGH: the GraphQL connection is ONE page. A short page would drop
/// rows from a wholesale-replaced block and let `next_phase_number` reuse a
/// number, so `totalCount > nodes.len()` falls back to the paginated REST
/// endpoint the `tm-epic` manual procedure names.
#[test]
fn gh_backend_pages_a_truncated_sub_issue_connection() {
    let runner = FakeRunner::new(vec![
        // One node, but the server says there are two.
        ok_out(
            r#"{"subIssues":{"nodes":[{"number":1,"title":"[EPIC_9 PHASE_1] a","state":"OPEN"}],"totalCount":2}}"#,
        ),
        // Two arrays back to back. gh 2.96 does NOT emit this — its
        // `jsonArrayWriter` merges `--paginate` pages into one array, verified
        // live over 68 pages with zero `][` seams — so this case is defence
        // against an OLDER gh that concatenated them, kept because the stream
        // reader handles it for free. The merged shape is the normal one, and
        // `gh_backend_pages_a_body_carrying_a_reference_style_link` covers it.
        ok_out(
            r#"[{"number":1,"title":"[EPIC_9 PHASE_1] a","state":"open"}][{"number":2,"title":"[EPIC_9 PHASE_2] b","state":"closed"}]"#,
        ),
        ok_out("{\"body\":\"## Gate\\n\\nga\\n\"}"),
        ok_out("{\"body\":\"## Gate\\n\\ngb\\n\"}"),
    ]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let children = backend.children(9).expect("pages");
    assert_eq!(
        children.len(),
        2,
        "the short page was replaced, not trusted"
    );
    assert_eq!(children[1].number, 2);
    assert_eq!(children[1].state, "closed");
}

/// The complete page is used as-is — the fallback costs a round trip and must
/// not fire when the first read was whole.
#[test]
fn gh_backend_trusts_a_complete_sub_issue_page() {
    let runner = FakeRunner::new(vec![
        ok_out(
            r#"{"subIssues":{"nodes":[{"number":1,"title":"[EPIC_9 PHASE_1] a","state":"OPEN"}],"totalCount":1}}"#,
        ),
        ok_out("{\"body\":\"## Gate\\n\\nga\\n\"}"),
    ]);
    let backend = super::backend::GhEpicBackend::new(runner);
    assert_eq!(backend.children(9).expect("reads").len(), 1);
}

/// Critic HIGH: the lookup enumerates a label-filtered listing, never the
/// search index, and a FULL page is an error rather than "none found" — a full
/// page cannot be told from a truncated one, and guessing files a duplicate.
#[test]
fn gh_backend_refuses_a_full_page_rather_than_reporting_no_tracker() {
    let labels = vec!["epic".to_string(), "ws/x".to_string()];
    let rows: Vec<String> = (1..=200)
        .map(|n| format!(r#"{{"number":{n},"title":"unrelated {n}"}}"#))
        .collect();
    let runner = FakeRunner::new(vec![ok_out(&format!("[{}]", rows.join(",")))]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let err = backend.find_tracker("An outcome", &labels).unwrap_err();
    assert!(err.to_string().contains("full page"), "{err}");
    assert!(err.to_string().contains("--tracker"), "{err}");
}

#[test]
fn gh_backend_finds_a_tracker_in_a_label_filtered_listing() {
    let labels = vec!["epic".to_string(), "ws/x".to_string()];
    let runner = FakeRunner::new(vec![ok_out(
        r#"[{"number":8445,"title":"[EPIC 8445] An outcome"},{"number":9,"title":"An outcome"}]"#,
    )]);
    let backend = super::backend::GhEpicBackend::new(runner);
    assert_eq!(
        backend.find_tracker("An outcome", &labels).expect("reads"),
        Some(FoundTracker {
            number: 8445,
            placeholder: false
        }),
        "only the title carrying the issue's OWN number is a final-form tracker"
    );
}

/// Review finding: `split("][")` cut a child body containing a
/// reference-style markdown link, so `create` and `sync` both failed closed on
/// any epic whose connection page was short. Streaming top-level values cannot
/// be fooled by content.
#[test]
fn gh_backend_pages_a_body_carrying_a_reference_style_link() {
    let runner = FakeRunner::new(vec![
        ok_out(
            r#"{"subIssues":{"nodes":[{"number":1,"title":"[EPIC_9 PHASE_1] a","state":"OPEN"}],"totalCount":2}}"#,
        ),
        // gh 2.96 merges `--paginate` pages into ONE array; the `][` here is
        // inside a title, which is exactly what a seam-split would cut.
        ok_out(
            r#"[{"number":1,"title":"see [the spec][ref]","state":"open"},{"number":2,"title":"[EPIC_9 PHASE_2] b","state":"closed"}]"#,
        ),
        ok_out("{\"body\":\"## Gate\\n\\nga\\n\"}"),
        ok_out("{\"body\":\"## Gate\\n\\ngb\\n\"}"),
    ]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let children = backend
        .children(9)
        .expect("parses a body with a `][` in it");
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].title, "see [the spec][ref]");
}

/// Review finding: a run KILLED between the create and the retitle leaves the
/// placeholder title, and an exact-match lookup reported "no tracker exists".
#[test]
fn gh_backend_finds_a_placeholder_titled_tracker() {
    let labels = vec!["epic".to_string()];
    let runner = FakeRunner::new(vec![ok_out(
        r#"[{"number":8445,"title":"[EPIC] An outcome"}]"#,
    )]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let found = backend
        .find_tracker("An outcome", &labels)
        .expect("reads")
        .expect("the placeholder is a match");
    assert_eq!(found.number, 8445);
    assert!(found.placeholder, "and it reports WHICH form matched");

    // A placeholder AND a real tracker is still ambiguous, so it still refuses.
    let runner = FakeRunner::new(vec![ok_out(
        r#"[{"number":8445,"title":"[EPIC 8445] An outcome"},{"number":8446,"title":"[EPIC] An outcome"}]"#,
    )]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let err = backend.find_tracker("An outcome", &labels).unwrap_err();
    assert!(err.to_string().contains("--tracker"), "{err}");
}

#[test]
fn gh_backend_reads_an_issues_comment_bodies() {
    let runner = FakeRunner::new(vec![ok_out(
        r#"{"comments":[{"body":"no-project: missing scopes"},{"body":"unrelated"}]}"#,
    )]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let comments = backend.comments(8445).expect("reads");
    assert_eq!(comments.len(), 2);
    assert!(comments[0].starts_with(NO_PROJECT_PREFIX), "{comments:?}");
}

#[test]
fn gh_backend_reports_an_absent_plan_doc_as_none() {
    // `git log` exits 0 with empty output when the path is on no commit of the
    // ref, which is the whole signal D4's refusal rests on.
    let backend = super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out("\n")]));
    assert_eq!(backend.publish_sha(REL_PATH).expect("runs"), None);
    let backend = super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out(PUBLISHED_SHA)]));
    assert_eq!(
        backend.publish_sha(REL_PATH).expect("runs").as_deref(),
        Some(PUBLISHED_SHA)
    );
}

/// The fail-open that wiped #8445, refused at the lowest layer too.
#[test]
fn gh_backend_refuses_to_write_an_empty_body() {
    let runner = FakeRunner::new(vec![ok_out("")]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let err = backend.set_body(8445, "   \n").unwrap_err();
    assert!(
        err.to_string().contains("refusing to write an empty body"),
        "{err}"
    );
}

// ------------------------------------------------------ required verb options

#[test]
fn epic_create_requires_a_milestone() {
    let err = super::require_milestone(None).unwrap_err();
    assert!(err.to_string().contains("--milestone"), "{err}");
    assert!(super::require_milestone(Some("  ".to_string())).is_err());
    assert_eq!(
        super::require_milestone(Some("Issue management".to_string())).expect("accepts"),
        "Issue management"
    );
}

#[test]
fn epic_create_requires_a_component() {
    let err = super::require_components(vec![]).unwrap_err();
    assert!(err.to_string().contains("--component"), "{err}");
    assert!(super::require_components(vec![" ".to_string()]).is_err());
    assert_eq!(
        super::require_components(vec!["trusty-mpm".to_string()]).expect("accepts"),
        vec!["trusty-mpm".to_string()]
    );
    // Critic finding: `--component "" --component api` used to pass the empty
    // label straight through to `gh`.
    assert_eq!(
        super::require_components(vec![String::new(), "api".to_string()]).expect("accepts"),
        vec!["api".to_string()],
        "a blank entry is dropped, never forwarded as a label"
    );
}

#[test]
fn epic_create_requires_a_session_name() {
    assert_eq!(
        super::require_session(Some("tm-trusty-tools-15".to_string())).expect("accepts"),
        "tm-trusty-tools-15"
    );
    // Outside tmux there is no name to derive, so an unset flag is a refusal
    // rather than an issue filed without its `ws/` label. Inside tmux the tmux
    // name answers, which is the arm this assertion tolerates.
    match super::require_session(None) {
        Ok(name) => assert!(!name.trim().is_empty(), "a derived name is never blank"),
        Err(e) => assert!(e.to_string().contains("--session"), "{e}"),
    }
}

// ------------------------------------------------ phase 2 (#8448): State cell

/// A child with the given state and labels, for the State-cell tests.
fn labelled_child(state: &str, labels: &[&str]) -> ChildIssue {
    ChildIssue {
        number: 5,
        title: render::phase_title(1, 1, "x"),
        state: state.to_string(),
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        body: "## Gate\n\ng\n".to_string(),
    }
}

/// AC6: a closed child reads `closed`, whatever label it still wears.
#[test]
fn state_cell_reads_closed_for_a_closed_child() {
    assert_eq!(
        render::state_cell(&labelled_child("CLOSED", &["status:tested"]), STATUS),
        "closed"
    );
}

/// AC6: an open child reads its `status:*` label without the prefix.
#[test]
fn state_cell_reads_the_status_label_of_an_open_child() {
    for state in ["in-progress", "coded", "merged", "tested"] {
        let label = format!("status:{state}");
        let child = labelled_child("OPEN", &["trusty-mpm", &label]);
        assert_eq!(render::state_cell(&child, STATUS), state);
        let table = render::phases_table(std::slice::from_ref(&child), STATUS);
        assert!(table.contains(&format!("| #5 | {state} |")), "{table}");
    }
}

/// AC6: an open child with no `status:*` label reads `open`.
#[test]
fn state_cell_reads_open_for_an_unlabelled_open_child() {
    assert_eq!(
        render::state_cell(
            &labelled_child("OPEN", &["trusty-mpm", "enhancement"]),
            STATUS
        ),
        "open"
    );
}

/// #8448 (review MEDIUM 4): the prefix is the model's `status_prefix`, not a
/// constant. Under the crate default's `unicorn:` a `unicorn:coded` child reads
/// `coded`; a hardcoded `status:` read it as `open`, and read a `status:coded`
/// child as `coded` under a model that never issues that label.
#[test]
fn phases_table_uses_the_configured_status_prefix() {
    let child = labelled_child("OPEN", &["unicorn", "unicorn:coded"]);
    assert_eq!(render::state_cell(&child, "unicorn:"), "coded");
    assert_eq!(render::state_cell(&child, STATUS), "open");
    let table = render::phases_table(std::slice::from_ref(&child), "unicorn:");
    assert!(table.contains("| #5 | coded |"), "{table}");
    let default_model: StateModel =
        serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("the default model parses");
    assert_eq!(default_model.label_config.status_prefix, "unicorn:");
}

/// #8448 (review MEDIUM 3): the live #8445 body wraps every outcome across
/// indented continuation lines; `close` must post the whole text, not the
/// first line. The fold stops at a blank line, a new `- ` item, or a heading.
#[test]
fn outcomes_of_folds_wrapped_continuation_lines() {
    let body = "## Outcomes\n\
                \n\
                - **O1** A tracker and its phase issues are created from a committed plan\n\
                \x20 document by one command, with titles, labels, milestone and native sub-issue\n\
                \x20 links applied without anyone typing them.\n\
                - **O2** The `phases` block is regenerated from live child state by a command,\n\
                \x20 never by a session retyping a table, and content outside the markers is\n\
                \x20 provably untouched.\n\
                - **O3** A phase transition updates its tracker without a human remembering to,\n\
                \x20 so a closed phase never leaves a stale tracker.\n\
                - **O4** `tm issue audit` reports a tracker whose block has drifted from its\n\
                \x20 children, so the guarantee is checked rather than asserted.\n\
                \n\
                \x20 not an outcome: a blank line ended the fold\n\
                \n\
                ## Ratified decisions\n\
                \n\
                - **O9** a heading ended the section\n";
    let outcomes = render::outcomes_of(body);
    assert_eq!(outcomes.len(), 4, "{outcomes:?}");
    assert_eq!(outcomes[0].0, "O1");
    assert_eq!(
        outcomes[0].1,
        "A tracker and its phase issues are created from a committed plan document by one \
         command, with titles, labels, milestone and native sub-issue links applied without \
         anyone typing them."
    );
    assert_eq!(
        outcomes[3].1,
        "`tm issue audit` reports a tracker whose block has drifted from its children, so the \
         guarantee is checked rather than asserted."
    );
    assert!(!outcomes[3].1.contains("not an outcome"), "{outcomes:?}");
}

// ------------------------------------------------------- phase 2: `defer`

fn defer_opts() -> DeferOptions {
    DeferOptions {
        item: "The | pipe thing".to_string(),
        why: "later".to_string(),
        destination: "unscheduled".to_string(),
    }
}

/// AC5: exactly one row lands, escaped, and the report says it was written.
#[test]
fn defer_appends_exactly_one_row() {
    let backend = FakeBackend::new();
    let before = tracker_fixture("| # |");
    backend.seed_tracker(100, "[EPIC 100] An outcome", &before);
    let report = defer::defer(&backend, 100, &defer_opts()).expect("defers");
    assert!(!report.unchanged);
    let after = backend.issue(100).body;
    let block = render::block_content(&after, DEFERRED_START, DEFERRED_END).expect("block");
    let before_block = render::block_content(&before, DEFERRED_START, DEFERRED_END).expect("b");
    assert_eq!(
        block.lines().count(),
        before_block.lines().count() + 1,
        "{block}"
    );
    assert!(
        block.ends_with(r"| The \| pipe thing | later | unscheduled |"),
        "{block}"
    );
}

/// AC5: the `phases` and `followups` blocks — and everything else outside the
/// `deferred` markers — are byte-identical before and after.
#[test]
fn defer_leaves_the_phases_and_followups_blocks_byte_identical() {
    let backend = FakeBackend::new();
    let before = tracker_fixture("| 1 | phase 1 | #101 | in-progress | gate 1 |");
    backend.seed_tracker(100, "[EPIC 100] An outcome", &before);
    defer::defer(&backend, 100, &defer_opts()).expect("defers");
    let after = backend.issue(100).body;
    assert_ne!(after, before);
    let split = |text: &str| -> (String, String) {
        let start = text.find(DEFERRED_START).expect("start");
        let end = text.find(DEFERRED_END).expect("end");
        (
            text[..start + DEFERRED_START.len()].to_string(),
            text[end..].to_string(),
        )
    };
    let (head_before, tail_before) = split(&before);
    let (head_after, tail_after) = split(&after);
    assert_eq!(
        head_before, head_after,
        "bytes before the deferred block changed"
    );
    assert_eq!(
        tail_before, tail_after,
        "bytes after the deferred block changed"
    );
    assert!(head_after.contains(PHASES_START) && head_after.contains(PHASES_END));
    assert!(tail_after.contains(FOLLOWUPS_START) && tail_after.contains(FOLLOWUPS_END));
}

#[test]
fn defer_is_a_no_op_when_the_row_is_already_present() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    defer::defer(&backend, 100, &defer_opts()).expect("first");
    let report = defer::defer(&backend, 100, &defer_opts()).expect("second");
    assert!(report.unchanged, "the same row must not stack");
    assert_eq!(backend.calls("set_body"), 1);
}

#[test]
fn defer_refuses_a_blank_cell_and_writes_nothing() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    let mut opts = defer_opts();
    opts.why = "  ".to_string();
    let err = defer::defer(&backend, 100, &opts).unwrap_err();
    assert!(err.to_string().contains("--why"), "{err}");
    assert_eq!(backend.calls("body"), 0, "refused before any read");
    assert_eq!(backend.calls("set_body"), 0);
}

#[test]
fn defer_seeds_the_header_into_an_empty_block() {
    let backend = FakeBackend::new();
    let body = format!("prose\n{DEFERRED_START}\n{DEFERRED_END}\n");
    backend.seed_tracker(100, "[EPIC 100] An outcome", &body);
    defer::defer(&backend, 100, &defer_opts()).expect("defers");
    let after = backend.issue(100).body;
    assert!(
        after.contains("| Item | Why deferred | Where it went |"),
        "{after}"
    );
    assert!(after.starts_with("prose\n"), "{after}");
}

// ------------------------------------------------------- phase 2: `close`

/// A tracker body declaring two outcomes above the marker blocks.
fn tracker_with_outcomes() -> String {
    format!(
        "## Outcomes\n\n- **O1** The first outcome.\n- **O2** The second outcome.\n\n{}",
        tracker_fixture("| # |")
    )
}

fn two_evidence() -> Vec<String> {
    vec![
        "O1: proved by #101".to_string(),
        "O2 proved by #102".to_string(),
    ]
}

fn seed_closable(backend: &FakeBackend, child_states: &[&str]) {
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_with_outcomes());
    for (i, state) in child_states.iter().enumerate() {
        let n = 101 + i as u64;
        let title = render::phase_title(100, i as u64 + 1, &format!("phase {}", i + 1));
        backend.seed_child(100, n, &title, state, "## Gate\n\ng\n");
    }
}

/// AC3, first half: an open child is a refusal that names it, and nothing is
/// posted or closed.
#[test]
fn close_refuses_while_a_child_is_open_and_names_it() {
    let backend = FakeBackend::new();
    seed_closable(&backend, &["CLOSED", "OPEN"]);
    let err = close::close(&backend, 100, &two_evidence()).unwrap_err();
    assert!(err.to_string().contains("#102"), "{err}");
    assert!(!err.to_string().contains("#101"), "{err}");
    assert_eq!(backend.calls("comment"), 0);
    assert_eq!(backend.calls("close_issue"), 0);
    assert_eq!(backend.issue(100).state, "OPEN");
}

/// AC3, second half: once the child is closed the same call succeeds.
#[test]
fn close_succeeds_once_every_child_is_closed() {
    let backend = FakeBackend::new();
    seed_closable(&backend, &["CLOSED", "OPEN"]);
    close::close(&backend, 100, &two_evidence()).expect_err("refused while #102 is open");
    backend
        .issues
        .borrow_mut()
        .get_mut(&102)
        .expect("#102")
        .state = "CLOSED".to_string();
    let report = close::close(&backend, 100, &two_evidence()).expect("closes");
    assert_eq!(report.phases, vec![101, 102]);
    assert!(report.comment_posted);
    assert_eq!(backend.issue(100).state, "CLOSED");
}

/// AC3: the comment carries one line per outcome the tracker body declares.
#[test]
fn close_posts_one_line_per_declared_outcome() {
    let backend = FakeBackend::new();
    seed_closable(&backend, &["CLOSED"]);
    let report = close::close(&backend, 100, &two_evidence()).expect("closes");
    assert_eq!(report.outcomes, 2);
    let comments = backend.issue(100).comments;
    assert_eq!(comments.len(), 1, "{comments:?}");
    let comment = &comments[0];
    assert!(comment.starts_with(CLOSE_COMMENT_PREFIX), "{comment}");
    assert!(
        comment.contains("- **O1** The first outcome. — proved by #101"),
        "{comment}"
    );
    assert!(
        comment.contains("- **O2** The second outcome. — proved by #102"),
        "{comment}"
    );
    assert_eq!(comment.matches("\n- **O").count(), 2, "{comment}");
}

/// A tracker with no linked phases has nothing this verb can vouch for — and
/// an empty page is exactly what a broken sub-issue read would look like.
#[test]
fn close_refuses_a_tracker_with_no_children() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_with_outcomes());
    let err = close::close(&backend, 100, &two_evidence()).unwrap_err();
    assert!(err.to_string().contains("no native sub-issues"), "{err}");
    assert_eq!(backend.calls("close_issue"), 0);
}

#[test]
fn close_refuses_a_tracker_that_declares_no_outcomes() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "CLOSED",
        "## Gate\n\ng\n",
    );
    let err = close::close(&backend, 100, &two_evidence()).unwrap_err();
    assert!(err.to_string().contains("## Outcomes"), "{err}");
    assert_eq!(backend.calls("comment"), 0);
    assert_eq!(backend.calls("close_issue"), 0);
}

#[test]
fn close_refuses_evidence_that_does_not_match_the_declared_outcomes() {
    let backend = FakeBackend::new();
    seed_closable(&backend, &["CLOSED"]);
    // Missing O2, unknown O3.
    let err = close::close(
        &backend,
        100,
        &["O1: yes".to_string(), "O3: invented".to_string()],
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("missing [O2]"), "{text}");
    assert!(text.contains("unknown [O3]"), "{text}");
    // A repeated id, and an entry with no text, are refused too.
    let err = close::close(&backend, 100, &["O1: a".to_string(), "O1: b".to_string()]).unwrap_err();
    assert!(err.to_string().contains("twice"), "{err}");
    let err = close::close(&backend, 100, &["O1:".to_string()]).unwrap_err();
    assert!(err.to_string().contains("O<n>: <what proves it>"), "{err}");
    assert_eq!(backend.calls("comment"), 0);
    assert_eq!(backend.calls("close_issue"), 0);
}

/// The two-call tail: a run killed after the comment and before the close
/// leaves the comment; the re-run closes without posting a second one.
#[test]
fn close_does_not_post_a_second_comment_on_re_run() {
    let backend = FakeBackend::new().fails("close_issue:1");
    seed_closable(&backend, &["CLOSED"]);
    let err = close::close(&backend, 100, &two_evidence()).unwrap_err();
    assert!(err.to_string().contains("still open"), "{err}");
    assert_eq!(backend.issue(100).comments.len(), 1);
    assert_eq!(backend.issue(100).state, "OPEN");

    backend.clear_failures();
    let report = close::close(&backend, 100, &two_evidence()).expect("re-run closes");
    assert!(
        !report.comment_posted,
        "the comment was already on the record"
    );
    assert_eq!(
        backend.issue(100).comments.len(),
        1,
        "one closing comment, not two"
    );
    assert_eq!(backend.issue(100).state, "CLOSED");
}

#[test]
fn close_propagates_a_failed_close_call() {
    let backend = FakeBackend::new().fails("close_issue");
    seed_closable(&backend, &["CLOSED"]);
    let err = close::close(&backend, 100, &two_evidence()).unwrap_err();
    assert!(
        err.to_string().contains("scripted failure: close_issue"),
        "{err}"
    );
    assert!(err.to_string().contains("#100"), "{err}");
}

// ------------------------------------------- phase 2: the transition hook

fn model() -> StateModel {
    serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("the default model parses")
}

/// A tracker whose `phases` block is stale (header only) with one linked,
/// phase-titled child; the transition moves that child `queued → approved`.
fn seed_hookable(backend: &FakeBackend, parent: Option<u64>) -> FakeTickets {
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    let title = render::phase_title(100, 1, "phase 1");
    backend.issues.borrow_mut().insert(
        101,
        FakeIssue {
            title: title.clone(),
            body: "## Gate\n\ng\n".to_string(),
            state: "OPEN".to_string(),
            parent,
            // The lifecycle label the LIVE child wears after the swap, which
            // is what the regenerated State cell must show (AC6) — under the
            // default model's `unicorn:` prefix, not this repo's `status:`.
            labels: vec!["unicorn:coded".to_string()],
            ..FakeIssue::default()
        },
    );
    FakeTickets::new(101, &title, &["unicorn", "unicorn:queued"])
}

/// The happy path: the label moves, then the parent tracker's block is
/// regenerated from the child's live state.
#[test]
fn transition_hook_regenerates_the_parent_trackers_block() {
    let backend = FakeBackend::new();
    let tickets = seed_hookable(&backend, Some(100));
    let hooked =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 101, "approved", None)
            .expect("transitions and syncs");
    assert!(!hooked.report.no_op);
    let synced = hooked.synced.expect("the tracker was synced");
    assert_eq!(synced.tracker, 100);
    assert_eq!(tickets.count("swap_labels"), 1);
    assert!(
        backend
            .issue(100)
            .body
            .contains("| 1 | phase 1 | #101 | coded | g |"),
        "{}",
        backend.issue(100).body
    );
}

/// AC1: the sync fails AFTER the label moved, and the command fails with it —
/// naming the stale tracker and the repair. The backend errors ONLY on the
/// sync's write; every other call succeeds. Deleting the `?` on the sync arm
/// turns this into a plain success, which is the fail-open this pins.
#[test]
fn transition_hook_fails_the_command_when_the_sync_fails() {
    let backend = FakeBackend::new().fails("set_body");
    let tickets = seed_hookable(&backend, Some(100));
    let err =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 101, "approved", None)
            .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("tracker #100"), "{text}");
    assert!(text.contains("tm issue epic sync 100"), "{text}");
    assert!(text.contains("is now `approved`"), "{text}");
    assert_eq!(
        tickets.count("swap_labels"),
        1,
        "the label had already moved"
    );
    assert_eq!(backend.calls("set_body"), 1, "the write was attempted once");
}

/// AC2, first half: a title that is not `[EPIC_<n> PHASE_<m>]` costs no epic
/// backend call at all — not even a parent read.
#[test]
fn transition_hook_makes_no_epic_call_for_a_non_phase_title() {
    let backend = FakeBackend::new();
    let tickets = FakeTickets::new(7, "a plain issue", &["unicorn", "unicorn:queued"]);
    let hooked =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 7, "approved", None)
            .expect("transitions");
    assert!(hooked.synced.is_none());
    assert_eq!(tickets.count("swap_labels"), 1);
    assert_eq!(backend.total_calls(), 0, "{:?}", backend.counts.borrow());
}

/// AC2, second half: a phase-titled issue with no parent costs one parent
/// read and no tracker read or write — exactly one backend call in total.
#[test]
fn transition_hook_reads_no_tracker_for_a_parentless_phase() {
    let backend = FakeBackend::new();
    let tickets = seed_hookable(&backend, None);
    let hooked =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 101, "approved", None)
            .expect("transitions");
    assert!(hooked.synced.is_none());
    assert_eq!(backend.calls("parent"), 1);
    assert_eq!(backend.total_calls(), 1, "{:?}", backend.counts.borrow());
}

/// #8448 (review HIGH 1): a run killed between the label swap and the sync
/// leaves the tracker stale, and re-issuing the command is then a no-op
/// transition (#8003). The no-op still syncs, so the re-run converges; a
/// no-op that returned before the sync exited 0 and repaired nothing.
#[test]
fn transition_hook_regenerates_a_stale_tracker_on_a_no_op_transition() {
    let backend = FakeBackend::new();
    let tickets = seed_hookable(&backend, Some(100));
    let hooked =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 101, "queued", None)
            .expect("no-op transitions and syncs");
    assert!(hooked.report.no_op);
    assert_eq!(tickets.count("swap_labels"), 0, "the no-op wrote no label");
    let synced = hooked.synced.expect("the stale tracker was synced");
    assert_eq!(synced.tracker, 100);
    assert!(!synced.unchanged, "the block was stale, so it was written");
    assert_eq!(backend.calls("set_body"), 1);
    assert!(
        backend
            .issue(100)
            .body
            .contains("| 1 | phase 1 | #101 | coded | g |"),
        "{}",
        backend.issue(100).body
    );
    // The second re-run finds the block current and writes nothing.
    let again =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 101, "queued", None)
            .expect("no-op");
    assert!(again.synced.expect("synced").unchanged);
    assert_eq!(backend.calls("set_body"), 1, "no second write");
}

/// The zero-call guarantee of AC2 holds for a no-op too: a non-phase title
/// costs no epic backend call whether or not the transition moved anything.
#[test]
fn transition_hook_makes_no_epic_call_for_a_no_op_on_a_non_phase_title() {
    let backend = FakeBackend::new();
    let tickets = FakeTickets::new(7, "a plain issue", &["unicorn", "unicorn:queued"]);
    let hooked =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 7, "queued", None)
            .expect("no-op");
    assert!(hooked.report.no_op);
    assert!(hooked.synced.is_none());
    assert_eq!(tickets.count("swap_labels"), 0);
    assert_eq!(backend.total_calls(), 0, "{:?}", backend.counts.borrow());
}

/// A parent read that fails cannot tell "no tracker" from "could not look", so
/// it fails the command and names the tracker by the title's number.
#[test]
fn transition_hook_names_the_tracker_when_the_parent_read_fails() {
    let backend = FakeBackend::new().fails("parent");
    let tickets = seed_hookable(&backend, Some(100));
    let err =
        hook::transition_with_tracker_sync(&tickets, &backend, &model(), 101, "approved", None)
            .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("tracker #100"), "{text}");
    assert!(text.contains("tm issue epic sync 100"), "{text}");
    assert_eq!(render::epic_number_of("[EPIC_8445 PHASE_2] x"), Some(8445));
    assert_eq!(render::epic_number_of("[EPIC 8445] x"), None);
}

// --------------------------------------------- phase 2: audit set-level rows

fn row<'a>(
    rows: &'a [trusty_mpm::core::issue_audit::AuditRow],
    req: &str,
) -> &'a trusty_mpm::core::issue_audit::AuditRow {
    rows.iter()
        .find(|r| r.requirement == req)
        .unwrap_or_else(|| panic!("no {req} row in {rows:?}"))
}

#[test]
fn audit_rows_pass_a_tracker_whose_block_matches() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "OPEN",
        "## Gate\n\ng\n",
    );
    sync::sync(&backend, 100, STATUS).expect("bring the block current");
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(row(&rows, REQ_PHASES_BLOCK).verdict, Verdict::Pass);
    assert_eq!(row(&rows, REQ_PHASE_LINKAGE).verdict, Verdict::Pass);
    assert_eq!(
        row(&rows, REQ_PHASE_LINKAGE).detail,
        "1 of 1 linked phases seen by search"
    );
}

/// #8448 (review HIGH 2): the candidates come from GitHub's search index,
/// which lags a just-created issue. A linked phase the search did not return
/// proves the result is short, and a short result cannot vouch for the
/// unlinked set — so the row FAILs with the shortfall, never PASSes.
#[test]
fn audit_rows_fail_when_search_omits_a_linked_phase() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    for (n, phase) in [(101, 1), (102, 2)] {
        backend.seed_child(
            100,
            n,
            &render::phase_title(100, phase, "p"),
            "OPEN",
            "## Gate\n\ng\n",
        );
    }
    sync::sync(&backend, 100, STATUS).expect("current");
    backend.hide_from_search(102);
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    let linkage = row(&rows, REQ_PHASE_LINKAGE);
    assert_eq!(linkage.verdict, Verdict::Fail, "{linkage:?}");
    assert_eq!(
        linkage.detail,
        "search index returned 1 of 2 known phases — re-run in a minute"
    );
    assert_eq!(backend.calls("phase_titled_issues"), 1);
}

/// #8448 (review MEDIUM): with no linked phase there is nothing to cross-check
/// the search against — an empty result and a lagging one are the same bytes —
/// so the row is INFO, not a PASS it cannot back. An unlinked phase-titled
/// issue the search does return still FAILs.
#[test]
fn audit_rows_report_info_when_no_linked_phase_cross_checks_the_search() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    sync::sync(&backend, 100, STATUS).expect("an empty block is current");
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    let linkage = row(&rows, REQ_PHASE_LINKAGE);
    assert_eq!(linkage.verdict, Verdict::Info, "{linkage:?}");
    assert!(
        linkage.detail.contains("cannot be cross-checked"),
        "{}",
        linkage.detail
    );
    assert_eq!(row(&rows, REQ_PHASES_BLOCK).verdict, Verdict::Pass);

    // The same tracker once the search returns an unlinked phase: FAIL wins.
    backend.seed_tracker(102, &render::phase_title(100, 2, "orphan"), "body");
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    let linkage = row(&rows, REQ_PHASE_LINKAGE);
    assert_eq!(linkage.verdict, Verdict::Fail, "{linkage:?}");
    assert!(
        linkage.detail.contains("--add-sub-issue 102"),
        "{}",
        linkage.detail
    );
}

/// AC4: a stale block is a FAIL carrying the exact repair wording.
#[test]
fn audit_rows_fail_a_stale_phases_block_with_the_sync_command() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "OPEN",
        "## Gate\n\ng\n",
    );
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    let block = row(&rows, REQ_PHASES_BLOCK);
    assert_eq!(block.verdict, Verdict::Fail);
    assert_eq!(
        block.detail,
        "phases block stale — run tm issue epic sync 100"
    );
    assert_eq!(backend.calls("set_body"), 0, "an audit never writes");
}

/// AC4: a phase-titled issue that is not a native sub-issue is FAIL, not INFO.
#[test]
fn audit_rows_fail_a_phase_titled_issue_that_is_not_a_sub_issue() {
    let backend = FakeBackend::new();
    backend.seed_tracker(100, "[EPIC 100] An outcome", &tracker_fixture("| # |"));
    backend.seed_child(
        100,
        101,
        &render::phase_title(100, 1, "p"),
        "OPEN",
        "## Gate\n\ng\n",
    );
    sync::sync(&backend, 100, STATUS).expect("current");
    // Phase-titled for #100, parent None — the unlinked case.
    backend.seed_tracker(102, &render::phase_title(100, 2, "orphan"), "body");
    // Phase-titled for ANOTHER epic: not this tracker's concern.
    backend.seed_tracker(103, &render::phase_title(999, 1, "elsewhere"), "body");
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    let linkage = row(&rows, REQ_PHASE_LINKAGE);
    assert_eq!(linkage.verdict, Verdict::Fail, "{linkage:?}");
    assert!(linkage.detail.contains("#102"), "{}", linkage.detail);
    assert!(
        linkage.detail.contains("--add-sub-issue 102"),
        "{}",
        linkage.detail
    );
    assert!(!linkage.detail.contains("#103"), "{}", linkage.detail);
}

#[test]
fn audit_rows_are_empty_for_a_non_tracker() {
    let backend = FakeBackend::new();
    backend.seed_tracker(7, "a plain issue", "prose with no markers\n");
    let rows = audit_rows::epic_rows(&backend, 7, STATUS).expect("rows");
    assert!(rows.is_empty(), "{rows:?}");
    assert_eq!(backend.calls("children"), 0);
    assert_eq!(backend.calls("phase_titled_issues"), 0);
}

#[test]
fn audit_rows_fail_a_body_whose_markers_cannot_be_rewritten() {
    let backend = FakeBackend::new();
    let body = format!("{PHASES_START}\na\n{PHASES_START}\nb\n{PHASES_END}\n");
    backend.seed_tracker(100, "[EPIC 100] An outcome", &body);
    let rows = audit_rows::epic_rows(&backend, 100, STATUS).expect("rows");
    let block = row(&rows, REQ_PHASES_BLOCK);
    assert_eq!(block.verdict, Verdict::Fail);
    assert!(
        block.detail.contains("exactly one is required"),
        "{}",
        block.detail
    );
}

// ------------------------------------------- phase 2: the `gh` layer additions

#[test]
fn gh_backend_reads_a_childs_labels_with_its_body() {
    let runner = FakeRunner::new(vec![
        ok_out(
            r#"{"subIssues":{"nodes":[{"number":8447,"title":"[EPIC_8445 PHASE_1] a","state":"OPEN"}],"totalCount":1}}"#,
        ),
        ok_out(
            "{\"body\":\"## Gate\\n\\nnone\\n\",\"labels\":[{\"name\":\"status:coded\"},{\"name\":\"trusty-mpm\"}]}",
        ),
    ]);
    let backend = super::backend::GhEpicBackend::new(runner);
    let children = backend.children(8445).expect("parses");
    assert_eq!(children[0].labels, vec!["status:coded", "trusty-mpm"]);
    assert_eq!(render::state_cell(&children[0], STATUS), "coded");
    let calls = backend_calls(&backend);
    assert!(calls[1].contains(&"body,labels".to_string()), "{calls:?}");
}

/// Borrow the fake runner's argv log back out of a backend.
fn backend_calls(backend: &super::backend::GhEpicBackend<FakeRunner>) -> Vec<Vec<String>> {
    backend.runner().calls.borrow().clone()
}

#[test]
fn gh_backend_reads_a_parent_and_its_absence() {
    let backend = super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out(
        r#"{"parent":{"number":8445,"title":"[EPIC 8445] x"}}"#,
    )]));
    assert_eq!(backend.parent(8448).expect("reads"), Some(8445));
    let backend =
        super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out(r#"{"parent":null}"#)]));
    assert_eq!(backend.parent(8448).expect("reads"), None);
    // A failed read is an error, never `None`.
    let backend = super::backend::GhEpicBackend::new(FakeRunner::new(vec![]));
    assert!(backend.parent(8448).is_err());
}

/// #8448 (review LOW 5): a `--json parent` answer always carries the key, so
/// a document without it is a parse error — never "no parent", which would
/// make the transition hook skip the sync.
#[test]
fn gh_backend_refuses_a_parent_read_with_no_parent_key() {
    let backend =
        super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out(r#"{"number":8448}"#)]));
    let err = backend.parent(8448).unwrap_err();
    assert!(err.to_string().contains("parent of #8448"), "{err}");
}

#[test]
fn gh_backend_refuses_a_full_phase_title_page() {
    let rows: Vec<String> = (1..=200)
        .map(|n| format!(r#"{{"number":{n},"title":"[EPIC_9 PHASE_{n}] p"}}"#))
        .collect();
    let backend = super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out(&format!(
        "[{}]",
        rows.join(",")
    ))]));
    let err = backend.phase_titled_issues(9).unwrap_err();
    assert!(err.to_string().contains("full page"), "{err}");
    let backend = super::backend::GhEpicBackend::new(FakeRunner::new(vec![ok_out(
        r#"[{"number":8447,"title":"[EPIC_8445 PHASE_1] a"}]"#,
    )]));
    let found = backend.phase_titled_issues(8445).expect("reads");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].number, 8447);
    let calls = backend_calls(&backend);
    assert!(
        calls[0].contains(&"in:title \"EPIC_8445 PHASE_\"".to_string()),
        "{calls:?}"
    );
}
