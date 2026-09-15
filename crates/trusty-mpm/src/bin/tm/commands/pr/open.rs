//! `tm pr open` — pre-flight the PR, then spawn `gh pr create` (#6653).
//!
//! Why: the four checks this runs are all mechanical, all written down in
//! `tm-workflow.md`, and all currently the agent's to remember: the
//! nine-field body contract, the exact attribution footer, the shipped
//! `--assignee @me --label trusty-mpm --label ws/<session>` defaults, and the
//! changelog fragment the diff owes. Each failure is cheap to catch here and
//! expensive to catch later — a thin body reaches the review gate, a missing
//! fragment reaches CI, a missing `ws/` label is never noticed at all.
//! What: [`run`] validates, then either prints the assembled argv
//! (`--dry-run`) or runs `gh pr create`. A pre-flight failure exits
//! [`super::EXIT_CHECK_FAILED`] naming the check, and `gh` is never spawned.
//! Once the PR exists the contract changes (#7869): every later step reports
//! against a NAMED PR, and metadata that could not be applied exits
//! [`super::EXIT_PARTIAL`] rather than hiding the number the caller needs.
//! Test: the sibling `tests.rs` — `open_*`, `pr_7869_*`.

use std::path::Path;

use anyhow::Context as _;

use trusty_mpm::core::component_labels::CrateOwnership;
use trusty_mpm::core::policy_labels;
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use super::body::{self, IssueLink};
use super::metadata_apply;
use super::{EXIT_CHECK_FAILED, EXIT_OK, EXIT_PARTIAL, GhRunner, argv};
use crate::cli::PrOpenArgs;

// #6918: both label names come from `core::policy_labels`, the crate's one
// policy table. This module used to spell them itself — a `FRAMEWORK_LABEL`
// constant and a `format!("ws/{}")` that skipped the 50-char truncation
// `policy_labels::workstream_label` applies, so a long session name produced a
// PR label that did not match the one `seed-labels` and launch had created.

/// The two pre-flight facts `tm pr open` cannot compute from its own inputs.
///
/// Why: the workstream session name comes from the environment or tmux, and
/// the changelog verdict comes from running a repo script. Both are real
/// side-effecting probes, so they sit behind a seam and the tests drive a
/// fake — otherwise no test of the open path could run without tmux and a
/// git checkout.
/// What: the session-name resolver and the changelog gate.
/// Test: `FakePreflight` in `tests.rs`.
pub(crate) trait Preflight {
    /// The workstream session name, or `None` when it cannot be resolved.
    fn session_name(&self) -> Option<String>;
    /// Run the changelog-fragment gate for `origin/<base>...<head>` (#7747).
    ///
    /// `head` is [`diff_head`]'s answer — `HEAD` when the caller named no
    /// `--head`. An implementation that cannot judge the named head answers
    /// [`ChangelogVerdict::HeadElsewhere`] rather than a verdict about some
    /// other ref.
    fn changelog_gate(&self, base: &str, head: &str) -> anyhow::Result<ChangelogVerdict>;
    /// Where the local `origin/<base>` stands against the remote (#7748).
    ///
    /// Why: every gate below diffs `origin/<base>...<head>`, and so does the
    /// credential scan that runs before the push this PR documents. All of them
    /// read a LOCAL ref that can be hundreds of commits behind — measured twice
    /// on 2026-09-13, one stale base would have put ~1,270 unrelated paths into
    /// a scan. Verifying it here means one probe covers every diff in the run.
    /// What: the verdict;
    /// [`trusty_mpm::core::base_ref_freshness::BaseFreshness::refusal`] decides.
    /// `mode` is the caller's write permission — a `--dry-run` passes
    /// `CompareOnly`, because a fetch moves a ref every worktree of the clone
    /// shares and a preview must not.
    fn base_freshness(
        &self,
        base: &str,
        mode: trusty_mpm::core::base_ref_freshness::RefreshMode,
    ) -> trusty_mpm::core::base_ref_freshness::BaseFreshness;
    /// Paths `git diff --name-only origin/<base>...<head>` reports (#7274).
    ///
    /// Why: the PR's component labels are the crates these paths belong to, so
    /// this probe is what makes the label derivation real. It sits behind the
    /// same seam as the other two rather than beside them, so the tests keep
    /// driving one fake.
    /// What: repository-relative paths. An error means the diff could not be
    /// read, which downgrades the label derivation to a warning.
    fn changed_paths(&self, base: &str, head: &str) -> anyhow::Result<Vec<String>>;
    /// The workspace crate ownership used to label those paths (#7274).
    fn ownership(&self) -> CrateOwnership;
    /// Where the PR just opened is recorded for post-merge cleanup (#7275).
    ///
    /// Why: the registry is real state under `~/.trusty-mpm`, and the daemon's
    /// sweep acts on it by DELETING branches. A unit test that wrote a live
    /// entry would arm that sweep against a repository the test invented, so
    /// the path is injected through the seam that already carries this
    /// command's environment rather than resolved at the write site.
    /// Test: `open_records_the_new_pr_for_cleanup`,
    /// `open_creates_and_reports`.
    fn cleanup_registry(&self) -> trusty_mpm::core::pr_cleanup::CleanupRegistry;
}

/// What the changelog-fragment gate said.
///
/// Why: "the script is absent" is not the same answer as "the script passed",
/// and reporting the first as the second would let a repo without the gate
/// look like a repo that cleared it.
/// What: `Pass`, `Skipped` (no such script in this repo), `Fail` carrying the
/// script's own output, or `HeadElsewhere` — the gate declining to answer about
/// a `--head` it cannot diff (#7747).
/// Test: `open_reports_changelog_failure`, `open_docs_only_skips_the_changelog_gate`,
/// `open_head_without_docs_only_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChangelogVerdict {
    /// The gate ran and exited 0.
    Pass,
    /// No `scripts/check_changelog_fragment.sh` in this repository.
    Skipped,
    /// The gate ran and exited non-zero; the string is its output, trimmed.
    Fail(String),
    /// The named `--head` is not the commit the checkout stands on, so the
    /// gate could not judge it and did not run (#7747).
    HeadElsewhere,
}

/// Production [`Preflight`].
///
/// Why/What/Test: resolves the session name from `$TM_SESSION_NAME` first and
/// tmux second — the same resolution `tm-workflow.md`'s shipped-defaults
/// section describes (`tmux display-message -p '#{session_name}'`) — reusing
/// the crate's existing bounded tmux probe rather than adding a second one.
/// The changelog gate shells to the repo's own script so this command and CI
/// can never disagree about the verdict. Exercised live; the decision logic
/// it feeds is covered against `FakePreflight`.
pub(crate) struct RealPreflight;

impl Preflight for RealPreflight {
    fn session_name(&self) -> Option<String> {
        std::env::var("TM_SESSION_NAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(crate::commands::statusline::branch::tmux_session_name)
    }

    fn cleanup_registry(&self) -> trusty_mpm::core::pr_cleanup::CleanupRegistry {
        trusty_mpm::core::pr_cleanup::CleanupRegistry::production()
    }

    fn changelog_gate(&self, base: &str, head: &str) -> anyhow::Result<ChangelogVerdict> {
        let root = repo_root()?;
        // #7747: the script takes `--base` and always diffs it against the
        // checkout's HEAD — it has no `--head` of its own. A named head that
        // resolves to a DIFFERENT commit therefore cannot be judged here, and
        // saying `Pass` about the checkout instead would clear a source PR
        // against a diff it does not contain (#7282 round 5).
        if !head_is_checkout(&root, head) {
            return Ok(ChangelogVerdict::HeadElsewhere);
        }
        let script = root.join("scripts/check_changelog_fragment.sh");
        if !script.exists() {
            return Ok(ChangelogVerdict::Skipped);
        }
        let out = std::process::Command::new("bash")
            .arg(&script)
            .arg("--base")
            .arg(format!("origin/{base}"))
            .current_dir(&root)
            .output()
            .with_context(|| format!("cannot run {}", script.display()))?;
        if out.status.success() {
            return Ok(ChangelogVerdict::Pass);
        }
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(ChangelogVerdict::Fail(text.trim().to_string()))
    }

    fn base_freshness(
        &self,
        base: &str,
        mode: trusty_mpm::core::base_ref_freshness::RefreshMode,
    ) -> trusty_mpm::core::base_ref_freshness::BaseFreshness {
        // #7748: rooted at the checkout, so the probe reads the same ref store
        // every diff below reads.
        match repo_root() {
            Ok(root) => trusty_mpm::core::base_ref_freshness::check(
                &trusty_mpm::core::base_ref_freshness::RealBaseRefs::at(&root),
                base,
                mode,
            ),
            Err(e) => trusty_mpm::core::base_ref_freshness::BaseFreshness::Undetermined {
                reason: format!("{e:#}"),
            },
        }
    }

    fn changed_paths(&self, base: &str, head: &str) -> anyhow::Result<Vec<String>> {
        let root = repo_root()?;
        // Three-dot: the changes THIS branch made, never main's own drift.
        // #7282: `head` is the `--head` branch when one was named, so the labels
        // describe the branch being opened rather than the checkout's HEAD.
        let out = std::process::Command::new("git")
            .args(["diff", "--name-only", &format!("origin/{base}...{head}")])
            .current_dir(&root)
            .output()
            .context("cannot run `git diff --name-only`")?;
        anyhow::ensure!(
            out.status.success(),
            "`git diff --name-only origin/{base}...{head}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn ownership(&self) -> CrateOwnership {
        CrateOwnership::resolve(std::env::current_dir().ok().as_deref())
    }
}

/// The `--head` branch, when the caller named a non-blank one.
///
/// Why: a blank `--head ""` must read as "not supplied" rather than reaching
/// `gh` as an empty head, which it rejects with a message about the current
/// branch — the exact confusion #7282 was.
/// What: trims, and drops the empty result.
/// Test: `open_head_is_ignored_when_blank`.
fn head_branch(args: &PrOpenArgs) -> Option<&str> {
    args.head
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
}

/// The refusal a `--head` earns when the gate could not judge it.
///
/// Why (#7282 round 5, relaxed by #7747): `scripts/check_changelog_fragment.sh`
/// accepts only `--base`, `--staged` and `--file`, so
/// [`Preflight::changelog_gate`] can diff nothing but the CHECKOUT's HEAD. That
/// makes the gate wrong for a head standing somewhere else — but not for a head
/// that IS the checkout's commit under another name, which is the case #7747
/// reported: a local branch pushed under a different remote name needs `--head`
/// only so `gh` stops resolving the head through `@{push}`. The verdict, not the
/// branch name, decides.
/// What: the message for [`ChangelogVerdict::HeadElsewhere`], naming the head
/// and the caller's two ways forward.
/// Test: `open_head_without_docs_only_is_refused`,
/// `open_head_without_docs_only_never_calls_gh`,
/// `pr_7747_a_head_that_is_the_checkout_opens_a_source_pr`.
fn head_elsewhere_refusal(args: &PrOpenArgs) -> String {
    let head = head_branch(args).unwrap_or("HEAD");
    format!(
        "--head `{head}` is not the commit this checkout stands on, and \
         scripts/check_changelog_fragment.sh takes only --base — it would judge \
         origin/{}...HEAD, this checkout, rather than `{head}`. Check `{head}` out, \
         or pass --docs-only if this PR changes no crate source.",
        args.base
    )
}

/// Does `head` name the commit the checkout's `HEAD` is on? (#7747)
///
/// Why: this is what decides whether the changelog gate's `origin/<base>...HEAD`
/// diff is the PR's own diff. Resolving `origin/<head>` as well as `<head>` is
/// what covers the reported case — a local branch whose pushed remote branch
/// carries a different name, where only the remote ref resolves.
/// What: true when `head` is the literal `HEAD`, or when either candidate ref
/// resolves to the same commit as `HEAD`. An unresolvable head is false: a head
/// this repository cannot name is one the gate cannot judge.
/// Test: `head_rev_candidates_try_the_remote_ref`, and
/// `pr_7747_a_head_that_is_the_checkout_opens_a_source_pr` through the seam.
fn head_is_checkout(root: &Path, head: &str) -> bool {
    if head == "HEAD" {
        return true;
    }
    let Some(here) = resolve_commit(root, "HEAD") else {
        return false;
    };
    head_rev_candidates(head)
        .iter()
        .any(|rev| resolve_commit(root, rev).is_some_and(|c| c == here))
}

/// The refs a `--head <name>` may mean, in resolution order (#7747).
///
/// What: the name as given, then `origin/<name>` — the pushed branch, which is
/// the only one that resolves when the local branch carries a different name.
/// Test: `head_rev_candidates_try_the_remote_ref`.
pub(crate) fn head_rev_candidates(head: &str) -> [String; 2] {
    [head.to_string(), format!("origin/{head}")]
}

/// The commit `rev` names in `root`, or `None` when it names none.
fn resolve_commit(root: &Path, rev: &str) -> Option<String> {
    let peeled = format!("{rev}^{{commit}}");
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", peeled.as_str()])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// The revision the pre-flight diffs against `origin/<base>`.
///
/// Why (#7282): the changelog gate and the component-label diff both asked
/// about `HEAD`, which is the checkout's current branch — not the branch a
/// `--head` caller is opening. Reading main's own state there answers "no
/// changes" for every such PR, so the gate passes vacuously and the PR gets no
/// component labels.
/// What: the `--head` branch when one was named, else `HEAD`.
/// Test: `open_head_drives_the_preflight_diff_revision`.
pub(crate) fn diff_head(args: &PrOpenArgs) -> &str {
    head_branch(args).unwrap_or("HEAD")
}

/// The repository root of the current working directory.
fn repo_root() -> anyhow::Result<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("cannot run `git rev-parse --show-toplevel`")?;
    anyhow::ensure!(out.status.success(), "not inside a git repository");
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}

/// Everything `tm pr open` decided before it was allowed to call `gh`.
///
/// Why: the argv and the "which fields were supplied" report are both derived
/// from the same validated inputs, and building them as a value keeps the
/// dry-run path and the real path provably identical.
/// What: the `gh pr create` argv, the resolved workstream label, and the
/// contract fields the body actually filled.
/// Test: `open_dry_run_never_calls_gh`, `open_argv_carries_shipped_defaults`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OpenPlan {
    /// The full `gh` argv, without the `gh` itself.
    pub(crate) argv: Vec<String>,
    /// The `ws/<session>` label attached to the PR.
    pub(crate) workstream_label: String,
    /// The `gh label create …` argv that makes that label exist first (#7513).
    ///
    /// Why: `gh pr create --label ws/<x>` fails outright on a label the
    /// repository has never seen, and the caller most likely to hit that is the
    /// one that cannot seed it — `session_context_pause` publishes from inside
    /// the daemon, where `tm issue seed-labels`' tmux-session read finds
    /// nothing. Carrying the seed in the PLAN rather than spelling it at the
    /// call site is what keeps `--dry-run` a faithful rehearsal of the real run.
    /// What: `create_label_argv` for the workstream label, with `--force`
    /// (`ws/` is the framework's own namespace, so refreshing it is safe and
    /// makes the seed idempotent). The convention label is deliberately NOT
    /// seeded here: it is an ordinary repo label a project may have styled, and
    /// `--force` would rewrite that on every PR.
    pub(crate) label_seed_argv: Vec<String>,
    /// Contract fields present and non-empty, in contract order.
    pub(crate) supplied: Vec<&'static str>,
    /// The body as it will be sent, after the issue link was applied.
    pub(crate) body: String,
    /// The assignee `gh pr create` was told to set (#7869).
    ///
    /// Why: when the create exits non-zero AFTER creating the PR, the retry has
    /// to re-apply exactly what the create was applying. Reading it back off
    /// the argv would be a second spelling of the same fact.
    /// Test: `pr_7869_a_create_that_fails_after_creating_reports_the_pr_and_retries`.
    pub(crate) assignee: String,
}

impl OpenPlan {
    /// The labels `gh pr create` carries: the convention label and `ws/<x>`.
    ///
    /// Test: `pr_7869_a_create_that_fails_after_creating_reports_the_pr_and_retries`.
    pub(crate) fn create_labels(&self) -> Vec<String> {
        vec![
            policy_labels::CONVENTION_LABEL.to_string(),
            self.workstream_label.clone(),
        ]
    }
}

/// Validate the inputs and assemble the `gh pr create` invocation.
///
/// Why: every check lives here rather than in [`run`] so the whole gate is one
/// pure function of (args, body text, session name, changelog verdict) — which
/// is what makes "each missing field exits 2" testable without a `gh` or a
/// repository.
/// What: in order — the body contract and footer ([`body::validate`], the
/// contract half skipped under `--minimal`, #7615), the `Refs`/`Closes` rule
/// ([`body::apply_issue_link`]), the workstream label, and the changelog
/// verdict, whose [`ChangelogVerdict::HeadElsewhere`] arm is the `--head`
/// refusal (#7747). Returns every failure found, not just the first, so one run
/// fixes them all. Both labels and the assignee come from `core::policy_labels`
/// and the resolved `agents.ticketing` block (#6918), never from constants
/// spelled here.
/// Test: `open_reports_each_missing_field`, `open_rejects_bad_footer`,
/// `open_requires_a_session_name`, `open_reports_changelog_failure`,
/// `open_docs_only_skips_the_changelog_gate`,
/// `open_head_without_docs_only_is_refused`,
/// `open_head_with_docs_only_plans`,
/// `pr_7615_minimal_skips_the_heading_contract`,
/// `open_labels_come_from_the_policy_table`,
/// `open_assignee_comes_from_the_ticketing_block`.
pub(crate) fn plan(
    args: &PrOpenArgs,
    body_text: &str,
    session: Option<&str>,
    changelog: ChangelogVerdict,
    // #6918: the resolved `agents.ticketing` standard supplies the assignee.
    ticketing: &ResolvedTicketing,
) -> Result<OpenPlan, Vec<String>> {
    let mut failures: Vec<String> = Vec::new();

    let report = body::validate(body_text);
    // #7615: `--minimal` drops the nine-heading half for a project whose own
    // `CLAUDE.md` names a different body standard. `merge_failures` is that half
    // removed — the footer, and nothing else.
    if args.minimal {
        failures.extend(report.merge_failures());
    } else {
        failures.extend(report.failures());
    }

    let link = if args.closes {
        IssueLink::Closes
    } else {
        IssueLink::Refs
    };
    let linked = match body::apply_issue_link(body_text, args.issue, link) {
        Ok(text) => text,
        Err(e) => {
            failures.push(e);
            body_text.to_string()
        }
    };

    let session = session.map(str::trim).filter(|s| !s.is_empty());
    if session.is_none() {
        failures.push(
            "cannot resolve the workstream session name for the `ws/<session>` label; \
             set $TM_SESSION_NAME, run inside tmux, or pass --session <name>"
                .to_string(),
        );
    }

    match changelog {
        ChangelogVerdict::Pass | ChangelogVerdict::Skipped => {}
        ChangelogVerdict::Fail(output) => failures.push(format!(
            "scripts/check_changelog_fragment.sh failed for origin/{}...HEAD \
             (pass --docs-only if this PR changes no crate source):\n{output}",
            args.base
        )),
        // #7747: the gate declined to answer about this head, so `--head` is
        // refused here rather than opening a PR on an unjudged diff.
        ChangelogVerdict::HeadElsewhere => failures.push(head_elsewhere_refusal(args)),
    }

    if !failures.is_empty() {
        return Err(failures);
    }

    // #6918: `session` is Some here — an unresolved name is a failure above —
    // and the shared derivation refuses a blank name the same way, so the
    // `None` arm is unreachable rather than a second blankness rule.
    let Some(workstream) = session.and_then(policy_labels::workstream_label) else {
        return Err(vec![
            "cannot derive the `ws/<session>` label from the resolved session name".to_string(),
        ]);
    };
    // #7513: the label the PR is about to carry has to exist first.
    let label_seed_argv = policy_labels::create_label_argv(
        &workstream,
        args.repo.as_deref().filter(|r| !r.trim().is_empty()),
        policy_labels::is_owned_namespace(&workstream.name),
    );
    let label = workstream.name;
    let mut gh_argv = argv(&["pr", "create"]);
    if let Some(repo) = args.repo.as_deref().filter(|r| !r.trim().is_empty()) {
        gh_argv.push("--repo".to_string());
        gh_argv.push(repo.to_string());
    }
    gh_argv.push("--base".to_string());
    gh_argv.push(args.base.clone());
    // #7282: `gh` otherwise reads the head from the checkout's current branch,
    // which is wrong for any caller whose branch is not checked out.
    if let Some(head) = head_branch(args) {
        gh_argv.push("--head".to_string());
        gh_argv.push(head.to_string());
    }
    gh_argv.push("--title".to_string());
    gh_argv.push(args.title.clone());
    gh_argv.push("--body".to_string());
    gh_argv.push(linked.clone());
    gh_argv.push("--assignee".to_string());
    gh_argv.push(ticketing.default_assignee.clone());
    gh_argv.push("--label".to_string());
    gh_argv.push(policy_labels::CONVENTION_LABEL.to_string());
    gh_argv.push("--label".to_string());
    gh_argv.push(label.clone());

    Ok(OpenPlan {
        argv: gh_argv,
        workstream_label: label,
        label_seed_argv,
        supplied: report.supplied.iter().map(|f| f.heading()).collect(),
        body: linked,
        assignee: ticketing.default_assignee.clone(),
    })
}

/// Run `tm pr open`.
///
/// Why: this is the entry point the `version-control` agent calls instead of
/// hand-assembling `gh pr create`, so its contract is the exit code — 0 when
/// the PR exists (or the dry run printed its argv), 2 when a check failed and
/// `gh` was never spawned.
/// What: reads the body file, resolves the session name and the changelog
/// verdict through [`Preflight`], calls [`plan`], and then either prints the
/// argv (`--dry-run`) or runs it. On success prints the PR number and URL from
/// `gh`'s own output plus the one-line list of supplied body fields; `--rung`
/// is echoed there so the claimed test-ladder rung is visible at open time.
///
/// #7748: the run starts by verifying `origin/<base>` against the remote, so no
/// gate here — and no credential scan taken over the same range — diffs a base
/// the checkout never fetched.
/// Test: `open_dry_run_never_calls_gh`, `open_creates_and_reports`,
/// `open_failure_exits_two_without_calling_gh`, `open_rejects_an_empty_body_file`,
/// `pr_7747_a_head_that_is_the_checkout_opens_a_source_pr`,
/// `pr_7748_a_stale_base_refuses_before_gh_is_called`.
pub(crate) fn run<R: GhRunner, P: Preflight>(
    gh: &R,
    args: &PrOpenArgs,
    pre: &P,
) -> anyhow::Result<i32> {
    let body_text = read_body(&args.body_file)?;
    // #7748: every gate below, and the credential scan that precedes the push,
    // diffs `origin/<base>...<head>`. Verify that base against the remote FIRST
    // — a stale one silently widens each of those diffs.
    // A preview compares without fetching: `refs/remotes/origin/<base>` is
    // shared by every worktree of the clone (#7748 round 2).
    let mode = if args.dry_run {
        trusty_mpm::core::base_ref_freshness::RefreshMode::CompareOnly
    } else {
        trusty_mpm::core::base_ref_freshness::RefreshMode::FetchOnDrift
    };
    if let Some(reason) = pre.base_freshness(&args.base, mode).refusal() {
        eprintln!(
            "tm pr open: origin/{} is not usable as a diff base; gh was not called",
            args.base
        );
        eprintln!("  - {reason}");
        eprintln!(
            "  fix it with `git fetch origin {}`, then re-run — and re-run the credential scan \
             against the refreshed base",
            args.base
        );
        return Ok(EXIT_CHECK_FAILED);
    }
    let session = match args.session.clone() {
        Some(s) => Some(s),
        None => pre.session_name(),
    };
    let changelog = if args.docs_only {
        ChangelogVerdict::Skipped
    } else {
        // #7747: the gate is asked about the branch being opened, not about
        // whatever the checkout happens to stand on.
        pre.changelog_gate(&args.base, diff_head(args))?
    };

    // #6918: a malformed `agents.ticketing` block is an error here, not a
    // silent revert to the built-in standard.
    let ticketing = trusty_mpm::core::trusty_tools_config::resolve_ticketing(
        &trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load(),
    )?;

    let plan = match plan(args, &body_text, session.as_deref(), changelog, &ticketing) {
        Ok(p) => p,
        Err(failures) => {
            eprintln!(
                "tm pr open: {} check(s) failed; gh was not called",
                failures.len()
            );
            for f in &failures {
                eprintln!("  - {f}");
            }
            // #7574: a missing heading is fixed by writing the skeleton, so
            // print the skeleton rather than leaving the author to infer it.
            if let Some(skeleton) = skeleton_hint(&failures) {
                eprintln!("\nrequired body skeleton — paste this and fill each section:\n");
                eprintln!("{skeleton}");
            }
            return Ok(EXIT_CHECK_FAILED);
        }
    };

    if args.dry_run {
        println!("gh {}", shell_render(&plan.label_seed_argv));
        println!("gh {}", shell_render(&plan.argv));
        return Ok(EXIT_OK);
    }

    // #7513: seed the `ws/<session>` label before the create that applies it.
    // Best-effort by design: the seed is not the deliverable, and a repository
    // where it fails but the label already exists must still open its PR. It
    // does NOT fail open in the sense that matters — a label that genuinely
    // cannot be created makes `gh pr create --label` fail loudly on the very
    // next line, which is the error the operator needs to see.
    match gh.run(&plan.label_seed_argv) {
        Ok(seed) if !seed.success => eprintln!(
            "  warning: could not seed the `{}` label: {}",
            plan.workstream_label,
            seed.stderr.trim()
        ),
        Err(e) => eprintln!(
            "  warning: could not seed the `{}` label: {e:#}",
            plan.workstream_label
        ),
        Ok(_) => {}
    }

    let out = gh.run(&plan.argv)?;
    // #7869: `gh pr create` creates the PR and THEN applies the assignee and
    // labels over separate API calls. A 502 on one of those exits non-zero with
    // the PR already created, and bailing here printed no number at all — the
    // caller had to find PR #7918 with `gh pr list --head`. The URL on stdout is
    // the proof the PR exists, so that case is reported, not swallowed.
    let created_partially = if out.success {
        false
    } else {
        anyhow::ensure!(
            created_pr_url(&out.stdout).is_some(),
            "`gh pr create` failed: {}",
            out.stderr.trim()
        );
        eprintln!(
            "tm pr open: `gh pr create` exited non-zero AFTER creating the PR: {}",
            out.stderr.trim()
        );
        true
    };
    let url = out
        .stdout
        .lines()
        .rev()
        .find(|l| l.contains("http"))
        .unwrap_or("")
        .trim();
    let number = url.rsplit('/').next().unwrap_or("?");
    println!("opened PR #{number} — {url}");
    println!(
        "  labels: {}, {}",
        policy_labels::CONVENTION_LABEL,
        plan.workstream_label
    );
    if let Some(rung) = args.rung {
        println!("  test-ladder rung claimed: {rung}");
    }
    println!("  body fields supplied: {}", plan.supplied.join(", "));

    let mut missing: Vec<String> = Vec::new();
    // #7869: retry the create's own metadata step once, before the derived half.
    if created_partially {
        missing.extend(metadata_apply::retry_create_defaults(gh, args, number, &plan).missing());
    }
    // #7274: the PR half of the labels/project/milestone standard, applied
    // after the PR exists because every step needs its number.
    missing.extend(metadata_apply::apply(gh, args, pre, number, &plan.body).missing());
    // #7275: record the PR so the daemon can clean up after it merges.
    record_for_cleanup(pre, url, number);

    if missing.is_empty() {
        return Ok(EXIT_OK);
    }
    // #7869: the PR exists, so the exit code says "partially applied", never
    // "the check failed and gh was not called".
    eprintln!(
        "tm pr open: PR #{number} EXISTS ({url}) but this metadata could not be applied: {}",
        missing.join("; ")
    );
    Ok(EXIT_PARTIAL)
}

/// The paste-able body skeleton, when a check failed on a missing heading.
///
/// Why (#7574): `tm pr open` named each missing heading on its own line, and the
/// agent on trusty-things#253 still had to guess the skeleton and spend a second
/// invocation checking the guess. The skeleton is only ever the fix for a
/// MISSING heading, so it is offered there and nowhere else — a body that failed
/// on its footer or its changelog fragment gets no wall of headings it already
/// has.
/// What: [`body::skeleton`] when any failure line opens with
/// [`body::MISSING_FIELD_PREFIX`], else `None`.
/// Test: `pr_7574_a_missing_heading_offers_the_body_skeleton`,
/// `pr_7574_other_failures_offer_no_skeleton`.
pub(crate) fn skeleton_hint(failures: &[String]) -> Option<String> {
    failures
        .iter()
        .any(|f| f.starts_with(body::MISSING_FIELD_PREFIX))
        .then(body::skeleton)
}

/// The PR URL `gh pr create` printed, when its stdout carries a parsable one.
///
/// Why (#7869): this is the only evidence that a non-zero `gh pr create` still
/// created the PR. It is deliberately stricter than the success path's read —
/// concluding "the PR exists" off a line that merely contains `http` would turn
/// a genuine create failure into a silent success.
/// What: the last stdout line that starts with `http` and names a `/pull/` path.
/// Test: `pr_7869_a_create_that_fails_after_creating_reports_the_pr_and_retries`,
/// `pr_7869_a_create_that_fails_with_no_url_is_still_an_error`.
fn created_pr_url(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| l.starts_with("http") && l.contains("/pull/"))
}

/// Record the PR just opened, so the daemon can clean up after it merges
/// (#7275, owner amendment 2026-09-09).
///
/// Why: the periodic trigger has to know WHICH pull requests to watch. Every PR
/// this harness creates comes through here, so this is the one place that
/// knows — and the only place a second, competing registry could be avoided.
/// What: appends a registry entry keyed by (repo, number), taking BOTH from the
/// URL `gh pr create` just printed rather than asking `gh` again — a second
/// round trip could resolve a different remote than the one that took the push.
/// The checkout the PR was opened from is recorded too, so cleanup runs its git
/// commands in the right tree. BEST-EFFORT: the PR is already open, so a failed
/// write is a warning on stderr, never a failed `tm pr open` — the operator can
/// still run `tm pr cleanup <n>` by hand.
/// Test: `open_records_the_new_pr_for_cleanup`,
/// `open_records_nothing_for_an_unparsable_url`.
fn record_for_cleanup<P: Preflight>(pre: &P, url: &str, number: &str) {
    let Ok(pr) = number.trim().parse::<u64>() else {
        eprintln!("tm pr open: could not read the new PR's number; not recording it for cleanup");
        return;
    };
    let (Some(repo), Ok(root)) = (repo_from_pr_url(url), std::env::current_dir()) else {
        eprintln!("tm pr open: could not resolve the repo or cwd; not recording #{pr} for cleanup");
        return;
    };
    let entry = trusty_mpm::core::pr_cleanup::OpenedPr {
        pr,
        repo,
        repo_root: root,
        opened_at: chrono::Utc::now(),
        cleaned_at: None,
    };
    if let Err(e) = pre.cleanup_registry().record_open(entry) {
        eprintln!("tm pr open: could not record #{pr} for post-merge cleanup: {e:#}");
    }
}

/// `owner/repo` from a `https://<host>/<owner>/<repo>/pull/<n>` URL.
///
/// Test: `open_records_the_new_pr_for_cleanup`,
/// `open_records_nothing_for_an_unparsable_url`.
fn repo_from_pr_url(url: &str) -> Option<String> {
    let (before, _) = url.trim().rsplit_once("/pull/")?;
    let mut parts = before.rsplitn(3, '/');
    let repo = parts.next()?;
    let owner = parts.next()?;
    (!repo.is_empty() && !owner.is_empty() && !owner.contains(':'))
        .then(|| format!("{owner}/{repo}"))
}

/// Read the body file, rejecting an empty one before anything else.
fn read_body(path: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read PR body file {}", path.display()))?;
    anyhow::ensure!(
        !text.trim().is_empty(),
        "PR body file {} is empty",
        path.display()
    );
    Ok(text)
}

/// Render an argv for the `--dry-run` line, quoting anything with whitespace.
///
/// Why: the dry-run output exists so a caller can SEE the exact invocation,
/// and a multi-line `--body` pasted raw would make the line unreadable and
/// unrunnable. What it prints is a faithful shell rendering, not the argv the
/// runner uses — `GhCommand` never goes through a shell.
/// What: single-quotes any argument containing whitespace or a quote,
/// escaping embedded single quotes.
/// Test: `shell_render_quotes_multiline_body`.
pub(crate) fn shell_render(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || c == '\'' || c == '"')
            {
                format!("'{}'", a.replace('\'', r"'\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
