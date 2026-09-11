//! Unit tests for `tm pr` (#6653).
//!
//! Every test drives the [`GhRunner`] / [`Preflight`] seams with a scripted
//! fake, so nothing here touches the network, a live `gh`, or a real PR.

use super::body::{self, ATTRIBUTION_FOOTER, FIELDS, Field, IssueLink};
use super::merge;
use super::metadata::{self, ChangedPaths, PrMetadata, RefsIssue, RefsLookup};
use super::open::{self, ChangelogVerdict, Preflight};
use super::queue_check;
use super::{GhRun, GhRunner, repo_slug};
use crate::cli::{PrMergeArgs, PrOpenArgs, PrQueueCheckArgs};
use trusty_mpm::core::component_labels::CrateOwnership;
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

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

/// A [`Preflight`] with every probe pinned.
struct FakePreflight {
    session: Option<String>,
    changelog: ChangelogVerdict,
    /// #7274: the diff the component labels are derived from.
    changed: Vec<String>,
    /// #7274: the workspace ownership those paths are looked up in.
    ownership: CrateOwnership,
    /// #7274 round 2: make `changed_paths` fail the way a broken `git` does.
    diff_fails: bool,
    /// #7275: a scratch registry, so no test writes a live cleanup entry that
    /// would arm the daemon's branch-deleting sweep against an invented repo.
    registry_dir: tempfile::TempDir,
    /// #7282: every revision `changed_paths` was asked to diff against.
    diff_heads: std::cell::RefCell<Vec<String>>,
}

impl FakePreflight {
    fn ok() -> Self {
        Self {
            session: Some("tm-test-01".to_string()),
            changelog: ChangelogVerdict::Pass,
            changed: Vec::new(),
            ownership: CrateOwnership::default(),
            diff_fails: false,
            registry_dir: tempfile::tempdir().expect("registry tempdir"),
            diff_heads: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// [`Self::ok`] with a diff and the ownership map to resolve it (#7274).
    fn with_diff(mut self, paths: &[&str]) -> Self {
        self.changed = paths.iter().map(|p| (*p).to_string()).collect();
        self.ownership = test_ownership();
        self
    }

    /// [`Self::ok`] whose diff read fails outright (#7274 round 2).
    fn with_unreadable_diff(mut self) -> Self {
        self.ownership = test_ownership();
        self.diff_fails = true;
        self
    }
}

impl Preflight for FakePreflight {
    fn session_name(&self) -> Option<String> {
        self.session.clone()
    }
    fn changelog_gate(&self, _base: &str) -> anyhow::Result<ChangelogVerdict> {
        Ok(self.changelog.clone())
    }
    fn changed_paths(&self, _base: &str, head: &str) -> anyhow::Result<Vec<String>> {
        anyhow::ensure!(!self.diff_fails, "git diff exploded");
        self.diff_heads.borrow_mut().push(head.to_string());
        Ok(self.changed.clone())
    }
    fn ownership(&self) -> CrateOwnership {
        self.ownership.clone()
    }
    fn cleanup_registry(&self) -> trusty_mpm::core::pr_cleanup::CleanupRegistry {
        trusty_mpm::core::pr_cleanup::CleanupRegistry::under_root(self.registry_dir.path())
    }
}

/// A two-crate workspace map, without a filesystem.
fn test_ownership() -> CrateOwnership {
    CrateOwnership::from_members([
        ("crates/trusty-mpm/".to_string(), "trusty-mpm".to_string()),
        (
            "crates/trusty-agents-common/".to_string(),
            "trusty-agents-common".to_string(),
        ),
    ])
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
        head: None,
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
fn an_issue_ref_at_line_start_is_not_a_heading() {
    // #7461: `#7459 flakes not seen.` was consumed as an ATX heading, which
    // ended the section it sat in and claimed no field — so every field after
    // it reported empty. CommonMark needs whitespace after the `#` run, and a
    // real `# Heading` (one `#`, then a space) must still claim its field.
    let body = full_body().replace(
        "## Risk\n\nsomething real.\n",
        "# Risk\n\n#7459 flakes not seen.\n#12 and a second ref, also prose.\n",
    );
    let report = body::validate(&body);
    assert!(report.missing.is_empty(), "{report:?}");
    assert!(report.empty.is_empty(), "{report:?}");
    assert!(
        report.supplied.contains(&Field::Risk),
        "the `# Risk` h1 must still be read as a heading: {report:?}"
    );

    // The ref lines are prose, so they travel into the body `gh` is handed.
    let args = open_args("/dev/null");
    let plan = open::plan(
        &args,
        &body,
        Some("tm-test-01"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect("a complete body plans");
    assert!(
        plan.body.contains("#7459 flakes not seen."),
        "the issue ref must survive into the PR body: {}",
        plan.body
    );
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

/// The session link Claude Code appends after the footer in a PR body.
const SESSION_LINK: &str = "https://claude.ai/code/session_0194bkFi1G1Wv3kh1bVbMw4q";

#[test]
fn footer_accepts_the_trailing_session_link() {
    // #7297: this IS the shape Claude Code's provisioned attribution tells a
    // session to write — footer, blank line, session link. Rejecting it sent
    // version-control agents to `gh pr create`, skipping this gate entirely.
    let body = format!("{}\n{SESSION_LINK}\n", full_body());
    let report = body::validate(&body);
    assert!(report.footer_ok, "{report:?}");
    assert!(report.failures().is_empty(), "{report:?}");

    // The labelled form the commit message uses is the same block.
    let labelled = format!("{}\nClaude-Session: {SESSION_LINK}\n", full_body());
    assert!(body::validate(&labelled).footer_ok);
}

#[test]
fn footer_accepts_a_session_link_before_the_footer() {
    // The workaround shape agents adopted while #7297 stood. Bodies already
    // written this way must keep passing.
    let mut s = String::new();
    for f in FIELDS {
        s.push_str(&format!("## {}\n\nsomething real.\n\n", f.heading()));
    }
    s.push_str(&format!("{SESSION_LINK}\n\n{ATTRIBUTION_FOOTER}\n"));
    let report = body::validate(&s);
    assert!(report.footer_ok, "{report:?}");
    assert!(report.failures().is_empty(), "{report:?}");
}

#[test]
fn footer_rejects_a_body_with_no_attribution_line() {
    // A session link alone is not attribution — the footer is still required.
    let body = full_body().replace(ATTRIBUTION_FOOTER, SESSION_LINK);
    assert!(!body.contains(ATTRIBUTION_FOOTER));
    let report = body::validate(&body);
    assert!(!report.footer_ok, "{report:?}");
    assert!(
        report
            .failures()
            .iter()
            .any(|f| f.contains("attribution footer")),
        "{report:?}"
    );
}

#[test]
fn footer_rejects_a_session_link_with_trailing_junk() {
    // `is_session_link` tested only the URL PREFIX, so a line that merely
    // STARTED with the link — link plus a sentence — closed the body as
    // attribution. The session id must be the whole rest of the line.
    let body = format!("{}\n{SESSION_LINK} some trailing junk\n", full_body());
    assert!(!body::validate(&body).footer_ok, "{body}");

    // The same holds for the labelled form.
    let labelled = format!(
        "{}\nClaude-Session: {SESSION_LINK} some trailing junk\n",
        full_body()
    );
    assert!(!body::validate(&labelled).footer_ok, "{labelled}");

    // A bare prefix with no session id is not a link either.
    let bare = format!("{}\nhttps://claude.ai/code/session_\n", full_body());
    assert!(!body::validate(&bare).footer_ok, "{bare}");
}

#[test]
fn footer_rejects_two_stacked_session_links() {
    // The block is the footer plus AT MOST ONE session link. Two stacked links
    // leave a link, not the footer, as the line before the last one.
    let body = format!("{}\n{SESSION_LINK}\n\n{SESSION_LINK}\n", full_body());
    let report = body::validate(&body);
    assert!(!report.footer_ok, "{report:?}");
    assert!(
        report
            .failures()
            .iter()
            .any(|f| f.contains("attribution footer")),
        "{report:?}"
    );
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
fn issue_link_lands_above_a_footer_a_session_link_trails() {
    // #7297 made the session link a legal LAST line, and the insertion point is
    // the footer rather than the end of the body — so the issue line must still
    // land above the footer and leave the link closing the body.
    let body = format!("{}\n{SESSION_LINK}\n", full_body());
    let out =
        body::apply_issue_link(&body, Some(7297), IssueLink::Refs).expect("refs link applies");

    let at = |needle: &str| {
        out.lines()
            .position(|l| l.trim() == needle)
            .unwrap_or_else(|| panic!("`{needle}` missing from:\n{out}"))
    };
    assert!(at("Refs #7297") < at(ATTRIBUTION_FOOTER), "{out}");
    assert!(at(ATTRIBUTION_FOOTER) < at(SESSION_LINK), "{out}");
    assert_eq!(
        out.lines().rev().find(|l| !l.trim().is_empty()),
        Some(SESSION_LINK),
        "{out}"
    );
    assert!(body::validate(&out).footer_ok, "{out}");
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
    assert!(err.contains("closes #3"), "{err}");
}

/// #6895: PR #6894's field 1 read `Fixes #6888`, which the old
/// `starts_with("closes ")` guard let through; squash-merge then closed #6888
/// while it still sat unverified at `status:merged`.
#[test]
fn issue_link_rejects_every_closing_keyword() {
    for keyword in [
        "Close", "Closes", "Closed", "Fix", "Fixes", "Fixed", "Resolve", "Resolves", "Resolved",
    ] {
        let body = full_body().replace("## Outcome\n", &format!("## Outcome\n\n{keyword} #6888\n"));
        let Err(err) = body::apply_issue_link(&body, Some(6888), IssueLink::Refs) else {
            panic!("`{keyword} #6888` must be refused");
        };
        assert!(err.contains(&keyword.to_ascii_lowercase()), "{err}");
    }
}

/// GitHub scans the whole body, so a keyword in a later field closes too.
#[test]
fn issue_link_rejects_a_closing_keyword_outside_field_one() {
    let body = full_body().replace("## Review\n", "## Review\n\nfixes #6888 as reviewed.\n");
    let err = body::apply_issue_link(&body, Some(6888), IssueLink::Refs)
        .expect_err("a keyword outside field 1 must be refused");
    assert!(err.contains("fixes #6888"), "{err}");
}

/// #5389: "Does NOT close #5357" still closed #5357 — GitHub ignores negation.
#[test]
fn issue_link_rejects_a_negated_closing_keyword() {
    let body = full_body().replace("## Risk\n", "## Risk\n\nDoes NOT close #5357.\n");
    let err = body::apply_issue_link(&body, Some(1), IssueLink::Refs)
        .expect_err("a negated keyword must be refused");
    assert!(err.contains("close #5357"), "{err}");
}

#[test]
fn issue_link_allows_prose_that_only_looks_like_a_keyword() {
    let body = full_body().replace(
        "## Changes\n",
        "## Changes\n\nprefix #12 stays; the fix for #13 stays; Refs #14 stays.\n",
    );
    body::apply_issue_link(&body, Some(14), IssueLink::Refs)
        .expect("prose without a keyword+reference pair must pass");
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
        &ResolvedTicketing::default(),
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
fn open_labels_come_from_the_policy_table() {
    // #6918: `tm pr open` used to spell `trusty-mpm` and `ws/<session>`
    // itself. Its own `format!("ws/{}")` skipped the 50-char truncation
    // `policy_labels::workstream_label` applies, so a session name long enough
    // to overflow GitHub's cap produced a PR label that did not match the one
    // `tm issue seed-labels` and session launch had actually created.
    let long = "a".repeat(80);
    let args = open_args("/dev/null");
    let plan = open::plan(
        &args,
        &full_body(),
        Some(&long),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect("a complete body plans");
    let expected = trusty_mpm::core::policy_labels::workstream_label(&long)
        .expect("a non-blank name derives a label");
    assert_eq!(plan.workstream_label, expected.name);
    assert!(
        plan.workstream_label.len() <= trusty_mpm::core::policy_labels::GITHUB_LABEL_MAX_LEN,
        "{}",
        plan.workstream_label
    );
    assert!(
        plan.argv
            .iter()
            .any(|a| a == trusty_mpm::core::policy_labels::CONVENTION_LABEL),
        "{:?}",
        plan.argv
    );
}

#[test]
fn open_assignee_comes_from_the_ticketing_block() {
    // #6918: `--assignee @me` was hardcoded; it is now the resolved
    // `agents.ticketing.default_assignee`.
    let ticketing = ResolvedTicketing::default().with_default_assignee("bobmatnyc");
    let args = open_args("/dev/null");
    let plan = open::plan(
        &args,
        &full_body(),
        Some("tm-test-01"),
        ChangelogVerdict::Pass,
        &ticketing,
    )
    .expect("a complete body plans");
    assert!(
        plan.argv.join(" ").contains("--assignee bobmatnyc"),
        "{:?}",
        plan.argv
    );
}

#[test]
fn open_reports_each_missing_field() {
    for f in FIELDS {
        let args = open_args("/dev/null");
        let failures = open::plan(
            &args,
            &body_without(f),
            Some("s"),
            ChangelogVerdict::Pass,
            &ResolvedTicketing::default(),
        )
        .expect_err("a missing field must fail the plan");
        assert_eq!(failures.len(), 1, "{f:?}: {failures:?}");
        assert!(failures[0].contains(f.heading()), "{failures:?}");
    }
}

#[test]
fn open_rejects_bad_footer() {
    let args = open_args("/dev/null");
    let body = format!("{}\nafterword\n", full_body());
    let failures = open::plan(
        &args,
        &body,
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect_err("a footer that is not last must fail");
    assert!(
        failures.iter().any(|f| f.contains("attribution footer")),
        "{failures:?}"
    );
}

#[test]
fn open_requires_a_session_name() {
    let args = open_args("/dev/null");
    let failures = open::plan(
        &args,
        &full_body(),
        None,
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
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
    let failures = open::plan(
        &args,
        &full_body(),
        Some("s"),
        verdict,
        &ResolvedTicketing::default(),
    )
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
    let plan = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::Skipped,
        &ResolvedTicketing::default(),
    )
    .expect("docs-only plans without the gate");
    assert!(plan.argv.join(" ").contains("pr create"));
}

// ── open: the explicit head branch (#7282) ───────────────────────────────

/// The value `flag` was given in `argv`, or `None` when the flag is absent.
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let at = argv.iter().position(|a| a == flag)?;
    argv.get(at + 1).map(String::as_str)
}

/// Why: `gh pr create` reads the head from the checkout's CURRENT branch. The
/// session-pause publisher builds its branch with git plumbing and never checks
/// it out, so `gh` read `main`, found nothing to open, and aborted with
/// "you must first push the current branch to a remote, or use the --head
/// flag" (#7282 round 4). Naming the head is what makes the caller's checkout
/// state irrelevant.
/// Test target: `plan`'s argv assembly.
#[test]
fn open_argv_carries_the_explicit_head_branch() {
    let mut args = open_args("/dev/null");
    args.head = Some("chore/sessions-abc-20260909-233853".to_string());
    // #7282 round 5: `--head` only plans when paired with `--docs-only`.
    args.docs_only = true;
    let plan = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::Skipped,
        &ResolvedTicketing::default(),
    )
    .expect("a named head plans");

    assert_eq!(
        flag_value(&plan.argv, "--head"),
        Some("chore/sessions-abc-20260909-233853"),
        "{:?}",
        plan.argv
    );
    // The base is still named too, so neither end of the PR is inferred.
    assert_eq!(flag_value(&plan.argv, "--base"), Some("main"));
}

/// Why: no `--head` must leave `gh` inferring the head exactly as before, so
/// every existing caller keeps its behavior.
#[test]
fn open_argv_omits_head_when_none_is_named() {
    let args = open_args("/dev/null");
    let plan = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect("plans without a head");
    assert!(!plan.argv.iter().any(|a| a == "--head"), "{:?}", plan.argv);
}

/// Why: `--head ""` reaching `gh` as an empty head produces the same confusing
/// current-branch message this fix exists to end, so a blank reads as absent.
#[test]
fn open_head_is_ignored_when_blank() {
    let mut args = open_args("/dev/null");
    args.head = Some("   ".to_string());
    let plan = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect("a blank head plans");
    assert!(!plan.argv.iter().any(|a| a == "--head"), "{:?}", plan.argv);
}

/// Why: the component labels come from `origin/<base>...<rev>`. Asking about
/// `HEAD` when the PR opens from another branch describes the checkout, not the
/// PR — an empty diff and no labels for every `--head` caller.
/// Test target: `diff_head`, through `run`.
#[test]
fn open_head_drives_the_preflight_diff_revision() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.head = Some("chore/sessions-abc".to_string());
    // #7282 round 5: `--head` only reaches `gh` alongside `--docs-only`.
    args.docs_only = true;
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("pr edit", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
    open::run(&gh, &args, &pre).expect("create succeeds");
    assert_eq!(
        pre.diff_heads.borrow().as_slice(),
        ["chore/sessions-abc".to_string()]
    );
}

/// Why: `scripts/check_changelog_fragment.sh` accepts only `--base`, `--staged`
/// and `--file`, so the gate `tm pr open` runs can diff nothing but
/// `origin/<base>...HEAD`. A `--head` caller standing on another branch would
/// have that gate judge the checkout instead of the PR, and a source PR with no
/// fragment would pass it — the changelog gate silently evaluating the wrong
/// ref (#7282 round 5, code-critic HIGH). The refusal has to name the
/// obligation, because the caller's next move is either `--docs-only` or a real
/// checkout of the head.
/// Test target: `head_docs_only_conflict`, through `plan`.
#[test]
fn open_head_without_docs_only_is_refused() {
    let mut args = open_args("/dev/null");
    args.head = Some("chore/sessions-abc".to_string());
    let failures = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect_err("--head without --docs-only must not plan");

    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("--head requires --docs-only"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("check_changelog_fragment.sh"),
        "the refusal must name what cannot be checked: {failures:?}"
    );
    assert!(
        failures[0].contains("chore/sessions-abc"),
        "the refusal must name the head it is about: {failures:?}"
    );
}

/// Why: the refusal is worth nothing if the PR opens anyway — `gh` must never
/// be spawned, and the exit code must be the check-failed one every other
/// pre-flight failure uses.
/// Test target: `run`'s failure path with a `--head` and no `--docs-only`.
#[test]
fn open_head_without_docs_only_never_calls_gh() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.head = Some("chore/sessions-abc".to_string());
    let gh = FakeGh::new();
    let code = open::run(&gh, &args, &FakePreflight::ok()).expect("a failed check is not an error");

    assert_eq!(code, 2);
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
}

/// Why: `--docs-only` is the one case where the gate's verdict cannot be wrong,
/// because it is skipped — so the pair must still plan. This is the pause
/// publisher's own invocation, and refusing it would break every pause.
/// Test target: `head_docs_only_conflict`, permitting arm.
#[test]
fn open_head_with_docs_only_plans() {
    let mut args = open_args("/dev/null");
    args.head = Some("chore/sessions-abc".to_string());
    args.docs_only = true;
    let plan = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::Skipped,
        &ResolvedTicketing::default(),
    )
    .expect("--head with --docs-only plans");

    assert_eq!(flag_value(&plan.argv, "--head"), Some("chore/sessions-abc"));
}

/// Why: without `--head` the diff must still be the checkout's `HEAD`.
#[test]
fn open_without_head_diffs_the_checkouts_head() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("pr edit", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
    open::run(&gh, &args, &pre).expect("create succeeds");
    assert_eq!(pre.diff_heads.borrow().as_slice(), ["HEAD".to_string()]);
}

#[test]
fn open_plan_reports_every_failure_at_once() {
    let args = open_args("/dev/null");
    let body = body_without(Field::Risk).replace("## Tests\n\nsomething real.\n", "## Tests\n\n");
    let failures = open::plan(
        &args,
        &body,
        None,
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
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

// ── PR labels / project / milestone standard (#7274) ─────────────────────

/// The `gh issue view --json` payload for an issue with both fields set.
const ISSUE_JSON: &str = r#"{"number":7274,"milestone":{"title":"mpm 1.6"},
    "projectItems":[{"title":"trusty-mpm"},{"title":"Harness"}],
    "labels":[],"comments":[],"state":"OPEN"}"#;

fn refs_issue() -> RefsIssue {
    RefsIssue {
        number: 7274,
        milestone: Some("mpm 1.6".to_string()),
        projects: vec!["trusty-mpm".to_string()],
    }
}

#[test]
fn metadata_inherits_milestone_and_projects() {
    let meta = metadata::plan(
        RefsLookup::Found(&refs_issue()),
        ChangedPaths::Read(&["crates/trusty-mpm/src/lib.rs"]),
        &test_ownership(),
    );
    assert_eq!(meta.labels, vec!["trusty-mpm".to_string()]);
    assert_eq!(meta.milestone.as_deref(), Some("mpm 1.6"));
    assert_eq!(meta.projects, vec!["trusty-mpm".to_string()]);
    assert!(
        meta.notes.is_empty(),
        "nothing was missing: {:?}",
        meta.notes
    );
}

#[test]
fn metadata_without_refs_applies_nothing() {
    let meta = metadata::plan(
        RefsLookup::Absent,
        ChangedPaths::Read(&["docs/specs/DOC-65.md"]),
        &test_ownership(),
    );
    assert!(meta.is_empty(), "{meta:?}");
    let notes = meta.notes.join("\n");
    assert!(
        notes.contains("no component label: no workspace crate owns the changed paths"),
        "{notes}"
    );
    assert!(notes.contains("no `Refs #N`"), "{notes}");
}

/// #7274 round 2: a diff `git` refused to produce is not a diff no crate owns.
#[test]
fn metadata_notes_an_unreadable_diff() {
    let meta = metadata::plan(
        RefsLookup::Found(&refs_issue()),
        ChangedPaths::<&str>::Unreadable,
        &test_ownership(),
    );
    let notes = meta.notes.join("\n");
    assert!(
        notes.contains("no component label: the diff could not be read"),
        "{notes}"
    );
    assert!(
        !notes.contains("no workspace crate owns"),
        "a failed read must not blame the workspace: {notes}"
    );
    assert!(meta.labels.is_empty());
    // The other half is unaffected — the issue was still read.
    assert_eq!(meta.milestone.as_deref(), Some("mpm 1.6"));
}

/// #7274 round 2: a `Refs` line whose issue could not be read is not a body
/// with no `Refs` line.
#[test]
fn metadata_notes_an_unreadable_refs_issue() {
    let meta = metadata::plan(
        RefsLookup::Unreadable(7274),
        ChangedPaths::Read(&["crates/trusty-mpm/src/lib.rs"]),
        &test_ownership(),
    );
    let notes = meta.notes.join("\n");
    assert!(
        notes.contains("no project or milestone: issue #7274 could not be read"),
        "{notes}"
    );
    assert!(
        !notes.contains("carries no `Refs #N`"),
        "the body carried one; that is what triggered the lookup: {notes}"
    );
    assert!(meta.milestone.is_none());
    assert!(meta.projects.is_empty());
    // The component label is unaffected — the diff was still read.
    assert_eq!(meta.labels, vec!["trusty-mpm".to_string()]);
}

#[test]
fn metadata_multi_crate_diff() {
    let meta = metadata::plan(
        RefsLookup::Found(&refs_issue()),
        ChangedPaths::Read(&[
            "crates/trusty-agents-common/src/assets/agents/version-control.md",
            "crates/trusty-mpm/src/bin/tm/commands/pr/open.rs",
            "crates/trusty-mpm/src/core/component_labels.rs",
            "docs/specs/DOC-65-universal-framework-agents.md",
        ]),
        &test_ownership(),
    );
    assert_eq!(
        meta.labels,
        vec!["trusty-agents-common".to_string(), "trusty-mpm".to_string()],
        "one label per crate the diff touches, docs contributing none"
    );
}

#[test]
fn metadata_notes_an_issue_with_no_milestone() {
    let bare = RefsIssue {
        number: 99,
        milestone: None,
        projects: Vec::new(),
    };
    let meta = metadata::plan(
        RefsLookup::Found(&bare),
        ChangedPaths::Read(&["crates/trusty-mpm/a.rs"]),
        &test_ownership(),
    );
    let notes = meta.notes.join("\n");
    assert!(
        notes.contains("no milestone: issue #99 carries none"),
        "{notes}"
    );
    assert!(
        notes.contains("no project: issue #99 joins none"),
        "{notes}"
    );
    assert!(meta.milestone.is_none());
    assert!(meta.projects.is_empty());
}

#[test]
fn metadata_parses_a_gh_issue_view_payload() {
    let facts = serde_json::from_str(ISSUE_JSON).expect("the fixture parses");
    let issue = RefsIssue::from_facts(7274, &facts);
    assert_eq!(issue.milestone.as_deref(), Some("mpm 1.6"));
    assert_eq!(
        issue.projects,
        vec!["trusty-mpm".to_string(), "Harness".to_string()]
    );
}

#[test]
fn metadata_edit_argv_carries_every_field() {
    let meta = PrMetadata {
        labels: vec!["trusty-mpm".to_string()],
        milestone: Some("mpm 1.6".to_string()),
        projects: vec!["trusty-mpm".to_string()],
        notes: Vec::new(),
    };
    let argv = metadata::edit_argv("4242", Some("o/r"), &meta).join(" ");
    assert!(argv.starts_with("pr edit 4242 --repo o/r"), "{argv}");
    assert!(argv.contains("--add-label trusty-mpm"), "{argv}");
    assert!(argv.contains("--milestone mpm 1.6"), "{argv}");
    assert!(argv.contains("--add-project trusty-mpm"), "{argv}");
}

#[test]
fn metadata_finds_the_first_refs() {
    let body = "## Outcome\n\nRefs #7274\n\nRefs #9999\n";
    assert_eq!(metadata::first_refs_issue(body), Some(7274));
}

#[test]
fn metadata_finds_a_qualified_refs() {
    assert_eq!(
        metadata::first_refs_issue("Refs bobmatnyc/trusty-tools#7274\n"),
        Some(7274)
    );
}

#[test]
fn metadata_ignores_refs_mid_sentence() {
    assert_eq!(
        metadata::first_refs_issue("This one refs #12 in passing.\nRefs #34\n"),
        Some(34),
        "only a line that STARTS with the keyword is the link line"
    );
    assert_eq!(metadata::first_refs_issue("## Outcome\n\ntext\n"), None);
}

/// #7274 round 2: a body that quotes the convention before stating its own
/// link must inherit from its own link, not from the sample.
#[test]
fn metadata_ignores_refs_inside_a_fence() {
    let body =
        "## Outcome\n\nEvery fix PR reads:\n\n```\nfix(x): y\n\nRefs #999\n```\n\nRefs #7274\n";
    assert_eq!(
        metadata::first_refs_issue(body),
        Some(7274),
        "a fenced sample is not the PR's own link"
    );
    // An info string opens a fence the same way a bare one does.
    let tagged = "```text\nRefs #999\n```\n\nRefs #7274\n";
    assert_eq!(metadata::first_refs_issue(tagged), Some(7274));
    // A body whose only `Refs` is fenced has no link at all.
    assert_eq!(metadata::first_refs_issue("```\nRefs #999\n```\n"), None);
}

#[test]
fn open_applies_pr_metadata() {
    let mut body = full_body();
    body = body.replace(
        ATTRIBUTION_FOOTER,
        &format!("Refs #7274\n\n{ATTRIBUTION_FOOTER}"),
    );
    let (_d, path) = scratch_body(&body);
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("issue view 7274", ISSUE_JSON)
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_diff(&[
        "crates/trusty-mpm/src/bin/tm/commands/pr/open.rs",
        "crates/trusty-agents-common/src/assets/agents/version-control.md",
    ]);
    let code = open::run(&gh, &args, &pre).expect("create succeeds");
    assert_eq!(code, super::EXIT_OK);
    let edit = gh
        .calls()
        .into_iter()
        .map(|c| c.join(" "))
        .find(|c| c.starts_with("pr edit"))
        .expect("a `gh pr edit` ran");
    assert!(edit.contains("--add-label trusty-mpm"), "{edit}");
    assert!(edit.contains("--add-label trusty-agents-common"), "{edit}");
    assert!(edit.contains("--milestone mpm 1.6"), "{edit}");
    assert!(edit.contains("--add-project trusty-mpm"), "{edit}");
    assert!(edit.contains("--add-project Harness"), "{edit}");
}

#[test]
fn open_without_refs_says_so() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new().on("pr create", "https://github.com/o/r/pull/7\n");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
    let code = open::run(&gh, &args, &pre).expect("create succeeds");
    assert_eq!(code, super::EXIT_OK);
    // The component label still lands; only the inherited half is skipped.
    let edit = gh
        .calls()
        .into_iter()
        .map(|c| c.join(" "))
        .find(|c| c.starts_with("pr edit"))
        .expect("a `gh pr edit` ran for the component label");
    assert!(edit.contains("--add-label trusty-mpm"), "{edit}");
    assert!(!edit.contains("--milestone"), "{edit}");
    assert!(
        !gh.calls()
            .iter()
            .any(|c| c.join(" ").contains("issue view")),
        "no `Refs #N` means no issue read at all"
    );
}

#[test]
fn open_survives_a_failed_metadata_edit() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/7\n")
        .on_fail("pr edit", "label not found");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
    let code = open::run(&gh, &args, &pre).expect("a failed edit is not an error");
    assert_eq!(
        code,
        super::EXIT_OK,
        "the PR already exists; the metadata apply is best-effort"
    );
}

/// #7274 round 2: the diff-read failure arm reaches `plan`. The note itself is
/// stdout, which this harness cannot capture, so the observable proof is that
/// the edit carries the inherited half and no component label.
#[test]
fn open_notes_an_unreadable_diff() {
    let mut body = full_body();
    body = body.replace(
        ATTRIBUTION_FOOTER,
        &format!("Refs #7274\n\n{ATTRIBUTION_FOOTER}"),
    );
    let (_d, path) = scratch_body(&body);
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("issue view 7274", ISSUE_JSON)
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_unreadable_diff();
    let code = open::run(&gh, &args, &pre).expect("an unreadable diff is not an error");
    assert_eq!(code, super::EXIT_OK);
    let edit = gh
        .calls()
        .into_iter()
        .map(|c| c.join(" "))
        .find(|c| c.starts_with("pr edit"))
        .expect("the inherited half still applies");
    assert!(!edit.contains("--add-label"), "{edit}");
    assert!(edit.contains("--milestone mpm 1.6"), "{edit}");
}

/// #7274 round 2: a `Refs` line whose `gh issue view` fails still opens the PR
/// and still applies the component label the diff earned.
#[test]
fn open_notes_an_unreadable_refs_issue() {
    let mut body = full_body();
    body = body.replace(
        ATTRIBUTION_FOOTER,
        &format!("Refs #7274\n\n{ATTRIBUTION_FOOTER}"),
    );
    let (_d, path) = scratch_body(&body);
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on_fail("issue view 7274", "GraphQL: Could not resolve to an Issue")
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
    let code = open::run(&gh, &args, &pre).expect("an unreadable issue is not an error");
    assert_eq!(code, super::EXIT_OK);
    assert!(
        gh.calls()
            .iter()
            .any(|c| c.join(" ").contains("issue view 7274")),
        "the body carried a `Refs`, so the lookup was attempted"
    );
    let edit = gh
        .calls()
        .into_iter()
        .map(|c| c.join(" "))
        .find(|c| c.starts_with("pr edit"))
        .expect("the component label still applies");
    assert!(edit.contains("--add-label trusty-mpm"), "{edit}");
    assert!(!edit.contains("--milestone"), "{edit}");
    assert!(!edit.contains("--add-project"), "{edit}");
}

// ── post-merge cleanup registry (#7275) ──────────────────────────────────

/// REGRESSION (#7275): a PR `tm pr open` created is recorded for post-merge
/// cleanup, keyed by the repo and number in `gh`'s own URL — with no second
/// `gh` call, so the record cannot name a different remote than the push used.
#[test]
fn open_records_the_new_pr_for_cleanup() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new().on(
        "pr create",
        "https://github.com/bobmatnyc/trusty-tools/pull/7275\n",
    );
    let pre = FakePreflight::ok();
    let code = open::run(&gh, &args, &pre).expect("create succeeds");
    assert_eq!(code, super::EXIT_OK);
    assert_eq!(
        gh.calls().len(),
        1,
        "recording must not cost a second gh call"
    );

    let entries = pre.cleanup_registry().entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].pr, 7275);
    assert_eq!(entries[0].repo, "bobmatnyc/trusty-tools");
    assert!(
        entries[0].pending(),
        "a freshly opened PR has not been cleaned up"
    );
}

/// REGRESSION (#7275): a URL `gh` printed in a shape this cannot parse records
/// nothing, rather than an entry naming a repository it guessed at.
#[test]
fn open_records_nothing_for_an_unparsable_url() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new().on("pr create", "created: see the web UI\n");
    let pre = FakePreflight::ok();
    open::run(&gh, &args, &pre).expect("create succeeds");
    assert!(
        pre.cleanup_registry().entries().is_empty(),
        "an unparsable URL must not become a guessed registry entry"
    );
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

// ── tm pr merge (#6808) ──────────────────────────────────────────────────

/// Default `tm pr merge` flags for PR 42.
fn merge_args() -> PrMergeArgs {
    PrMergeArgs {
        pr: 42,
        auto: false,
        no_delete_branch: false,
        repo: None,
    }
}

/// A `gh pr view` payload for PR 42, with `patch`'s keys overriding the
/// clean-and-approved defaults.
fn merge_view(body: &str, patch: serde_json::Value) -> merge::MergeView {
    let mut v = serde_json::json!({
        "number": 42,
        "title": "feat(x): a thing",
        "body": body,
        "isDraft": false,
        "labels": [],
        "reviewDecision": "APPROVED",
        "mergeStateStatus": "CLEAN",
        "mergeable": "MERGEABLE",
        "headRefName": "feat/6808-x",
    });
    let obj = v.as_object_mut().expect("object");
    for (k, val) in patch.as_object().expect("patch is an object") {
        obj.insert(k.clone(), val.clone());
    }
    serde_json::from_value(v).expect("view parses")
}

/// The decision `tm pr merge` would reach for this view.
fn merge_decision(view: &merge::MergeView) -> merge::Decision {
    let failures = body::validate(&view.body).failures();
    merge::decide(view, &failures)
}

/// The refusal reason, or `None` when the decision was to merge.
fn merge_refusal(view: &merge::MergeView) -> Option<String> {
    match merge_decision(view) {
        merge::Decision::Merge => None,
        merge::Decision::Refuse(r) => Some(r),
    }
}

/// [`full_body`] with the attribution footer stripped off the end.
fn body_without_footer() -> String {
    let mut s = String::new();
    for f in FIELDS {
        s.push_str(&format!("## {}\n\nsomething real.\n\n", f.heading()));
    }
    s
}

#[test]
fn merge_valid_body_merges() {
    let view = merge_view(&full_body(), serde_json::json!({}));
    assert_eq!(merge_decision(&view), merge::Decision::Merge);
}

#[test]
fn merge_refuses_missing_footer() {
    let view = merge_view(&body_without_footer(), serde_json::json!({}));
    let reason = merge_refusal(&view).expect("refused");
    assert!(reason.contains("attribution footer"), "{reason}");
    assert!(reason.contains("tm pr open"), "{reason}");
}

#[test]
fn merge_refuses_draft() {
    let view = merge_view(&full_body(), serde_json::json!({"isDraft": true}));
    assert_eq!(merge_refusal(&view).as_deref(), Some("the PR is a draft"));
}

#[test]
fn merge_refuses_do_not_merge_label_any_case() {
    for name in ["do-not-merge", "DO-NOT-MERGE", "Do-Not-Merge"] {
        let view = merge_view(
            &full_body(),
            serde_json::json!({"labels": [{"name": name}]}),
        );
        let reason = merge_refusal(&view).unwrap_or_else(|| panic!("`{name}` must refuse"));
        assert!(reason.contains(name), "{reason}");
    }
}

#[test]
fn merge_refuses_changes_requested() {
    let view = merge_view(
        &full_body(),
        serde_json::json!({"reviewDecision": "CHANGES_REQUESTED"}),
    );
    let reason = merge_refusal(&view).expect("refused");
    assert!(reason.contains("CHANGES_REQUESTED"), "{reason}");
}

#[test]
fn merge_behind_is_not_a_refusal() {
    // Repo rule: a BEHIND branch merges fine, so only CONFLICTING stops here.
    let view = merge_view(
        &full_body(),
        serde_json::json!({"mergeStateStatus": "BEHIND"}),
    );
    assert_eq!(merge_decision(&view), merge::Decision::Merge);
}

/// `CONFLICTING` is a `MergeableState` value, never a `MergeStateStatus` one
/// (#6808) — reading it off `mergeStateStatus` let every conflicted PR through.
#[test]
fn merge_refuses_conflicting_with_update_branch_hint() {
    let view = merge_view(
        &full_body(),
        serde_json::json!({"mergeable": "CONFLICTING"}),
    );
    let reason = merge_refusal(&view).expect("refused");
    assert!(reason.contains("mergeable CONFLICTING"), "{reason}");
    assert!(reason.contains("gh pr update-branch 42"), "{reason}");
}

/// `DIRTY` is how the same conflict spells itself in `mergeStateStatus`.
#[test]
fn merge_refuses_dirty_merge_state_with_update_branch_hint() {
    let view = merge_view(
        &full_body(),
        serde_json::json!({"mergeStateStatus": "DIRTY", "mergeable": "UNKNOWN"}),
    );
    let reason = merge_refusal(&view).expect("refused");
    assert!(reason.contains("mergeStateStatus DIRTY"), "{reason}");
    assert!(reason.contains("gh pr update-branch 42"), "{reason}");
}

/// Every other merge state and review decision is `gh pr merge`'s to judge —
/// which is what makes `--auto` on a still-checking PR the intended path.
#[test]
fn merge_other_merge_states_fall_through_to_gh() {
    for state in ["BLOCKED", "UNSTABLE", "HAS_HOOKS", "UNKNOWN"] {
        let view = merge_view(&full_body(), serde_json::json!({"mergeStateStatus": state}));
        assert_eq!(
            merge_decision(&view),
            merge::Decision::Merge,
            "mergeStateStatus {state} must not refuse"
        );
    }
    for decision in [
        serde_json::json!(null),
        serde_json::json!("REVIEW_REQUIRED"),
    ] {
        let view = merge_view(
            &full_body(),
            serde_json::json!({"reviewDecision": decision}),
        );
        assert_eq!(merge_decision(&view), merge::Decision::Merge);
    }
}

/// A `gh pr view` stdout payload for PR 42.
fn merge_view_json(body: &str, patch: serde_json::Value) -> String {
    let mut v = serde_json::json!({
        "number": 42,
        "title": "feat(x): a thing",
        "body": body,
        "isDraft": false,
        "labels": [],
        "reviewDecision": "APPROVED",
        "mergeStateStatus": "CLEAN",
        "mergeable": "MERGEABLE",
        "headRefName": "feat/6808-x",
    });
    let obj = v.as_object_mut().expect("object");
    for (k, val) in patch.as_object().expect("patch is an object") {
        obj.insert(k.clone(), val.clone());
    }
    v.to_string()
}

#[test]
fn merge_argv_carries_squash_delete_and_body_file() {
    let gh = FakeGh::new()
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on("pr merge", "");
    assert_eq!(
        merge::run(&gh, &merge_args()).expect("runs"),
        super::EXIT_OK
    );

    let calls = gh.calls();
    let merge_call = calls
        .iter()
        .find(|a| {
            a.first().map(String::as_str) == Some("pr")
                && a.get(1).map(String::as_str) == Some("merge")
        })
        .expect("gh pr merge was called");
    let joined = merge_call.join(" ");
    assert!(
        joined.starts_with("pr merge 42 --squash --delete-branch"),
        "{joined}"
    );
    assert!(!merge_call.contains(&"--auto".to_string()), "{joined}");

    let subject = merge_call
        .iter()
        .position(|a| a == "--subject")
        .map(|i| merge_call[i + 1].clone())
        .expect("--subject supplied");
    assert_eq!(subject, "feat(x): a thing (#42)");

    let body_file = merge_call
        .iter()
        .position(|a| a == "--body-file")
        .map(|i| merge_call[i + 1].clone())
        .expect("--body-file supplied");
    assert!(!body_file.is_empty(), "--body-file needs a path");
}

#[test]
fn merge_argv_honours_auto_and_no_delete_branch() {
    let gh = FakeGh::new()
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on("pr merge", "");
    let args = PrMergeArgs {
        pr: 42,
        auto: true,
        no_delete_branch: true,
        repo: Some("o/r".to_string()),
    };
    assert_eq!(merge::run(&gh, &args).expect("runs"), super::EXIT_OK);

    let calls = gh.calls();
    let merge_call = calls.last().expect("a call");
    let joined = merge_call.join(" ");
    assert!(joined.contains("--auto"), "{joined}");
    assert!(joined.contains("--repo o/r"), "{joined}");
    assert!(!joined.contains("--delete-branch"), "{joined}");
}

#[test]
fn merge_refuses_without_calling_gh_merge() {
    let gh = FakeGh::new().on(
        "pr view",
        &merge_view_json(&full_body(), serde_json::json!({"isDraft": true})),
    );
    assert_eq!(
        merge::run(&gh, &merge_args()).expect("runs"),
        super::EXIT_BLOCKED
    );
    assert!(
        gh.calls()
            .iter()
            .all(|a| a.get(1).map(String::as_str) != Some("merge")),
        "gh pr merge must not be called on a refusal: {:?}",
        gh.calls()
    );
}

/// A failed `gh pr view` is an error, not a silent merge (#6808).
#[test]
fn merge_errors_when_gh_view_fails() {
    let gh = FakeGh::new().on_fail("pr view", "could not resolve to a PullRequest");
    let err = merge::run(&gh, &merge_args()).expect_err("must not swallow the failure");
    assert!(format!("{err:#}").contains("could not resolve"), "{err:#}");
    assert!(
        gh.calls()
            .iter()
            .all(|a| a.get(1).map(String::as_str) != Some("merge")),
        "gh pr merge must not run after a failed view: {:?}",
        gh.calls()
    );
}

/// A failed `gh pr merge` is an error, never a reported success (#6808).
#[test]
fn merge_errors_when_gh_merge_fails() {
    let gh = FakeGh::new()
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on_fail("pr merge", "Pull request is not mergeable");
    let err = merge::run(&gh, &merge_args()).expect_err("must not swallow the failure");
    assert!(format!("{err:#}").contains("not mergeable"), "{err:#}");
}
