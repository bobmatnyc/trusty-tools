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
use std::collections::{BTreeMap, HashMap};

use super::backend::{ChildIssue, EpicBackend, NewIssue, issue_number_from_url};
use super::create::{self, CreateOptions, NO_PROJECT_PREFIX};
use super::plan::{self, PlanError};
use super::render::{
    self, DEFERRED_END, DEFERRED_START, FOLLOWUPS_END, FOLLOWUPS_START, PHASES_END, PHASES_START,
};
use super::sync;
use crate::commands::ticket::runner::{CommandOutput, CommandRunner};

/// The repository-relative path the fake backend reports for any plan path.
const REL_PATH: &str = "docs/research/tm-epic-cli/epic-plan.md";
/// A plausible 40-hex commit for the permalink assertions.
const PUBLISHED_SHA: &str = "581cfb4da254f7448c5c042c8d9bea50ebe84828";

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
    fail: RefCell<Vec<String>>,
    /// The label set the last `find_tracker` call narrowed by (#8447 HIGH).
    find_labels: RefCell<Vec<String>>,
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
        }
    }

    /// Script one call to fail. `key` is `<op>` or `<op>:<nth>`.
    fn fails(self, key: &str) -> Self {
        self.fail.borrow_mut().push(key.to_string());
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
        if fail.iter().any(|k| k == op || k == &format!("{op}:{nth}")) {
            anyhow::bail!("scripted failure: {op} call {nth}");
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
                body: i.body.clone(),
            })
            .collect())
    }

    fn find_tracker(&self, outcome: &str, labels: &[String]) -> anyhow::Result<Option<u64>> {
        self.tick("find_tracker")?;
        self.find_labels.borrow_mut().clone_from(&labels.to_vec());
        Ok(self
            .issues
            .borrow()
            .iter()
            .find(|(n, i)| render::is_tracker_title(&i.title, **n, outcome))
            .map(|(n, _)| *n))
    }

    fn attach_project(&self, repo: &str, issue: u64, number: u64) -> anyhow::Result<()> {
        self.tick("attach_project")?;
        assert_eq!(repo, self.repo, "the caller resolves the slug once");
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.projects.push(number);
        }
        Ok(())
    }

    fn comment(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        self.tick("comment")?;
        if let Some(found) = self.issues.borrow_mut().get_mut(&issue) {
            found.comments.push(body.to_string());
        }
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
        body: format!("## Gate\n\ngate {phase}\n"),
    }
}

#[test]
fn render_replaces_the_whole_phases_block() {
    let body = format!("prose\n\n{PHASES_START}\n| stale |\n{PHASES_END}\n\ntail\n");
    let table = render::phases_table(&[child(1, 101), child(2, 102)]);
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
    let rendered = render::phases_table(&[ChildIssue {
        number: 7,
        title: render::phase_title(1, 1, "x"),
        state: "CLOSED".to_string(),
        body: body.to_string(),
    }]);
    assert!(rendered.contains("| the gate line. |"), "{rendered}");
    assert!(!rendered.contains("phase summary"), "{rendered}");
    assert!(rendered.contains("| closed |"), "{rendered}");
}

#[test]
fn render_escapes_a_pipe_in_a_phase_title() {
    // The committed plan's own first phase is titled `tm issue epic create|sync`.
    let rendered = render::phases_table(&[ChildIssue {
        number: 9,
        title: "[EPIC_1 PHASE_1] tm issue epic create|sync".to_string(),
        state: "OPEN".to_string(),
        body: "## Gate\n\nnone | really\n".to_string(),
    }]);
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
    let backend = FakeBackend::new().fails("attach_project");
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
    let backend = FakeBackend::new().fails("attach_project").fails("comment");
    let err = create::create(&backend, &opts_for(&dir, Some(3))).unwrap_err();
    assert!(
        err.to_string().contains("scripted failure: comment"),
        "{err}"
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

    let report = sync::sync(&backend, 100).expect("syncs");
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

    sync::sync(&backend, 100).expect("syncs");
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

    let err = sync::sync(&backend, 100).unwrap_err();
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

    let err = sync::sync(&backend, 100).unwrap_err();
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

    sync::sync(&backend, 100).expect("first sync writes");
    assert_eq!(backend.calls("set_body"), 1);
    let report = sync::sync(&backend, 100).expect("second sync is a no-op");
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
    let err = sync::sync(&backend, 100).unwrap_err();
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
    let err = sync::sync(&backend, 100).unwrap_err();
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
        // The REST fallback, two pages concatenated the way `--paginate` emits.
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
        Some(8445),
        "only the title carrying the issue's OWN number is a tracker"
    );
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
