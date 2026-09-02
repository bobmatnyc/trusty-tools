//! Unit tests for `tm pr` (#6653).
//!
//! Every test drives the [`GhRunner`] / [`Preflight`] seams with a scripted
//! fake, so nothing here touches the network, a live `gh`, or a real PR.

use super::body::{self, ATTRIBUTION_FOOTER, FIELDS, Field, IssueLink};
use super::open::{self, ChangelogVerdict, Preflight};
use super::queue_check;
use super::{GhRun, GhRunner, repo_slug};
use crate::cli::{PrOpenArgs, PrQueueCheckArgs};

// ── fakes ────────────────────────────────────────────────────────────────

/// A `gh` seam that answers by argv-prefix match, in registration order.
struct FakeGh {
    /// (argv substring that must appear in the joined argv, response).
    routes: Vec<(String, GhRun)>,
    /// Every argv this fake was asked to run, in order.
    seen: std::cell::RefCell<Vec<Vec<String>>>,
}

impl FakeGh {
    fn new() -> Self {
        Self {
            routes: Vec::new(),
            seen: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn on(mut self, needle: &str, stdout: &str) -> Self {
        self.routes.push((
            needle.to_string(),
            GhRun {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        ));
        self
    }

    fn on_fail(mut self, needle: &str, stderr: &str) -> Self {
        self.routes.push((
            needle.to_string(),
            GhRun {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        ));
        self
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.seen.borrow().clone()
    }
}

impl GhRunner for FakeGh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        self.seen.borrow_mut().push(args.to_vec());
        let joined = args.join(" ");
        for (needle, run) in &self.routes {
            if joined.contains(needle.as_str()) {
                return Ok(run.clone());
            }
        }
        anyhow::bail!("FakeGh: no route for `gh {joined}`")
    }
}

/// A [`Preflight`] with both probes pinned.
struct FakePreflight {
    session: Option<String>,
    changelog: ChangelogVerdict,
}

impl FakePreflight {
    fn ok() -> Self {
        Self {
            session: Some("tm-test-01".to_string()),
            changelog: ChangelogVerdict::Pass,
        }
    }
}

impl Preflight for FakePreflight {
    fn session_name(&self) -> Option<String> {
        self.session.clone()
    }
    fn changelog_gate(&self, _base: &str) -> anyhow::Result<ChangelogVerdict> {
        Ok(self.changelog.clone())
    }
}

// ── body fixtures ────────────────────────────────────────────────────────

/// A body satisfying all seven fields and the footer.
fn full_body() -> String {
    let mut s = String::new();
    for f in FIELDS {
        s.push_str(&format!("## {}\n\nsomething real.\n\n", f.heading()));
    }
    s.push_str(ATTRIBUTION_FOOTER);
    s.push('\n');
    s
}

/// [`full_body`] with the section for `drop` removed entirely.
fn body_without(drop: Field) -> String {
    let mut s = String::new();
    for f in FIELDS {
        if f == drop {
            continue;
        }
        s.push_str(&format!("## {}\n\nsomething real.\n\n", f.heading()));
    }
    s.push_str(ATTRIBUTION_FOOTER);
    s.push('\n');
    s
}

fn open_args(body_file: &str) -> PrOpenArgs {
    PrOpenArgs {
        title: "feat(x): a thing".to_string(),
        body_file: body_file.into(),
        issue: None,
        closes: false,
        rung: None,
        base: "main".to_string(),
        docs_only: false,
        session: None,
        repo: None,
        dry_run: false,
    }
}

// ── body contract ────────────────────────────────────────────────────────

#[test]
fn body_field_table_covers_seven() {
    assert_eq!(FIELDS.len(), 7);
    let mut headings: Vec<&str> = FIELDS.iter().map(|f| f.heading()).collect();
    headings.sort_unstable();
    headings.dedup();
    assert_eq!(headings.len(), 7, "field headings must be distinct");
}

#[test]
fn body_accepts_a_complete_body() {
    let report = body::validate(&full_body());
    assert!(report.missing.is_empty(), "{report:?}");
    assert!(report.empty.is_empty(), "{report:?}");
    assert!(report.footer_ok);
    assert_eq!(report.supplied.len(), 7);
}

#[test]
fn body_reports_each_missing_field() {
    for f in FIELDS {
        let report = body::validate(&body_without(f));
        assert_eq!(report.missing, vec![f], "dropping {f:?} must be reported");
        assert_eq!(report.failures().len(), 1);
    }
}

#[test]
fn body_reports_empty_section() {
    let body = full_body().replace("## Risk\n\nsomething real.\n", "## Risk\n\n");
    let report = body::validate(&body);
    assert_eq!(report.empty, vec![Field::Risk], "{report:?}");
    assert!(report.missing.is_empty());
}

#[test]
fn body_accepts_alias_headings() {
    let body = format!(
        "## 1. Primary outcome\nx\n## 2. What changed\nx\n## 3. Risk / blast radius\nx\n\
         ## 4. Test evidence\nx\n## 5. Pre-existing failures\nx\n\
         ## 6. Documentation / changelog\nx\n## 7. Review-finding disposition\nx\n\n{ATTRIBUTION_FOOTER}\n"
    );
    let report = body::validate(&body);
    assert!(report.missing.is_empty(), "{report:?}");
    assert!(report.empty.is_empty(), "{report:?}");
}

#[test]
fn heading_text_strips_numbering_and_emphasis() {
    // A numbered, emphasised heading still claims its field.
    let body = full_body().replace("## Risk", "### **3. Risk**");
    let report = body::validate(&body);
    assert!(report.missing.is_empty(), "{report:?}");
}

#[test]
fn body_validate_is_repeatable() {
    let text = full_body();
    assert_eq!(body::validate(&text), body::validate(&text));
}

#[test]
fn footer_must_be_last_line() {
    let body = format!("{}\ntrailing prose\n", full_body());
    let report = body::validate(&body);
    assert!(!report.footer_ok);
    assert!(
        report
            .failures()
            .iter()
            .any(|f| f.contains("attribution footer")),
        "{report:?}"
    );
}

#[test]
fn footer_alone_does_not_fill_a_section() {
    // `## Review` holds only the footer — that is not content.
    let mut s = String::new();
    for f in FIELDS {
        if f == Field::Review {
            s.push_str("## Review\n\n");
            continue;
        }
        s.push_str(&format!("## {}\n\nreal.\n\n", f.heading()));
    }
    s.push_str(ATTRIBUTION_FOOTER);
    s.push('\n');
    let report = body::validate(&s);
    assert_eq!(report.empty, vec![Field::Review], "{report:?}");
}

// ── Refs vs Closes ───────────────────────────────────────────────────────

#[test]
fn issue_link_defaults_to_refs() {
    let out =
        body::apply_issue_link(&full_body(), Some(42), IssueLink::Refs).expect("refs link applies");
    assert!(out.lines().any(|l| l.trim() == "Refs #42"), "{out}");
    assert!(!out.contains("Closes #42"));
}

#[test]
fn issue_link_closes_is_opt_in() {
    let out = body::apply_issue_link(&full_body(), Some(42), IssueLink::Closes)
        .expect("closes link applies");
    assert!(out.lines().any(|l| l.trim() == "Closes #42"), "{out}");
}

#[test]
fn issue_link_is_inserted_above_the_footer() {
    let out =
        body::apply_issue_link(&full_body(), Some(7), IssueLink::Refs).expect("refs link applies");
    let refs_at = out
        .lines()
        .position(|l| l.trim() == "Refs #7")
        .expect("line present");
    let footer_at = out
        .lines()
        .position(|l| l.trim() == ATTRIBUTION_FOOTER)
        .expect("footer present");
    assert!(refs_at < footer_at, "{out}");
    // Still the last non-blank line.
    assert!(body::validate(&out).footer_ok);
}

#[test]
fn issue_link_is_idempotent() {
    let once = body::apply_issue_link(&full_body(), Some(9), IssueLink::Refs).expect("first");
    let twice = body::apply_issue_link(&once, Some(9), IssueLink::Refs).expect("second");
    assert_eq!(once, twice);
}

#[test]
fn issue_link_rejects_unrequested_closes() {
    let body = full_body().replace("## Outcome\n", "## Outcome\n\nCloses #3\n");
    let err = body::apply_issue_link(&body, Some(3), IssueLink::Refs)
        .expect_err("an unrequested Closes must be refused");
    assert!(err.contains("Closes"), "{err}");
}

#[test]
fn issue_link_without_issue_leaves_body_alone() {
    let text = full_body();
    let out = body::apply_issue_link(&text, None, IssueLink::Refs).expect("no-op");
    assert_eq!(out, text);
}

// ── open: the plan ───────────────────────────────────────────────────────

#[test]
fn open_argv_carries_shipped_defaults() {
    let args = open_args("/dev/null");
    let plan = open::plan(
        &args,
        &full_body(),
        Some("tm-test-01"),
        ChangelogVerdict::Pass,
    )
    .expect("a complete body plans");
    let joined = plan.argv.join(" ");
    assert!(joined.contains("pr create"), "{joined}");
    assert!(joined.contains("--assignee @me"), "{joined}");
    assert!(joined.contains("--label trusty-mpm"), "{joined}");
    assert!(joined.contains("--label ws/tm-test-01"), "{joined}");
    assert!(joined.contains("--base main"), "{joined}");
    assert_eq!(plan.workstream_label, "ws/tm-test-01");
    assert_eq!(plan.supplied.len(), 7);
}

#[test]
fn open_reports_each_missing_field() {
    for f in FIELDS {
        let args = open_args("/dev/null");
        let failures = open::plan(&args, &body_without(f), Some("s"), ChangelogVerdict::Pass)
            .expect_err("a missing field must fail the plan");
        assert_eq!(failures.len(), 1, "{f:?}: {failures:?}");
        assert!(failures[0].contains(f.heading()), "{failures:?}");
    }
}

#[test]
fn open_rejects_bad_footer() {
    let args = open_args("/dev/null");
    let body = format!("{}\nafterword\n", full_body());
    let failures = open::plan(&args, &body, Some("s"), ChangelogVerdict::Pass)
        .expect_err("a footer that is not last must fail");
    assert!(
        failures.iter().any(|f| f.contains("attribution footer")),
        "{failures:?}"
    );
}

#[test]
fn open_requires_a_session_name() {
    let args = open_args("/dev/null");
    let failures = open::plan(&args, &full_body(), None, ChangelogVerdict::Pass)
        .expect_err("no session name must fail");
    assert!(
        failures.iter().any(|f| f.contains("ws/<session>")),
        "{failures:?}"
    );
}

#[test]
fn open_reports_changelog_failure() {
    let args = open_args("/dev/null");
    let verdict = ChangelogVerdict::Fail("FAIL: crates/x has no fragment".to_string());
    let failures = open::plan(&args, &full_body(), Some("s"), verdict)
        .expect_err("a failing changelog gate must fail the plan");
    assert!(
        failures
            .iter()
            .any(|f| f.contains("check_changelog_fragment.sh")),
        "{failures:?}"
    );
}

#[test]
fn open_docs_only_skips_the_changelog_gate() {
    let mut args = open_args("/dev/null");
    args.docs_only = true;
    let plan = open::plan(&args, &full_body(), Some("s"), ChangelogVerdict::Skipped)
        .expect("docs-only plans without the gate");
    assert!(plan.argv.join(" ").contains("pr create"));
}

#[test]
fn open_plan_reports_every_failure_at_once() {
    let args = open_args("/dev/null");
    let body = body_without(Field::Risk).replace("## Tests\n\nsomething real.\n", "## Tests\n\n");
    let failures = open::plan(&args, &body, None, ChangelogVerdict::Pass)
        .expect_err("three problems must all be reported");
    assert_eq!(failures.len(), 3, "{failures:?}");
}

#[test]
fn shell_render_quotes_multiline_body() {
    let rendered = open::shell_render(&[
        "pr".to_string(),
        "--body".to_string(),
        "line one\nline two".to_string(),
    ]);
    assert_eq!(rendered, "pr --body 'line one\nline two'");
}

// ── open: the run path ───────────────────────────────────────────────────

/// Write `text` to a scratch file and return its path plus the temp dir.
fn scratch_body(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("body.md");
    std::fs::write(&path, text).expect("write body");
    (dir, path)
}

#[test]
fn open_dry_run_never_calls_gh() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.dry_run = true;
    let gh = FakeGh::new();
    let code = open::run(&gh, &args, &FakePreflight::ok()).expect("dry run succeeds");
    assert_eq!(code, super::EXIT_OK);
    assert!(gh.calls().is_empty(), "dry run must not spawn gh");
}

#[test]
fn open_failure_exits_two_without_calling_gh() {
    let (_d, path) = scratch_body(&body_without(Field::Docs));
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new();
    let code = open::run(&gh, &args, &FakePreflight::ok()).expect("a failed check is not an error");
    assert_eq!(code, super::EXIT_CHECK_FAILED);
    assert!(
        gh.calls().is_empty(),
        "gh must not be spawned on a failed check"
    );
}

#[test]
fn open_creates_and_reports() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new().on("pr create", "https://github.com/o/r/pull/4242\n");
    let code = open::run(&gh, &args, &FakePreflight::ok()).expect("create succeeds");
    assert_eq!(code, super::EXIT_OK);
    let calls = gh.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].join(" ").contains("--label ws/tm-test-01"));
}

#[test]
fn open_rejects_an_empty_body_file() {
    let (_d, path) = scratch_body("   \n");
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new();
    let err = open::run(&gh, &args, &FakePreflight::ok()).expect_err("empty body is an error");
    assert!(format!("{err:#}").contains("is empty"), "{err:#}");
}

// ── queue-check ──────────────────────────────────────────────────────────

const REQUIRED: &str = "Clippy\nRust tests\n";

/// A `gh pr view --json` payload with the given overrides folded in.
fn view_json(extra: &str) -> String {
    format!(
        r#"{{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
            "statusCheckRollup":[
              {{"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"}},
              {{"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS"}}],
            "comments":[]{extra}}}"#
    )
}

fn queue_args() -> PrQueueCheckArgs {
    PrQueueCheckArgs {
        pr: Some(1),
        base: "main".to_string(),
        repo: Some("o/r".to_string()),
        json: false,
    }
}

#[test]
fn repo_slug_prefers_the_flag() {
    let gh = FakeGh::new();
    assert_eq!(repo_slug(&gh, Some("o/r")).expect("flag wins"), "o/r");
    assert!(gh.calls().is_empty());
}

#[test]
fn repo_slug_falls_back_to_gh() {
    let gh = FakeGh::new().on("repo view", "owner/repo\n");
    assert_eq!(repo_slug(&gh, None).expect("gh answers"), "owner/repo");
}

#[test]
fn queue_required_contexts_parse() {
    let gh = FakeGh::new().on("branches/main/protection", REQUIRED);
    let got = queue_check::required_contexts(&gh, "o/r", "main").expect("parses");
    assert_eq!(got, vec!["Clippy".to_string(), "Rust tests".to_string()]);
}

#[test]
fn queue_empty_required_list_errors() {
    let gh = FakeGh::new().on("branches/main/protection", "\n");
    let err = queue_check::required_contexts(&gh, "o/r", "main")
        .expect_err("an empty gate list must not read as all-clear");
    assert!(
        format!("{err:#}").contains("no required status checks"),
        "{err:#}"
    );
}

#[test]
fn queue_protection_read_failure_is_an_error() {
    let gh = FakeGh::new().on_fail("branches/main/protection", "HTTP 404");
    let err = queue_check::required_contexts(&gh, "o/r", "main").expect_err("404 is an error");
    assert!(format!("{err:#}").contains("404"), "{err:#}");
}

#[test]
fn queue_reports_mergeable() {
    let gh = FakeGh::new()
        .on("branches/main/protection", REQUIRED)
        .on("pr view", &view_json(""));
    assert_eq!(
        queue_check::run(&gh, &queue_args()).expect("runs"),
        super::EXIT_OK
    );
}

#[test]
fn queue_stop_order_prefers_draft() {
    // Draft AND a hold label AND changes requested AND a red check: draft wins.
    let json = r#"{"isDraft":true,"labels":[{"name":"do-not-merge"}],
        "reviewDecision":"CHANGES_REQUESTED",
        "statusCheckRollup":[{"name":"Clippy","conclusion":"FAILURE"}],"comments":[]}"#;
    assert_eq!(first_reason(json), Some("draft".to_string()));
}

#[test]
fn queue_stop_order_prefers_hold() {
    let json = r#"{"isDraft":false,"labels":[{"name":"do-not-merge"}],
        "reviewDecision":"CHANGES_REQUESTED",
        "statusCheckRollup":[],"comments":[]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("hold label"), "{reason}");
}

#[test]
fn queue_stop_order_prefers_changes_requested() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"CHANGES_REQUESTED",
        "statusCheckRollup":[],
        "comments":[{"body":"code-critic verdict: BLOCK"}]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("CHANGES_REQUESTED"), "{reason}");
}

#[test]
fn queue_stop_order_prefers_critic_block() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "statusCheckRollup":[],
        "comments":[{"body":"code-critic verdict: BLOCK"}]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("code-critic BLOCK"), "{reason}");
}

#[test]
fn queue_critic_block_then_approve_is_clear() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "statusCheckRollup":[
          {"name":"Clippy","conclusion":"SUCCESS"},
          {"name":"Rust tests","conclusion":"SUCCESS"}],
        "comments":[{"body":"code-critic: BLOCK"},{"body":"code-critic: APPROVE"}]}"#;
    assert_eq!(first_reason(json), None);
}

#[test]
fn queue_critic_ignores_unrelated_comments() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "statusCheckRollup":[
          {"name":"Clippy","conclusion":"SUCCESS"},
          {"name":"Rust tests","conclusion":"SUCCESS"}],
        "comments":[{"body":"we should BLOCK bad merges in general"}]}"#;
    assert_eq!(first_reason(json), None);
}

#[test]
fn queue_required_context_missing() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "statusCheckRollup":[{"name":"Clippy","conclusion":"SUCCESS"}],"comments":[]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("`Rust tests` is missing"), "{reason}");
}

#[test]
fn queue_required_context_not_success() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "statusCheckRollup":[
          {"name":"Clippy","conclusion":"SUCCESS"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SKIPPED"}],
        "comments":[]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("`Rust tests` is not SUCCESS"), "{reason}");
}

#[test]
fn queue_accepts_status_context() {
    // A StatusContext entry carries `context`/`state`, not `name`/`conclusion`.
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "statusCheckRollup":[
          {"context":"Clippy","state":"SUCCESS"},
          {"context":"Rust tests","state":"SUCCESS"}],
        "comments":[]}"#;
    assert_eq!(first_reason(json), None);
}

#[test]
fn queue_exits_1_when_any_pr_blocked() {
    let gh = FakeGh::new().on("branches/main/protection", REQUIRED).on(
        "pr view",
        r#"{"isDraft":true,"labels":[],"reviewDecision":null,
                "statusCheckRollup":[],"comments":[]}"#,
    );
    assert_eq!(
        queue_check::run(&gh, &queue_args()).expect("runs"),
        super::EXIT_BLOCKED
    );
}

#[test]
fn queue_lists_open_prs() {
    let gh = FakeGh::new()
        .on("branches/main/protection", REQUIRED)
        .on("pr list", r#"[{"number":12},{"number":7}]"#)
        .on("pr view", &view_json(""));
    let mut args = queue_args();
    args.pr = None;
    assert_eq!(queue_check::run(&gh, &args).expect("runs"), super::EXIT_OK);
    let views: Vec<String> = gh
        .calls()
        .iter()
        .filter(|c| {
            c.first().map(String::as_str) == Some("pr")
                && c.get(1).map(String::as_str) == Some("view")
        })
        .map(|c| c[2].clone())
        .collect();
    assert_eq!(
        views,
        vec!["7".to_string(), "12".to_string()],
        "sorted ascending"
    );
}

#[test]
fn queue_empty_queue_is_ok() {
    let gh = FakeGh::new()
        .on("branches/main/protection", REQUIRED)
        .on("pr list", "[]");
    let mut args = queue_args();
    args.pr = None;
    assert_eq!(queue_check::run(&gh, &args).expect("runs"), super::EXIT_OK);
}

#[test]
fn queue_verdict_json_matches_the_line() {
    let blocked = queue_check::Verdict {
        number: 5,
        mergeable: false,
        reason: Some("draft".to_string()),
    };
    assert_eq!(blocked.line(), "#5 BLOCKED: draft");
    let json = serde_json::to_string(&blocked).expect("serializes");
    assert!(json.contains(r#""mergeable":false"#), "{json}");
    assert!(json.contains(r#""reason":"draft""#), "{json}");

    let clear = queue_check::Verdict {
        number: 6,
        mergeable: true,
        reason: None,
    };
    assert_eq!(clear.line(), "#6 MERGEABLE");
    assert!(
        !serde_json::to_string(&clear)
            .expect("serializes")
            .contains("reason")
    );
}

/// Run one PR through `queue-check` and return its stop reason, if any.
///
/// Why: the stop-condition table is the thing under test, and driving it
/// through the real `run` keeps the tests honest about the wiring too.
fn first_reason(view_json: &str) -> Option<String> {
    let gh = FakeGh::new()
        .on("branches/main/protection", REQUIRED)
        .on("pr view", view_json);
    let required = queue_check::required_contexts(&gh, "o/r", "main").expect("contexts");
    let verdicts = queue_check::verdicts(&gh, "o/r", &required, &[1]).expect("verdicts");
    verdicts.into_iter().next().and_then(|v| v.reason)
}
