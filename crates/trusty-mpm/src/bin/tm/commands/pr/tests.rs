//! Unit tests for `tm pr` (#6653).
//!
//! Every test drives the [`GhRunner`] / [`Preflight`] seams with a scripted
//! fake, so nothing here touches the network, a live `gh`, or a real PR.

use super::body::{self, ATTRIBUTION_FOOTER, FIELDS, Field, IssueLink};
use super::merge;
use super::metadata::{self, ChangedPaths, PrMetadata, RefKind, RefsIssue, RefsLookup};
use super::metadata_apply::{self, ApplyOutcome};
use super::open::{self, ChangelogVerdict, Preflight};
use super::queue_check;
use super::{GhRun, GhRunner, repo_slug};
use crate::cli::{PrMergeArgs, PrOpenArgs, PrQueueCheckArgs};
use trusty_mpm::core::component_labels::CrateOwnership;
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

// ── fakes ────────────────────────────────────────────────────────────────

/// How a [`FakeGh`] route is matched against the joined argv.
///
/// Why (#7646): the per-field retry's argv is a PREFIX of the combined edit's,
/// so a substring route cannot tell "apply the label alone" from "apply
/// everything at once" — and a test of the retry has to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteMatch {
    /// The needle appears anywhere in the joined argv.
    Contains,
    /// The needle IS the joined argv.
    Exact,
}

/// A `gh` seam that answers by argv match, in registration order.
struct FakeGh {
    /// (how to match, needle, response).
    routes: Vec<(RouteMatch, String, GhRun)>,
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
            RouteMatch::Contains,
            needle.to_string(),
            GhRun {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        ));
        self
    }

    /// A successful route matching the WHOLE argv (#7646).
    fn on_exact(mut self, argv: &str, stdout: &str) -> Self {
        self.routes.push((
            RouteMatch::Exact,
            argv.to_string(),
            GhRun {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        ));
        self
    }

    /// A failing route matching the WHOLE argv (#7945).
    ///
    /// Why: the post-merge confirmation read is itself a `gh pr view`, so a
    /// test of the case where only THAT read fails cannot express itself with a
    /// substring route.
    fn on_exact_fail(mut self, argv: &str, stderr: &str) -> Self {
        self.routes.push((
            RouteMatch::Exact,
            argv.to_string(),
            GhRun {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        ));
        self
    }

    fn on_fail(mut self, needle: &str, stderr: &str) -> Self {
        self.routes.push((
            RouteMatch::Contains,
            needle.to_string(),
            GhRun {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        ));
        self
    }

    /// A route that fails while still printing to stdout (#7869).
    ///
    /// Why: that is exactly what `gh pr create` does when it creates the PR and
    /// then 502s on the follow-up call that applies the assignee and labels —
    /// the URL is on stdout and the exit status is non-zero.
    fn on_partial(mut self, needle: &str, stdout: &str, stderr: &str) -> Self {
        self.routes.push((
            RouteMatch::Contains,
            needle.to_string(),
            GhRun {
                success: false,
                stdout: stdout.to_string(),
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
        for (how, needle, run) in &self.routes {
            let hit = match how {
                RouteMatch::Contains => joined.contains(needle.as_str()),
                RouteMatch::Exact => joined == *needle,
            };
            if hit {
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
    /// #7747: the branch name this fake's checkout stands on, if any. A
    /// `changelog_gate` asked about any other head answers `HeadElsewhere`,
    /// the way `RealPreflight` does when the ref resolves elsewhere.
    checkout_head: Option<String>,
    /// #7748: where the local `origin/<base>` stands against the remote.
    base_freshness: trusty_mpm::core::base_ref_freshness::BaseFreshness,
    /// #7748 round 2: every write permission the probe was asked with.
    freshness_modes: std::cell::RefCell<Vec<trusty_mpm::core::base_ref_freshness::RefreshMode>>,
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
            checkout_head: None,
            // #7748: a current base is the ordinary case every other test wants.
            base_freshness: trusty_mpm::core::base_ref_freshness::BaseFreshness::Fresh {
                sha: "aaa111".to_string(),
            },
            freshness_modes: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// [`Self::ok`] whose local diff base is behind the remote (#7748).
    fn with_stale_base(mut self) -> Self {
        self.base_freshness = trusty_mpm::core::base_ref_freshness::BaseFreshness::Stale {
            local: "old111".to_string(),
            remote: "new222".to_string(),
        };
        self
    }

    /// [`Self::ok`] standing on `branch`, so `--head <branch>` is judgeable
    /// (#7747).
    fn on_branch(mut self, branch: &str) -> Self {
        self.checkout_head = Some(branch.to_string());
        self
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
    fn changelog_gate(&self, _base: &str, head: &str) -> anyhow::Result<ChangelogVerdict> {
        // #7747: only a head this checkout stands on can be judged.
        if head != "HEAD" && self.checkout_head.as_deref() != Some(head) {
            return Ok(ChangelogVerdict::HeadElsewhere);
        }
        Ok(self.changelog.clone())
    }
    fn base_freshness(
        &self,
        _base: &str,
        mode: trusty_mpm::core::base_ref_freshness::RefreshMode,
    ) -> trusty_mpm::core::base_ref_freshness::BaseFreshness {
        self.freshness_modes.borrow_mut().push(mode);
        self.base_freshness.clone()
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

/// A body satisfying all nine fields and the footer.
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
        minimal: false,
        session: None,
        repo: None,
        dry_run: false,
    }
}

// ── body contract ────────────────────────────────────────────────────────

#[test]
fn body_field_table_covers_nine() {
    assert_eq!(FIELDS.len(), 9);
    let mut headings: Vec<&str> = FIELDS.iter().map(|f| f.heading()).collect();
    headings.sort_unstable();
    headings.dedup();
    assert_eq!(headings.len(), 9, "field headings must be distinct");
}

#[test]
fn body_accepts_a_complete_body() {
    let report = body::validate(&full_body());
    assert!(report.missing.is_empty(), "{report:?}");
    assert!(report.empty.is_empty(), "{report:?}");
    assert!(report.footer_ok);
    assert_eq!(report.supplied.len(), 9);
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

/// #7727: the nine body headings `tm pr open` checks must be named,
/// verbatim, in every asset that tells an agent how to write a PR body —
/// otherwise the asset drifts from the checker silently, the way it did
/// before this test existed (three of five ticketing-audit PR-opening runs
/// failed `tm pr open` and burned a turn on `--help` to find the headings).
#[test]
fn body_headings_are_named_verbatim_in_the_assets() {
    for f in FIELDS {
        let heading = format!("## {}", f.heading());
        assert!(
            trusty_agents_common::agent_assets::VERSION_CONTROL.contains(&heading),
            "version-control.md is missing {heading:?}"
        );
        assert!(
            trusty_mpm::core::bundle::TM_WORKFLOW.contains(&heading),
            "tm-workflow.md is missing {heading:?}"
        );
    }
}

#[test]
fn body_accepts_alias_headings() {
    let body = format!(
        "## 1. Primary outcome\nx\n## 2. What changed\nx\n## 3. Risk / blast radius\nx\n\
         ## 4. Test evidence\nx\n## 5. Pre-existing failures\nx\n\
         ## 6. Gates not run, and why\nnone\n## 7. Partial-red accounting\nnone\n\
         ## 8. Documentation / changelog\nx\n## 9. Review-finding disposition\nx\n\n{ATTRIBUTION_FOOTER}\n"
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
    assert_eq!(plan.supplied.len(), FIELDS.len());
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
/// obligation, because the caller's next move is either `--docs-only` or a run
/// from the worktree that holds the head (#8572). #7747 moved the decision from the branch NAME to the
/// gate's verdict; the refusal itself is unchanged.
/// Test target: `head_elsewhere_refusal`, through `plan`.
#[test]
fn open_head_without_docs_only_is_refused() {
    let mut args = open_args("/dev/null");
    args.head = Some("chore/sessions-abc".to_string());
    let failures = open::plan(
        &args,
        &full_body(),
        Some("s"),
        ChangelogVerdict::HeadElsewhere,
        &ResolvedTicketing::default(),
    )
    .expect_err("a head the gate could not judge must not plan");

    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("check_changelog_fragment.sh"),
        "the refusal must name what cannot be checked: {failures:?}"
    );
    assert!(
        failures[0].contains("--docs-only"),
        "the refusal must name the way out: {failures:?}"
    );
    assert!(
        failures[0].contains("chore/sessions-abc"),
        "the refusal must name the head it is about: {failures:?}"
    );
    // #8572: the refusal must send the caller to a worktree, never to a
    // checkout of the head in whatever tree it stands in.
    assert!(
        failures[0].contains("worktree") && !failures[0].contains("` out"),
        "the refusal must not tell the caller to check the head out: {failures:?}"
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
/// Test target: `plan`'s `ChangelogVerdict::Skipped` arm with a `--head`.
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

// ── #7747: a --head that IS the checkout opens a source PR ───────────────

/// Why (#7747): `gh pr create` resolves the head through `@{push}`, which fails
/// when the local branch name differs from the pushed remote one — a `-r4`
/// suffix against a same-named remote branch, with the matching local name held
/// by another worktree. `--head` is the only way past that, and requiring
/// `--docs-only` with it forced a source PR onto a hand-assembled
/// `gh pr create` that ran none of this command's checks. The named head IS the
/// checkout's commit there, so the gate's `origin/<base>...HEAD` diff is the
/// PR's own diff and the refusal had no grounds.
/// Test target: `run`, through `changelog_gate`'s head argument.
#[test]
fn pr_7747_a_head_that_is_the_checkout_opens_a_source_pr() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.head = Some("fix/b10-pr-open".to_string());
    // No --docs-only: this is a source PR.
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("pr edit", "");
    let pre = FakePreflight::ok()
        .on_branch("fix/b10-pr-open")
        .with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    assert_eq!(
        open::run(&gh, &args, &pre).expect("a head that is the checkout opens"),
        0
    );
    let create = gh
        .calls()
        .into_iter()
        .find(|c| c.join(" ").contains("pr create"))
        .expect("gh pr create was called");
    assert_eq!(flag_value(&create, "--head"), Some("fix/b10-pr-open"));
}

/// Why: the relaxation must not reach a head the gate genuinely cannot judge —
/// that is the #7282 round-5 hole, where a source PR with no fragment passed a
/// gate run against the checkout instead.
/// Test target: `run` with a head this checkout does not stand on.
#[test]
fn pr_7747_a_divergent_head_is_still_refused() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.head = Some("someone-elses-branch".to_string());
    let gh = FakeGh::new();
    let pre = FakePreflight::ok().on_branch("fix/b10-pr-open");

    assert_eq!(
        open::run(&gh, &args, &pre).expect("a failed check is not an error"),
        2
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
}

/// Why: only the pushed branch resolves when the local branch carries a
/// different name, so `origin/<head>` has to be a candidate — that is the
/// mismatch #7747 reported.
#[test]
fn head_rev_candidates_try_the_remote_ref() {
    assert_eq!(
        open::head_rev_candidates("fix/foo"),
        ["fix/foo".to_string(), "origin/fix/foo".to_string()]
    );
}

// ── #7615: --minimal opts out of the nine-heading contract ──────────────

/// Why (#7615): on trusty-things#261 the PM authorized that project's own
/// sparse Why/What/Test/Gate body and `tm pr open` refused it, naming all seven
/// headings. The agent fell back to `gh pr create`, losing the footer check and
/// the changelog gate as well — three gates given up to escape one.
/// Test target: `plan` under `--minimal`.
#[test]
fn pr_7615_minimal_skips_the_heading_contract() {
    let mut args = open_args("/dev/null");
    args.minimal = true;
    let sparse = format!("Why: a thing.\nWhat: it does it.\n\n{ATTRIBUTION_FOOTER}\n");
    let plan = open::plan(
        &args,
        &sparse,
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect("--minimal accepts a body written to another project's standard");
    assert!(plan.argv.join(" ").contains("pr create"));
}

/// Why: `--minimal` drops ONE check. The footer is the attribution the squash
/// commit carries, and the changelog gate is the one this command exists to
/// run — giving those up too would make the flag the fallback it replaces.
#[test]
fn pr_7615_minimal_still_enforces_the_footer_and_the_changelog() {
    let mut args = open_args("/dev/null");
    args.minimal = true;
    let failures = open::plan(
        &args,
        "Why: a thing.\nWhat: no footer at all.\n",
        Some("s"),
        ChangelogVerdict::Fail("FAIL: crates/x has no fragment".to_string()),
        &ResolvedTicketing::default(),
    )
    .expect_err("--minimal keeps the footer and changelog gates");

    assert!(
        failures.iter().any(|f| f.contains("attribution footer")),
        "{failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.contains("check_changelog_fragment.sh")),
        "{failures:?}"
    );
    assert!(
        !failures
            .iter()
            .any(|f| f.starts_with(body::MISSING_FIELD_PREFIX)),
        "the contract half must be skipped: {failures:?}"
    );
}

// ── #7574: a missing heading offers the skeleton ─────────────────────────

/// Why (#7574): on trusty-things#253 the first `tm pr open` failed with seven
/// named headings, the agent guessed the skeleton, and the second invocation was
/// spent finding out whether the guess was right. The skeleton printed verbatim
/// ends that round trip.
/// Test target: `skeleton_hint`, over `plan`'s own failures.
#[test]
fn pr_7574_a_missing_heading_offers_the_body_skeleton() {
    let args = open_args("/dev/null");
    let failures = open::plan(
        &args,
        &format!("## Outcome\n\nreal.\n\n{ATTRIBUTION_FOOTER}\n"),
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect_err("six missing headings must fail the plan");

    let skeleton = open::skeleton_hint(&failures).expect("a missing heading offers the skeleton");
    for f in FIELDS {
        assert!(
            skeleton.contains(&format!("## {}\n", f.heading())),
            "{:?} missing from the skeleton:\n{skeleton}",
            f.heading()
        );
    }
    assert!(skeleton.contains(ATTRIBUTION_FOOTER), "{skeleton}");
    // The headings appear in contract order, so the block is paste-able as is.
    let offsets: Vec<usize> = FIELDS
        .iter()
        .map(|f| {
            skeleton
                .find(&format!("## {}", f.heading()))
                .unwrap_or(usize::MAX)
        })
        .collect();
    assert!(offsets.windows(2).all(|w| w[0] < w[1]), "{offsets:?}");
}

/// Why: the skeleton is the fix for a MISSING heading and nothing else. Printing
/// it after a footer or changelog failure would bury the real reason under nine
/// headings the body already has.
#[test]
fn pr_7574_other_failures_offer_no_skeleton() {
    let args = open_args("/dev/null");
    let failures = open::plan(
        &args,
        &format!("{}\nafterword\n", full_body()),
        Some("s"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect_err("a trailing line after the footer must fail");
    assert!(open::skeleton_hint(&failures).is_none(), "{failures:?}");
}

/// Why: the skeleton is derived from the field table, so a field added there
/// must appear in it without a second edit.
#[test]
fn body_skeleton_names_every_field() {
    let skeleton = body::skeleton();
    for f in FIELDS {
        assert!(skeleton.contains(&format!("## {}", f.heading())), "{f:?}");
    }
    // Pasted unfilled it fails again, naming each section that holds no content.
    let report = body::validate(&skeleton);
    assert!(report.missing.is_empty(), "{report:?}");
    assert_eq!(report.empty.len(), FIELDS.len(), "{report:?}");
    assert!(report.footer_ok);
}

// ── #7336: the two disclosure fields widen the contract ──────────────────

/// A `## Gates not run` / `## Partial-red accounting` pair in its minimal form.
///
/// #7336: the issue rules an explicit "none" valid for either, so this is the
/// shortest body text that satisfies both.
const MINIMAL_DISCLOSURE: &str =
    "## Gates not run\n\nnone\n\n## Partial-red accounting\n\nnone\n\n";

/// The two headings #7336 adds, in contract order.
const NEW_HEADINGS: [&str; 2] = ["Gates not run", "Partial-red accounting"];

/// [`full_body`] with the `## <heading>` section removed by text.
///
/// Why (#7336): dropping a section by its heading rather than by [`Field`]
/// keeps this whole block COMPILING against a checkout whose field table
/// predates the two fields — so the red-first run reports a failed assertion
/// (the heading was never required) rather than an unknown-variant build error.
fn full_body_without_heading(heading: &str) -> String {
    full_body().replace(&format!("## {heading}\n\nsomething real.\n\n"), "")
}

/// Why (#7336): Codex's #7323 disclosed a provider gate it could not run and
/// its #6966 itemized each still-failing target; neither disclosure had a home
/// in the body contract, so both depended on the author volunteering them.
/// Test target: [`body::validate`] over a complete nine-field body.
#[test]
fn pr_7336_a_full_body_carrying_both_fields_passes() {
    for heading in NEW_HEADINGS {
        assert!(
            full_body().contains(&format!("## {heading}")),
            "the contract must require `## {heading}`"
        );
    }
    let report = body::validate(&full_body());
    assert!(report.missing.is_empty(), "{report:?}");
    assert!(report.empty.is_empty(), "{report:?}");
    assert_eq!(report.supplied.len(), FIELDS.len(), "{report:?}");
}

/// Why (#7336): a required field is only required if dropping it fails, and the
/// failure has to NAME the field — that is the whole point of the field table.
/// Test target: [`body::validate`] and `BodyReport::failures`.
#[test]
fn pr_7336_a_full_body_missing_either_field_fails_naming_it() {
    for heading in NEW_HEADINGS {
        let failures = body::validate(&full_body_without_heading(heading)).failures();
        assert_eq!(
            failures.len(),
            1,
            "dropping `## {heading}` must fail exactly once: {failures:?}"
        );
        assert!(
            failures[0].starts_with(body::MISSING_FIELD_PREFIX)
                && failures[0].contains(&format!("`## {heading}`")),
            "the failure must name the field: {failures:?}"
        );
    }
}

/// Why (#7336): "an explicit `none` is valid for either" is the ruling, and a
/// section holding one word is exactly the shape a whitespace-only section is
/// rejected for. This pins the difference.
#[test]
fn pr_7336_an_explicit_none_satisfies_both_fields() {
    let body = full_body()
        .replace(
            "## Gates not run\n\nsomething real.\n",
            "## Gates not run\n\nnone\n",
        )
        .replace(
            "## Partial-red accounting\n\nsomething real.\n",
            "## Partial-red accounting\n\nnone\n",
        );
    let report = body::validate(&body);
    assert!(
        report.missing.is_empty() && report.empty.is_empty(),
        "{report:?}"
    );
}

/// Why (#7336): the author who does not know `none` is valid writes an empty
/// section and fails a second time. The missing-field line carries the hint;
/// the seven older fields must NOT carry it, because `none` is no answer to
/// "what changed".
#[test]
fn pr_7336_the_failure_line_offers_the_minimal_none_form() {
    for heading in NEW_HEADINGS {
        let mut failures = body::validate(&full_body_without_heading(heading)).failures();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures.remove(0).contains("`none`"), "{heading}");
    }
    for heading in ["Outcome", "Changes", "Risk"] {
        let mut failures = body::validate(&full_body_without_heading(heading)).failures();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(!failures.remove(0).contains("`none`"), "{heading}");
    }
}

/// Why (#7336): the skeleton is what a failing author pastes, so a field absent
/// from it is a field nobody can satisfy on the second try.
#[test]
fn pr_7336_the_skeleton_carries_both_new_fields() {
    let skeleton = body::skeleton();
    for heading in ["## Gates not run", "## Partial-red accounting"] {
        assert!(skeleton.contains(heading), "{heading} missing:\n{skeleton}");
    }
    // Order: after `## Baseline`, before `## Docs`.
    let at = |h: &str| skeleton.find(h).unwrap_or(usize::MAX);
    assert!(
        at("## Baseline") < at("## Gates not run")
            && at("## Gates not run") < at("## Partial-red accounting")
            && at("## Partial-red accounting") < at("## Docs"),
        "{skeleton}"
    );
}

/// Why (#7336): widening the contract must not re-break #7615. `--minimal` is
/// for a project whose own `CLAUDE.md` names a different body standard, and the
/// two new fields are part of the standard it opts out of — a body in that
/// project's shape still opens, with or without them.
#[test]
fn pr_7336_minimal_validates_with_and_without_the_new_fields() {
    let mut args = open_args("/dev/null");
    args.minimal = true;
    for body in [
        format!("Why: a thing.\nWhat: it does it.\n\n{ATTRIBUTION_FOOTER}\n"),
        format!("Why: a thing.\n\n{MINIMAL_DISCLOSURE}{ATTRIBUTION_FOOTER}\n"),
    ] {
        open::plan(
            &args,
            &body,
            Some("s"),
            ChangelogVerdict::Pass,
            &ResolvedTicketing::default(),
        )
        .expect("--minimal accepts another project's standard, widened or not");
    }
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

/// #7748: a stale diff base refuses before any gate runs and before `gh`.
///
/// Why: the changelog gate, the component labels and the mandatory pre-push
/// credential scan all diff `origin/<base>...<head>`. A local base ref hundreds
/// of commits behind widens every one of them — the measured case would have
/// put ~1,270 unrelated paths through a credential scan.
#[test]
fn pr_7748_a_stale_base_refuses_before_gh_is_called() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new();
    let code = open::run(&gh, &args, &FakePreflight::ok().with_stale_base())
        .expect("a refused check is not an error");
    assert_eq!(code, super::EXIT_CHECK_FAILED);
    assert!(
        gh.calls().is_empty(),
        "gh must not be spawned against an unverified diff base: {:?}",
        gh.calls()
    );
}

/// #7748 round 2: a dry run compares the base; it never fetches.
///
/// Why: a fetch writes `refs/remotes/origin/<base>`, which every worktree of
/// the clone shares — a preview that moved it would change the diff base under
/// a sibling agent mid-task. The no-fetch property itself is pinned in
/// `compare_only_reports_stale_without_fetching`; this pins that `--dry-run`
/// asks for that mode.
#[test]
fn pr_7748_a_dry_run_compares_without_fetching() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.dry_run = true;
    let pre = FakePreflight::ok();
    let gh = FakeGh::new();
    assert_eq!(
        open::run(&gh, &args, &pre).expect("dry run succeeds"),
        super::EXIT_OK
    );
    assert_eq!(
        pre.freshness_modes.borrow().as_slice(),
        [trusty_mpm::core::base_ref_freshness::RefreshMode::CompareOnly],
        "a preview must ask for a compare, never a fetch"
    );
}

/// A real run is allowed to close the gap it finds.
#[test]
fn pr_7748_a_real_run_may_fetch_the_base() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let pre = FakePreflight::ok();
    let gh = FakeGh::new()
        .on("label create", "")
        .on("pr create", "https://github.com/o/r/pull/4242\n");
    assert_eq!(
        open::run(&gh, &args, &pre).expect("create succeeds"),
        super::EXIT_OK
    );
    assert_eq!(
        pre.freshness_modes.borrow().as_slice(),
        [trusty_mpm::core::base_ref_freshness::RefreshMode::FetchOnDrift]
    );
}

/// #7748 fail-closed: a base that could not be verified refuses too.
#[test]
fn pr_7748_an_unverifiable_base_refuses() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let mut pre = FakePreflight::ok();
    pre.base_freshness = trusty_mpm::core::base_ref_freshness::BaseFreshness::Undetermined {
        reason: "`git ls-remote origin refs/heads/main` failed: no such remote".to_string(),
    };
    let gh = FakeGh::new();
    assert_eq!(
        open::run(&gh, &args, &pre).expect("a refused check is not an error"),
        super::EXIT_CHECK_FAILED
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
}

#[test]
fn open_creates_and_reports() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("label create", "")
        .on("pr create", "https://github.com/o/r/pull/4242\n");
    let code = open::run(&gh, &args, &FakePreflight::ok()).expect("create succeeds");
    assert_eq!(code, super::EXIT_OK);
    let calls = gh.calls();
    // #7513: the label seed, then the create that applies it.
    assert_eq!(calls.len(), 2);
    assert!(calls[1].join(" ").contains("--label ws/tm-test-01"));
}

/// REGRESSION (#7513): `gh pr create --label ws/<x>` fails outright on a label
/// the repository has never seen, and the caller that hits that hardest —
/// `session_context_pause`, publishing from inside the daemon — has no tmux
/// session for `tm issue seed-labels` to read. So the open seeds the label
/// itself, BEFORE the create, and with `--force` so re-running is a no-op.
/// Red before the fix: the only `gh` call is `pr create`.
#[test]
fn open_seeds_the_workstream_label_before_creating_7513() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("label create", "")
        .on("pr create", "https://github.com/o/r/pull/4242\n");

    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("create succeeds"),
        super::EXIT_OK
    );

    let calls = gh.calls();
    let seed = calls[0].join(" ");
    assert!(seed.starts_with("label create ws/tm-test-01"), "{seed}");
    assert!(seed.contains("--force"), "{seed}");
    assert!(
        calls[1].join(" ").starts_with("pr create"),
        "the seed must come first: {:?}",
        calls[1]
    );
}

/// #7513: the seed is not the deliverable. A repository where `gh label create`
/// fails but the label already exists must still open its PR — and the warning
/// says which label could not be seeded, so a create that then fails on the
/// label is not a mystery.
#[test]
fn open_still_creates_when_the_label_seed_fails_7513() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    // No `label create` route: FakeGh answers it as a failure.
    let gh = FakeGh::new().on("pr create", "https://github.com/o/r/pull/4242\n");

    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("create succeeds"),
        super::EXIT_OK
    );
    assert_eq!(gh.calls().len(), 2, "the create still ran");
}

/// #7513: `--dry-run` prints the seed alongside the create and still spawns
/// nothing — adding a second command must not give the rehearsal a side effect.
#[test]
fn open_dry_run_spawns_no_gh_for_the_label_seed_7513() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.dry_run = true;
    let gh = FakeGh::new();

    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("dry run"),
        super::EXIT_OK
    );
    assert!(
        gh.calls().is_empty(),
        "a dry run must not spawn gh, seed included"
    );
}

// ── PR labels / project / milestone standard (#7274) ─────────────────────

/// The `gh issue view --json` payload for an issue with both fields set.
const ISSUE_JSON: &str = r#"{"number":7274,"milestone":{"title":"mpm 1.6"},
    "projectItems":[{"title":"trusty-mpm"},{"title":"Harness"}],
    "labels":[],"comments":[],"state":"OPEN"}"#;

fn refs_issue() -> RefsIssue {
    RefsIssue {
        number: 7274,
        kind: RefKind::Issue,
        milestone: Some("mpm 1.6".to_string()),
        projects: vec!["trusty-mpm".to_string()],
    }
}

/// The `gh pr view --json number,milestone,projectItems` payload for a link
/// line that names a PULL REQUEST rather than an issue (#7786).
const PR_REF_JSON: &str =
    r#"{"number":7782,"milestone":{"title":"mpm 1.6"},"projectItems":[{"title":"Harness"}]}"#;

/// The verbatim GraphQL answer `gh issue view <pr-number>` gives (#7786).
const NOT_AN_ISSUE: &str = "GraphQL: Could not resolve to an Issue with the number of 7782";

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
        kind: RefKind::Issue,
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
    let issue = RefsIssue::from_facts(7274, RefKind::Issue, &facts);
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
        inherited_from: Some(7274),
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
    assert_eq!(metadata::first_linked_issue(body), Some(7274));
}

#[test]
fn metadata_finds_a_qualified_refs() {
    assert_eq!(
        metadata::first_linked_issue("Refs bobmatnyc/trusty-tools#7274\n"),
        Some(7274)
    );
}

#[test]
fn metadata_ignores_refs_mid_sentence() {
    assert_eq!(
        metadata::first_linked_issue("This one refs #12 in passing.\nRefs #34\n"),
        Some(34),
        "only a line that STARTS with the keyword is the link line"
    );
    assert_eq!(metadata::first_linked_issue("## Outcome\n\ntext\n"), None);
}

/// #7274 round 2: a body that quotes the convention before stating its own
/// link must inherit from its own link, not from the sample.
#[test]
fn metadata_ignores_refs_inside_a_fence() {
    let body =
        "## Outcome\n\nEvery fix PR reads:\n\n```\nfix(x): y\n\nRefs #999\n```\n\nRefs #7274\n";
    assert_eq!(
        metadata::first_linked_issue(body),
        Some(7274),
        "a fenced sample is not the PR's own link"
    );
    // An info string opens a fence the same way a bare one does.
    let tagged = "```text\nRefs #999\n```\n\nRefs #7274\n";
    assert_eq!(metadata::first_linked_issue(tagged), Some(7274));
    // A body whose only `Refs` is fenced has no link at all.
    assert_eq!(metadata::first_linked_issue("```\nRefs #999\n```\n"), None);
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
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/7\n")
        // #7869: an unrouted edit is now a PARTIAL apply, not a swallowed
        // warning, and this test is about the label LANDING.
        .on("pr edit 7", "");
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

/// #7869: a failed metadata apply never aborts the run — the PR exists and is
/// reported — but it is no longer reported as a success either. After the one
/// per-field retry has also failed, the exit code is `EXIT_PARTIAL`, which is
/// distinct from "a check failed and `gh` was never called".
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
        super::EXIT_PARTIAL,
        "the PR exists, so this is a partial apply — never a silent success"
    );
    // The retry ran once and no more: the combined edit, then the label step.
    let edits = gh
        .calls()
        .into_iter()
        .filter(|c| c.join(" ").starts_with("pr edit"))
        .count();
    assert_eq!(edits, 2, "one combined edit, then exactly one retry step");
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
///
/// The failure here is deliberately NOT the `Could not resolve to an Issue`
/// answer — that one earns a `gh pr view` retry (#7786), which
/// `pr_7786_a_pr_ref_inherits_through_gh_pr_view` covers. This is the arm where
/// the read failed for any other reason and there is nothing to retry.
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
        .on_fail("issue view 7274", "HTTP 502 (api.github.com/graphql)")
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

// ── B7: the `tm pr open` / `tm pr merge` metadata contract ───────────────

/// A `full_body` with `line` inserted immediately above the footer.
fn body_linking(line: &str) -> String {
    full_body().replace(
        ATTRIBUTION_FOOTER,
        &format!("{line}\n\n{ATTRIBUTION_FOOTER}"),
    )
}

/// The `gh pr edit` argv the run issued, joined, if one did.
fn first_edit(gh: &FakeGh) -> Option<String> {
    gh.calls()
        .into_iter()
        .map(|c| c.join(" "))
        .find(|c| c.starts_with("pr edit"))
}

/// REGRESSION (#7869): metadata inheritance must not depend on WHICH sanctioned
/// keyword links the issue. `metadata::first_refs_issue` matched only `Refs`, so
/// every `tm pr open --closes` PR opened with no project and no milestone and
/// the PM re-applied both by hand.
/// Red before the fix: no `gh issue view` runs at all, and the only `gh pr edit`
/// carries the component label with no `--milestone`.
#[test]
fn pr_7869_a_closes_link_inherits_the_issues_metadata() {
    let (_d, path) = scratch_body(&full_body());
    let mut args = open_args(&path.to_string_lossy());
    args.issue = Some(7274);
    args.closes = true;
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("issue view 7274", ISSUE_JSON)
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    assert_eq!(
        open::run(&gh, &args, &pre).expect("create succeeds"),
        super::EXIT_OK
    );

    let edit = first_edit(&gh).expect("a `gh pr edit` ran");
    assert!(
        edit.contains("--milestone mpm 1.6"),
        "a `Closes #N` body inherits the milestone too: {edit}"
    );
    assert!(edit.contains("--add-project trusty-mpm"), "{edit}");
    assert!(edit.contains("--add-label trusty-mpm"), "{edit}");
}

/// REGRESSION (#7869, observed on PR #7918): `gh pr create` creates the PR and
/// THEN applies the assignee and labels over separate API calls. A 502 on one of
/// those exited non-zero with the PR already open, and `tm pr open` bailed
/// printing no number — the caller had to find the PR with `gh pr list --head`.
/// Red before the fix: `open::run` returns `Err`, so no number and no URL are
/// printed and nothing is retried.
#[test]
fn pr_7869_a_create_that_fails_after_creating_reports_the_pr_and_retries() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("label create", "")
        .on_partial(
            "pr create",
            "https://github.com/o/r/pull/7918\n",
            "HTTP 502: Something went wrong (graphql)",
        )
        .on("pr edit 7918", "");
    let pre = FakePreflight::ok();

    assert_eq!(
        open::run(&gh, &args, &pre).expect("a PR that exists is not an error"),
        super::EXIT_OK,
        "the retry succeeded, so the command is a success"
    );
    let edit = first_edit(&gh).expect("the create's own metadata step was retried");
    assert!(edit.contains("--add-assignee"), "{edit}");
    assert!(edit.contains("--add-label trusty-mpm"), "{edit}");
    assert!(edit.contains("--add-label ws/tm-test-01"), "{edit}");
    // The PR is real, so it is registered for post-merge cleanup like any other.
    assert_eq!(pre.cleanup_registry().entries()[0].pr, 7918);
}

/// REGRESSION (#7869): the same partial create whose retry ALSO fails exits
/// non-zero — but as `EXIT_PARTIAL`, never as `EXIT_CHECK_FAILED`, which means
/// "a check failed and `gh` was never called".
#[test]
fn pr_7869_a_create_retry_that_also_fails_exits_non_zero() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("label create", "")
        .on_partial(
            "pr create",
            "https://github.com/o/r/pull/7918\n",
            "HTTP 502: Something went wrong (graphql)",
        )
        .on_fail("pr edit 7918", "HTTP 502: Something went wrong (graphql)");

    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("the PR still exists"),
        super::EXIT_PARTIAL
    );
}

/// #7869: a `gh pr create` that failed with NO PR URL on stdout is still a hard
/// error. "The PR exists" must be read off evidence, never assumed.
#[test]
fn pr_7869_a_create_that_fails_with_no_url_is_still_an_error() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("label create", "")
        .on_fail("pr create", "pull request create failed: not authorized");
    let err = open::run(&gh, &args, &FakePreflight::ok()).expect_err("no PR means a real failure");
    assert!(format!("{err:#}").contains("not authorized"), "{err:#}");
}

/// REGRESSION (#7786): `tm pr open --issue N` where N names a PULL REQUEST made
/// `gh issue view` 404 (`Could not resolve to an Issue with the number of
/// 7782`), and the milestone and projects that PR carried were abandoned after
/// the one failed read — PR #7784 shipped with neither.
/// Red before the fix: no `gh pr view 7782` runs, and the `gh pr edit` carries
/// no `--milestone` and no `--add-project`.
#[test]
fn pr_7786_a_pr_ref_inherits_through_gh_pr_view() {
    let (_d, path) = scratch_body(&body_linking("Refs #7782"));
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on_fail("issue view 7782", NOT_AN_ISSUE)
        .on("pr view 7782", PR_REF_JSON)
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    assert_eq!(
        open::run(&gh, &args, &pre).expect("create succeeds"),
        super::EXIT_OK
    );
    assert!(
        gh.calls()
            .iter()
            .any(|c| c.join(" ").starts_with("pr view 7782")),
        "the 404 earns a `gh pr view` fallback: {:?}",
        gh.calls()
    );
    let edit = first_edit(&gh).expect("a `gh pr edit` ran");
    assert!(edit.contains("--milestone mpm 1.6"), "{edit}");
    assert!(edit.contains("--add-project Harness"), "{edit}");
}

/// #7786: the fallback asks only for the three fields a PR can answer.
/// `issue_audit::AUDIT_JSON_FIELDS` includes `parent`, `blockedBy` and
/// `subIssues`, which `gh pr view` rejects outright — reusing it would turn the
/// fallback into a second guaranteed failure.
#[test]
fn metadata_pr_view_argv_asks_only_for_pr_fields() {
    let argv = metadata::pr_view_argv(7782).join(" ");
    assert_eq!(argv, "pr view 7782 --json number,milestone,projectItems");
    for issue_only in ["parent", "blockedBy", "subIssues"] {
        assert!(!argv.contains(issue_only), "{argv}");
    }
}

/// REGRESSION (#7646): `apply_metadata` applied labels, milestone and projects
/// in ONE `gh pr edit`, so a milestone `gh` could not resolve — PR #7639
/// inherited the closed `tm 1.3.5` from #4642 — failed the whole edit and the
/// PR ended up with no component label either, warned about with nothing but
/// `gh`'s raw stderr (`'tm 1.3.5' not found`).
/// Red before the fix: exactly one `gh pr edit` runs, the component label never
/// lands, and the run exits 0 as though the metadata had been applied.
#[test]
fn pr_7646_a_failed_edit_names_the_field_and_retries_per_field() {
    let (_d, path) = scratch_body(&body_linking("Refs #7274"));
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/7639\n")
        .on("issue view 7274", ISSUE_JSON)
        // The per-field label retry succeeds — matched EXACTLY, because the
        // combined edit's argv starts with the same three tokens. Every edit
        // carrying the unresolvable milestone fails, the combined one included.
        .on_exact("pr edit 7639 --add-label trusty-mpm", "")
        .on_fail("pr edit 7639", "'mpm 1.6' not found");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    assert_eq!(
        open::run(&gh, &args, &pre).expect("the PR exists"),
        super::EXIT_PARTIAL,
        "an unresolvable milestone is a partial apply, not a success"
    );
    let edits: Vec<String> = gh
        .calls()
        .into_iter()
        .map(|c| c.join(" "))
        .filter(|c| c.starts_with("pr edit"))
        .collect();
    assert!(
        edits
            .iter()
            .any(|e| e == "pr edit 7639 --add-label trusty-mpm"),
        "the component label lands on its own despite the bad milestone: {edits:?}"
    );
    assert!(
        edits.iter().any(|e| e.contains("--milestone mpm 1.6")
            && !e.contains("--add-label")
            && !e.contains("--add-project")),
        "the milestone was retried by itself: {edits:?}"
    );
}

/// #7646 / Fail-Open Check: a metadata step that failed is never reported as
/// applied. The outcome names it as missing, with its value and the issue it was
/// inherited from, so the caller's exit message can say what the PR lacks.
#[test]
fn pr_7646_a_failed_step_is_reported_missing_never_applied() {
    let (_d, path) = scratch_body(&body_linking("Refs #7274"));
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new()
        .on("issue view 7274", ISSUE_JSON)
        .on_exact("pr edit 4242 --add-label trusty-mpm", "")
        .on_fail("pr edit 4242", "'mpm 1.6' not found");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    let missing = match metadata_apply::apply(&gh, &args, &pre, "4242", &body_linking("Refs #7274"))
    {
        ApplyOutcome::Partial(missing) => missing,
        ApplyOutcome::Applied => panic!("a failed milestone step must not read as Applied"),
    };
    let joined = missing.join("; ");
    assert!(joined.contains("milestone \"mpm 1.6\""), "{joined}");
    assert!(joined.contains("inherited from #7274"), "{joined}");
    assert!(
        !joined.contains("component labels"),
        "the label DID apply on retry, so it is not missing: {joined}"
    );
}

/// REGRESSION (#7868): `tm pr merge` re-ran the nine-field OPEN gate, so a body
/// written to the sparse prose rules (defect / evidence / resolution, no filled
/// headings for fields the change does not touch) could not be merged by the one
/// command that passes the reviewed body through `--body-file`. The PM merged
/// with raw `gh pr merge` instead, losing that guarantee entirely.
/// Red before the fix: `decide` refuses, naming every missing body field.
#[test]
fn pr_7868_a_sparse_body_is_not_a_merge_refusal() {
    let sparse = format!(
        "## Defect\n\n`tm pr open` dropped the milestone.\n\n\
         ## Evidence\n\n`gh pr view 7639 --json milestone` returned null.\n\n\
         ## Resolution\n\nThe apply retries per field.\n\n{ATTRIBUTION_FOOTER}\n"
    );
    // The body genuinely fails the nine-field contract — that is the point.
    assert_eq!(body::validate(&sparse).contract_gaps().len(), FIELDS.len());

    let view = merge_view(&sparse, serde_json::json!({}));
    assert_eq!(
        merge_decision(&view),
        merge::Decision::Merge,
        "a sparse body carrying the footer merges"
    );
}

/// #7868: the footer is still a hard refusal even on a sparse body — it is part
/// of the landing commit message `tm pr merge` writes, unlike the nine fields.
#[test]
fn pr_7868_a_sparse_body_without_the_footer_still_refuses() {
    let view = merge_view("## Defect\n\nsomething broke.\n", serde_json::json!({}));
    let reason = merge_refusal(&view).expect("refused");
    assert!(reason.contains("attribution footer"), "{reason}");
}

// ── post-merge cleanup registry (#7275) ──────────────────────────────────

/// REGRESSION (#7275): a PR `tm pr open` created is recorded for post-merge
/// cleanup, keyed by the repo and number in `gh`'s own URL — with no second
/// `gh` call, so the record cannot name a different remote than the push used.
#[test]
fn open_records_the_new_pr_for_cleanup() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = FakeGh::new().on("label create", "").on(
        "pr create",
        "https://github.com/bobmatnyc/trusty-tools/pull/7275\n",
    );
    let pre = FakePreflight::ok();
    let code = open::run(&gh, &args, &pre).expect("create succeeds");
    assert_eq!(code, super::EXIT_OK);
    // #7513 added the label seed; recording still costs no call of its own.
    assert_eq!(
        gh.calls().len(),
        2,
        "recording must not cost a gh call beyond the seed and the create"
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
///
/// #8670: `mergeable`/`mergeStateStatus` default to the GitHub-clean happy
/// path so every pre-existing caller of this helper stays admitted; tests of
/// [`queue_check::stop_reason`]'s new mergeability gate override them via
/// `extra`.
fn view_json(extra: &str) -> String {
    format!(
        r#"{{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
            "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
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
        "statusCheckRollup":[{"name":"Clippy","status":"COMPLETED","conclusion":"FAILURE"}],"comments":[]}"#;
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
        "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
        "statusCheckRollup":[
          {"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS"}],
        "comments":[{"body":"code-critic: BLOCK"},{"body":"code-critic: APPROVE"}]}"#;
    assert_eq!(first_reason(json), None);
}

#[test]
fn queue_critic_ignores_unrelated_comments() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
        "statusCheckRollup":[
          {"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS"}],
        "comments":[{"body":"we should BLOCK bad merges in general"}]}"#;
    assert_eq!(first_reason(json), None);
}

#[test]
fn queue_required_context_missing() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
        "statusCheckRollup":[{"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"}],"comments":[]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("`Rust tests` is missing"), "{reason}");
}

#[test]
fn queue_required_context_not_success() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
        "statusCheckRollup":[
          {"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SKIPPED"}],
        "comments":[]}"#;
    let reason = first_reason(json).expect("blocked");
    assert!(reason.contains("`Rust tests` is not SUCCESS"), "{reason}");
}

#[test]
fn queue_accepts_status_context() {
    // A StatusContext entry carries `context`/`state`, not `name`/`conclusion`.
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
        "statusCheckRollup":[
          {"context":"Clippy","state":"SUCCESS"},
          {"context":"Rust tests","state":"SUCCESS"}],
        "comments":[]}"#;
    assert_eq!(first_reason(json), None);
}

/// A PR view whose `Clippy` is green and whose `Rust tests` ran as `runs`.
///
/// #8670: `mergeable`/`mergeStateStatus` default to GitHub-clean so the
/// duplicate-run regressions this feeds keep exercising the required-context
/// gate, not the new mergeability one.
fn duplicate_run_view(runs: &str) -> String {
    format!(
        r#"{{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
            "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
            "statusCheckRollup":[
              {{"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS",
                "startedAt":"2026-09-25T22:40:00Z","completedAt":"2026-09-25T22:45:00Z"}},
              {runs}],
            "comments":[]}}"#
    )
}

/// REGRESSION (#8638): PR #8637's rollup — a concurrency-cancelled run listed
/// before the fresh SUCCESS on the same head SHA. The latest run decides.
#[test]
fn queue_duplicate_cancelled_then_success_is_mergeable() {
    let json = duplicate_run_view(
        r#"{"name":"Rust tests","status":"COMPLETED","conclusion":"CANCELLED",
            "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-09-25T22:51:10Z","completedAt":"2026-09-25T22:53:26Z"}"#,
    );
    assert_eq!(first_reason(&json), None);
}

/// REGRESSION (#8638): the fail-open order. A stale SUCCESS listed first must
/// not shadow the later FAILURE of the same context.
#[test]
fn queue_duplicate_success_then_failure_is_blocked() {
    let json = duplicate_run_view(
        r#"{"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"FAILURE",
            "startedAt":"2026-09-25T22:52:00Z","completedAt":"2026-09-25T22:55:00Z"}"#,
    );
    let reason = first_reason(&json).expect("a red latest run blocks");
    assert!(reason.contains("`Rust tests` is not SUCCESS"), "{reason}");
}

/// REGRESSION (#8638): a newer run still in flight is pending, not the older
/// SUCCESS. `gh` prints a running check's `completedAt` as Go's zero time.
#[test]
fn queue_duplicate_success_then_running_is_pending() {
    let json = duplicate_run_view(
        r#"{"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
          {"name":"Rust tests","status":"IN_PROGRESS","conclusion":"",
            "startedAt":"2026-09-25T22:52:00Z","completedAt":"0001-01-01T00:00:00Z"}"#,
    );
    let reason = first_reason(&json).expect("a running latest run blocks");
    assert!(reason.contains("`Rust tests` is pending"), "{reason}");
    assert!(
        reason.ends_with("(IN_PROGRESS, started 2026-09-25T22:52:00+00:00)"),
        "{reason}"
    );
}

/// REGRESSION (#8638): a rerun still QUEUED carries no timestamps at all.
/// Any run without a conclusion makes the context pending, so the older
/// SUCCESS must not pass the PR.
#[test]
fn queue_duplicate_success_then_queued_is_pending() {
    let json = duplicate_run_view(
        r#"{"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"},
          {"name":"Rust tests","status":"QUEUED","conclusion":""}"#,
    );
    let reason = first_reason(&json).expect("a queued rerun blocks");
    assert!(reason.contains("`Rust tests` is pending"), "{reason}");
    assert!(reason.ends_with("(QUEUED, no startedAt)"), "{reason}");
}

/// REGRESSION (#8638): a CheckRun and a StatusContext that share a name are
/// two requirements, and GitHub gates on both. A green CheckRun must not
/// hide a red StatusContext of the same name.
#[test]
fn queue_check_and_status_same_name_both_required() {
    let json = duplicate_run_view(
        r#"{"__typename":"StatusContext","context":"Rust tests","state":"FAILURE",
            "startedAt":"2026-09-25T22:50:00Z"},
          {"__typename":"CheckRun","name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-09-25T22:51:00Z","completedAt":"2026-09-25T22:53:26Z"}"#,
    );
    let reason = first_reason(&json).expect("the red StatusContext blocks");
    assert!(reason.contains("`Rust tests` is not SUCCESS"), "{reason}");
}

/// #8638: a context that ran once is judged exactly as before.
#[test]
fn queue_single_run_is_unchanged() {
    let green = duplicate_run_view(
        r#"{"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"}"#,
    );
    assert_eq!(first_reason(&green), None);
    let red = duplicate_run_view(
        r#"{"name":"Rust tests","status":"COMPLETED","conclusion":"FAILURE",
            "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"}"#,
    );
    let reason = first_reason(&red).expect("red blocks");
    assert!(reason.contains("`Rust tests` is not SUCCESS"), "{reason}");
}

/// A `gh pr view --json` payload past every earlier gate (not draft, no hold
/// label, approved, no critic BLOCK), so only [`queue_check`]'s mergeability
/// gate and the required-context loop remain live.
fn mergeability_view(mergeable: &str, merge_state: &str) -> String {
    format!(
        r#"{{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
            "mergeable":{mergeable},"mergeStateStatus":{merge_state},
            "statusCheckRollup":[
              {{"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"}},
              {{"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS"}}],
            "comments":[]}}"#
    )
}

/// REGRESSION (#8670): PRs #8667 and #8669 both had `mergeable: CONFLICTING`
/// and still reported MERGEABLE, because queue-check never requested the
/// field at all.
#[test]
fn queue_mergeable_conflicting_is_blocked() {
    let json = mergeability_view(r#""CONFLICTING""#, r#""CLEAN""#);
    let reason = first_reason(&json).expect("a real conflict blocks");
    assert!(reason.contains("mergeable CONFLICTING"), "{reason}");
}

/// REGRESSION (#8670): `DIRTY` lives in `mergeStateStatus`, a separate enum
/// from `mergeable` (mirrors `tm pr merge`'s `conflict_field`, #6808).
#[test]
fn queue_merge_state_dirty_is_blocked() {
    let json = mergeability_view(r#""MERGEABLE""#, r#""DIRTY""#);
    let reason = first_reason(&json).expect("dirty blocks");
    assert!(reason.contains("mergeStateStatus DIRTY"), "{reason}");
}

/// `UNKNOWN` means GitHub has not finished computing mergeability — pending,
/// never mergeable (#8670 acceptance criterion).
#[test]
fn queue_mergeable_unknown_is_pending() {
    let json = mergeability_view(r#""UNKNOWN""#, r#""CLEAN""#);
    let reason = first_reason(&json).expect("unknown is never mergeable");
    assert!(reason.contains("mergeable is UNKNOWN"), "{reason}");
}

/// Same as [`queue_mergeable_unknown_is_pending`], for `mergeStateStatus`.
#[test]
fn queue_merge_state_unknown_is_pending() {
    let json = mergeability_view(r#""MERGEABLE""#, r#""UNKNOWN""#);
    let reason = first_reason(&json).expect("unknown is never mergeable");
    assert!(reason.contains("mergeStateStatus is UNKNOWN"), "{reason}");
}

/// A `mergeable` field genuinely omitted from the payload fails CLOSED —
/// never read as mergeable (#8670 acceptance criterion).
#[test]
fn queue_mergeable_field_missing_is_pending() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "mergeStateStatus":"CLEAN",
        "statusCheckRollup":[
          {"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS"}],
        "comments":[]}"#;
    let reason = first_reason(json).expect("a missing field is never mergeable");
    assert!(reason.contains("mergeable field is missing"), "{reason}");
}

/// Same as [`queue_mergeable_field_missing_is_pending`], for a genuinely
/// omitted `mergeStateStatus`.
#[test]
fn queue_merge_state_field_missing_is_pending() {
    let json = r#"{"isDraft":false,"labels":[],"reviewDecision":"APPROVED",
        "mergeable":"MERGEABLE",
        "statusCheckRollup":[
          {"name":"Clippy","status":"COMPLETED","conclusion":"SUCCESS"},
          {"name":"Rust tests","status":"COMPLETED","conclusion":"SUCCESS"}],
        "comments":[]}"#;
    let reason = first_reason(json).expect("a missing field is never mergeable");
    assert!(
        reason.contains("mergeStateStatus field is missing"),
        "{reason}"
    );
}

/// The MERGEABLE/CLEAN happy path is still admitted (#8670 acceptance
/// criterion) — this is the one shape that reaches `None`.
#[test]
fn queue_mergeable_clean_happy_path_is_admitted() {
    let json = mergeability_view(r#""MERGEABLE""#, r#""CLEAN""#);
    assert_eq!(first_reason(&json), None);
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
        no_cleanup: false,
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
    // #7868: mirrors `merge::run` — the footer refuses, the nine-field gaps
    // are reported.
    let failures = body::validate(&view.body).merge_failures();
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
        no_cleanup: false,
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

/// The stderr `gh pr merge --delete-branch` prints when a worktree holds the
/// head branch (#7945, the shape reported on PR #7943 and PR #8007).
const WORKTREE_HELD_STDERR: &str = "failed to delete local branch feat/6808-x: error: Cannot \
     delete branch 'feat/6808-x' used by worktree at \
     '/repo/.claude/worktrees/agent-a2278a6f4f4ce2d01'";

/// #7945: a cleanup failure after a landed merge is a warning, not the verdict.
///
/// Why: `gh pr merge --delete-branch` deletes the LOCAL branch after GitHub has
/// already squashed, so a worktree-held branch made `tm pr merge` exit 2 on a
/// merge that was on `main`. Exit 0 is also what lets `tm pr cleanup` run, and
/// cleanup is what reclaims the worktree that blocked the delete.
#[test]
fn pr_7945_a_worktree_held_branch_does_not_fail_a_landed_merge() {
    let gh = FakeGh::new()
        // Registered first: the confirmation read is also a `pr view`.
        .on_exact(
            "pr view 42 --json state,mergeCommit",
            &serde_json::json!({"state": "MERGED", "mergeCommit": {"oid": "686f9bb03"}})
                .to_string(),
        )
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on_fail("pr merge", WORKTREE_HELD_STDERR);

    assert_eq!(
        merge::run(&gh, &merge_args()).expect("a landed merge is not an error"),
        super::EXIT_OK
    );
    assert!(
        gh.calls()
            .iter()
            .any(|a| a.join(" ") == "pr view 42 --json state,mergeCommit"),
        "the merge outcome must be re-read before the failure is accepted: {:?}",
        gh.calls()
    );
}

/// #7945: a merge that did NOT land still fails, even on a cleanup-shaped error.
#[test]
fn pr_7945_a_merge_that_did_not_land_still_fails() {
    let gh = FakeGh::new()
        .on_exact(
            "pr view 42 --json state,mergeCommit",
            &serde_json::json!({"state": "OPEN"}).to_string(),
        )
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on_fail("pr merge", WORKTREE_HELD_STDERR);
    let err = merge::run(&gh, &merge_args()).expect_err("an unmerged PR must stay an error");
    assert!(
        format!("{err:#}").contains("Cannot delete branch"),
        "{err:#}"
    );
    assert!(
        confirmation_was_read(&gh),
        "the verdict must come from a re-read, not from the exit status alone: {:?}",
        gh.calls()
    );
}

/// #7945 round 2: only a branch-delete failure may be downgraded.
///
/// Why: "the PR reads MERGED" is true of a PR someone else merged an hour ago,
/// and of one whose merge landed before an unrelated failure. Reporting either
/// as this run's success would hide a real error behind a green exit.
#[test]
fn pr_7945_a_non_cleanup_failure_on_a_merged_pr_still_fails() {
    let gh = FakeGh::new()
        .on_exact(
            "pr view 42 --json state,mergeCommit",
            &serde_json::json!({"state": "MERGED", "mergeCommit": {"oid": "686f9bb03"}})
                .to_string(),
        )
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on_fail("pr merge", "HTTP 500: something else went wrong");
    let err = merge::run(&gh, &merge_args()).expect_err("an unrelated failure must stay an error");
    assert!(format!("{err:#}").contains("HTTP 500"), "{err:#}");
    assert!(
        !confirmation_was_read(&gh),
        "a failure that is not cleanup-shaped needs no re-read: {:?}",
        gh.calls()
    );
}

/// #7945 round 2: with `--no-delete-branch` a local-cleanup failure is impossible.
#[test]
fn pr_7945_no_delete_branch_never_downgrades_a_failure() {
    let gh = FakeGh::new()
        .on_exact(
            "pr view 42 --json state,mergeCommit",
            &serde_json::json!({"state": "MERGED"}).to_string(),
        )
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on_fail("pr merge", WORKTREE_HELD_STDERR);
    let args = PrMergeArgs {
        pr: 42,
        auto: false,
        no_delete_branch: true,
        no_cleanup: false,
        repo: None,
    };
    let err = merge::run(&gh, &args).expect_err("no delete was asked for, so none can have failed");
    assert!(
        format!("{err:#}").contains("Cannot delete branch"),
        "{err:#}"
    );
    assert!(!confirmation_was_read(&gh), "{:?}", gh.calls());
}

/// #7945 round 2: a PR already MERGED when the run starts is refused outright.
///
/// Why: the downgrade below reads "MERGED" as evidence that THIS invocation's
/// merge landed. That inference is only sound if the PR was OPEN when the run
/// began, which is what this refusal establishes.
#[test]
fn pr_7945_a_pr_already_merged_at_start_is_refused() {
    let gh = FakeGh::new().on(
        "pr view",
        &merge_view_json(&full_body(), serde_json::json!({"state": "MERGED"})),
    );
    assert_eq!(
        merge::run(&gh, &merge_args()).expect("a refusal is not an error"),
        super::EXIT_BLOCKED
    );
    assert!(
        gh.calls()
            .iter()
            .all(|a| a.get(1).map(String::as_str) != Some("merge")),
        "gh pr merge must not be called on a PR that is already merged: {:?}",
        gh.calls()
    );
}

/// #7945 closure condition 2: the report separates the two outcomes.
///
/// Why: "the reported output distinguishes merge succeeded from local branch
/// cleanup deferred/failed" is the issue's own wording, and it is only a
/// guarantee if a test holds the text to it.
#[test]
fn pr_7945_the_report_separates_the_landed_merge_from_the_deferred_cleanup() {
    let report =
        merge::cleanup_deferred_report(42, "feat/6808-x", " as 686f9bb03", WORKTREE_HELD_STDERR);
    assert!(
        report.contains("MERGE LANDED: #42 (feat/6808-x)"),
        "{report}"
    );
    assert!(report.contains("686f9bb03"), "{report}");
    assert!(report.contains("CLEANUP DEFERRED"), "{report}");
    assert!(
        report.contains("/repo/.claude/worktrees/agent-a2278a6f4f4ce2d01"),
        "the worktree holding the branch must be named: {report}"
    );
    assert!(report.contains("tm pr cleanup 42"), "{report}");
}

/// Was the post-failure merge-state confirmation issued? (#7945)
///
/// Why: both non-landed arms assert an ERROR, which the pre-fix code also
/// produced — from the exit status alone. Requiring the re-read is what makes
/// them fail against that code instead of agreeing with it by accident.
fn confirmation_was_read(gh: &FakeGh) -> bool {
    gh.calls()
        .iter()
        .any(|a| a.join(" ") == "pr view 42 --json state,mergeCommit")
}

/// #7945 fail-closed: an unanswerable confirmation never reads as merged.
///
/// Why: the re-read is the only evidence the squash landed. A `gh` that cannot
/// answer leaves the question open, so the failure the caller already saw
/// stands rather than being upgraded to success.
#[test]
fn pr_7945_an_unreadable_confirmation_still_fails() {
    let gh = FakeGh::new()
        .on_exact_fail(
            "pr view 42 --json state,mergeCommit",
            "HTTP 502: Bad gateway",
        )
        .on(
            "pr view",
            &merge_view_json(&full_body(), serde_json::json!({})),
        )
        .on_fail("pr merge", WORKTREE_HELD_STDERR);
    let err = merge::run(&gh, &merge_args()).expect_err("an unanswerable read must not pass");
    assert!(
        format!("{err:#}").contains("Cannot delete branch"),
        "the original failure is what the caller sees: {err:#}"
    );
    assert!(
        confirmation_was_read(&gh),
        "the read must be ATTEMPTED; only its failure keeps the original verdict: {:?}",
        gh.calls()
    );
}

/// #8366: the component labels come from a diff base verified against the
/// remote — never from whatever `origin/<base>` the checkout last fetched.
///
/// Why: #8366 reported labels derived off a stale `origin/main`. Since #7748
/// the run verifies the base with a fetch-capable probe before any diff; this
/// pins that the label diff sits behind that probe, so moving the label step
/// ahead of it (or giving it its own unverified read) turns this red.
/// What: a base the probe had to refresh still yields labels, read after a
/// `FetchOnDrift` probe; a base that stays stale refuses before the label diff
/// is ever read.
/// Test: this function IS the test.
#[test]
fn pr_8366_component_labels_are_read_only_from_a_remote_verified_base() {
    use trusty_mpm::core::base_ref_freshness::{BaseFreshness, RefreshMode};

    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let mut pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
    pre.base_freshness = BaseFreshness::Refreshed {
        was: "old111".to_string(),
        now: "new222".to_string(),
    };
    let gh = FakeGh::new()
        .on("label create", "")
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("pr edit 4242", "");
    assert_eq!(open::run(&gh, &args, &pre).expect("create"), super::EXIT_OK);
    assert_eq!(
        pre.freshness_modes.borrow().as_slice(),
        [RefreshMode::FetchOnDrift]
    );
    assert_eq!(pre.diff_heads.borrow().as_slice(), ["HEAD"]);
    assert!(
        gh.calls()
            .iter()
            .any(|c| c.join(" ").contains("--add-label trusty-mpm")),
        "{:?}",
        gh.calls()
    );

    let stale = FakePreflight::ok()
        .with_diff(&["crates/trusty-mpm/src/lib.rs"])
        .with_stale_base();
    let gh = FakeGh::new();
    assert_eq!(
        open::run(&gh, &args, &stale).expect("a refusal is not an error"),
        super::EXIT_CHECK_FAILED
    );
    assert!(
        stale.diff_heads.borrow().is_empty(),
        "no label diff may be read against an unverified base"
    );
}

// ── #8431: a target repository without the `trusty-mpm` label ──────────────

/// A `gh` fake whose answers are consumed in order, per argv substring.
///
/// Why: the #8431 recovery re-runs the SAME `gh pr create` argv after the label
/// exists, so the first and second answers must differ — a static route table
/// cannot say that.
struct SeqGh {
    answers: std::cell::RefCell<Vec<(String, GhRun)>>,
    seen: std::cell::RefCell<Vec<String>>,
}

impl SeqGh {
    fn new(answers: &[(&str, bool, &str, &str)]) -> Self {
        let answers = answers
            .iter()
            .map(|(needle, success, stdout, stderr)| {
                (
                    (*needle).to_string(),
                    GhRun {
                        success: *success,
                        stdout: (*stdout).to_string(),
                        stderr: (*stderr).to_string(),
                    },
                )
            })
            .collect();
        Self {
            answers: std::cell::RefCell::new(answers),
            seen: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.borrow().clone()
    }
}

impl GhRunner for SeqGh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        let joined = args.join(" ");
        self.seen.borrow_mut().push(joined.clone());
        let mut answers = self.answers.borrow_mut();
        let at = answers
            .iter()
            .position(|(needle, _)| joined.contains(needle.as_str()))
            .ok_or_else(|| anyhow::anyhow!("SeqGh: no answer left for `gh {joined}`"))?;
        Ok(answers.remove(at).1)
    }
}

const MISSING_CONVENTION: &str = "could not add label: 'trusty-mpm' not found";

/// REGRESSION (#8431): the create no longer fails on a repository that lacks
/// the `trusty-mpm` label — the label is created (without `--force`) and the
/// create retried with it.
///
/// Test: this function IS the test.
#[test]
fn pr_8431_a_missing_convention_label_is_created_and_the_create_retried() {
    assert_eq!(
        super::missing_label::missing_label(MISSING_CONVENTION),
        Some("trusty-mpm")
    );
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = SeqGh::new(&[
        ("label create ws/", true, "", ""),
        ("pr create", false, "", MISSING_CONVENTION),
        ("label create trusty-mpm", true, "", ""),
        ("pr create", true, "https://github.com/o/r/pull/4242\n", ""),
    ]);
    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("the PR opens"),
        super::EXIT_OK
    );
    let seen = gh.seen();
    let seed = seen
        .iter()
        .find(|c| c.starts_with("label create trusty-mpm"))
        .expect("the missing label was created");
    assert!(
        !seed.contains("--force"),
        "never restyle a project's label: {seed}"
    );
    let creates: Vec<&String> = seen.iter().filter(|c| c.starts_with("pr create")).collect();
    assert_eq!(creates.len(), 2, "{seen:?}");
    assert!(creates[1].contains("--label trusty-mpm"), "{}", creates[1]);
}

/// #8431: a label that cannot be created is dropped with a warning; the PR
/// still opens with its other label.
///
/// Test: this function IS the test.
#[test]
fn pr_8431_a_label_that_cannot_be_created_is_dropped_with_a_warning() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = SeqGh::new(&[
        ("label create ws/", true, "", ""),
        ("pr create", false, "", MISSING_CONVENTION),
        (
            "label create trusty-mpm",
            false,
            "",
            "HTTP 403: Must have admin rights",
        ),
        ("pr create", true, "https://github.com/o/r/pull/4242\n", ""),
    ]);
    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("the PR opens"),
        super::EXIT_OK
    );
    let retry = gh
        .seen()
        .into_iter()
        .filter(|c| c.starts_with("pr create"))
        .nth(1)
        .expect("a retried create");
    assert!(!retry.contains("--label trusty-mpm"), "{retry}");
    assert!(retry.contains("--label ws/tm-test-01"), "{retry}");
}

/// #8431 error arm: only "label not found" is recovered. Any other create
/// failure — including a missing label the plan never applied — still fails,
/// with no label created and no retry.
///
/// Test: this function IS the test.
#[test]
fn pr_8431_other_create_failures_still_fail() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    for stderr in [
        "GraphQL: Could not resolve to a Repository with the name 'o/r'",
        "could not add label: 'someone-else' not found",
    ] {
        let gh = SeqGh::new(&[
            ("label create ws/", true, "", ""),
            ("pr create", false, "", stderr),
        ]);
        let err = open::run(&gh, &args, &FakePreflight::ok()).expect_err("still a failure");
        assert!(format!("{err:#}").contains(stderr), "{err:#}");
        let creates = gh
            .seen()
            .iter()
            .filter(|c| c.starts_with("pr create") || c.starts_with("label create trusty"))
            .count();
        assert_eq!(creates, 1, "one create, no seed, no retry: {:?}", gh.seen());
    }
}

/// Review follow-up on #8431: recovery covers ONLY the convention label. A
/// missing `ws/<session>` label — seeded before create per #7513 — still
/// fails `tm pr open` loudly, never silently dropped, even though it is one
/// of the plan's own `create_labels()`.
///
/// Test: this function IS the test.
#[test]
fn pr_8431_a_missing_workstream_label_still_fails_loudly() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let stderr = "could not add label: 'ws/tm-test-01' not found";
    let gh = SeqGh::new(&[
        ("label create ws/", true, "", ""),
        ("pr create", false, "", stderr),
    ]);
    let err = open::run(&gh, &args, &FakePreflight::ok()).expect_err("still a failure");
    assert!(format!("{err:#}").contains(stderr), "{err:#}");
    let creates = gh
        .seen()
        .iter()
        .filter(|c| c.starts_with("pr create") || c.starts_with("label create trusty"))
        .count();
    assert_eq!(creates, 1, "one create, no seed, no retry: {:?}", gh.seen());
}

/// Review follow-up on #8431: `gh label create` losing a race — another
/// process (or GitHub's own read-after-write lag) created the convention
/// label first — reports "already exists" and a non-zero exit, but the
/// label the retry needs is there either way, so the retry still proceeds.
///
/// Test: this function IS the test.
#[test]
fn pr_8431_a_label_created_concurrently_counts_as_seeded() {
    let (_d, path) = scratch_body(&full_body());
    let args = open_args(&path.to_string_lossy());
    let gh = SeqGh::new(&[
        ("label create ws/", true, "", ""),
        ("pr create", false, "", MISSING_CONVENTION),
        (
            "label create trusty-mpm",
            false,
            "",
            "HTTP 422: Label \"trusty-mpm\" already exists",
        ),
        ("pr create", true, "https://github.com/o/r/pull/4242\n", ""),
    ]);
    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("the PR opens"),
        super::EXIT_OK
    );
    let seen = gh.seen();
    let creates: Vec<&String> = seen.iter().filter(|c| c.starts_with("pr create")).collect();
    assert_eq!(creates.len(), 2, "{seen:?}");
    assert!(creates[1].contains("--label trusty-mpm"), "{}", creates[1]);
}
