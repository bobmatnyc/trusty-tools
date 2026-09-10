//! `tm pr open` — pre-flight the PR, then spawn `gh pr create` (#6653).
//!
//! Why: the four checks this runs are all mechanical, all written down in
//! `tm-workflow.md`, and all currently the agent's to remember: the
//! seven-field body contract, the exact attribution footer, the shipped
//! `--assignee @me --label trusty-mpm --label ws/<session>` defaults, and the
//! changelog fragment the diff owes. Each failure is cheap to catch here and
//! expensive to catch later — a thin body reaches the review gate, a missing
//! fragment reaches CI, a missing `ws/` label is never noticed at all.
//! What: [`run`] validates, then either prints the assembled argv
//! (`--dry-run`) or runs `gh pr create`. Every failure exits
//! [`super::EXIT_CHECK_FAILED`] naming the check, and `gh` is never spawned.
//! Test: the sibling `tests.rs` — `open_*`.

use std::path::Path;

use anyhow::Context as _;

use trusty_mpm::core::component_labels::CrateOwnership;
use trusty_mpm::core::issue_audit::IssueFacts;
use trusty_mpm::core::issue_audit_gh::view_argv;
use trusty_mpm::core::policy_labels;
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use super::body::{self, IssueLink};
use super::metadata::{self, ChangedPaths, PrMetadata, RefsIssue, RefsLookup};
use super::{EXIT_CHECK_FAILED, EXIT_OK, GhRunner, argv};
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
    /// Run the changelog-fragment gate for `origin/<base>...HEAD`.
    fn changelog_gate(&self, base: &str) -> anyhow::Result<ChangelogVerdict>;
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
/// What: `Pass`, `Skipped` (no such script in this repo), or `Fail` carrying
/// the script's own output.
/// Test: `open_reports_changelog_failure`, `open_docs_only_skips_the_changelog_gate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChangelogVerdict {
    /// The gate ran and exited 0.
    Pass,
    /// No `scripts/check_changelog_fragment.sh` in this repository.
    Skipped,
    /// The gate ran and exited non-zero; the string is its output, trimmed.
    Fail(String),
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

    fn changelog_gate(&self, base: &str) -> anyhow::Result<ChangelogVerdict> {
        let root = repo_root()?;
        let script = root.join("scripts/check_changelog_fragment.sh");
        if !script.exists() {
            return Ok(ChangelogVerdict::Skipped);
        }
        // #7282: the script takes `--base` and always diffs it against the
        // checkout's HEAD — it has no `--head` of its own, which is why
        // `head_docs_only_conflict` refuses `--head` without `--docs-only`.
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

/// The refusal a `--head` earns when it is not paired with `--docs-only`.
///
/// Why (#7282 round 5): `scripts/check_changelog_fragment.sh` accepts only
/// `--base`, `--staged` and `--file`, so [`Preflight::changelog_gate`] can
/// diff nothing but `origin/<base>...HEAD` — the CHECKOUT's HEAD. A caller who
/// names `--head other-branch` from a checkout sitting on a different branch
/// therefore has the fragment gate judge a diff the PR does not contain, and a
/// source PR with no fragment passes it. Until the script can diff an explicit
/// head, refusing the pair is the only sound answer; `--docs-only` is the one
/// case where the gate's verdict does not matter, because it is skipped.
/// What: `Some(message)` when a non-blank `--head` was named without
/// `--docs-only`, else `None`.
/// Test: `open_head_without_docs_only_is_refused`,
/// `open_head_without_docs_only_never_calls_gh`,
/// `open_head_with_docs_only_plans`.
fn head_docs_only_conflict(args: &PrOpenArgs) -> Option<String> {
    let head = head_branch(args)?;
    if args.docs_only {
        return None;
    }
    Some(format!(
        "--head requires --docs-only until the changelog gate can diff an explicit head: \
         scripts/check_changelog_fragment.sh takes only --base, so it would judge \
         origin/{}...HEAD — this checkout — rather than `{head}`. \
         Run tm pr open from a checkout of `{head}` for a source PR.",
        args.base
    ))
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
fn diff_head(args: &PrOpenArgs) -> &str {
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
    /// Contract fields present and non-empty, in contract order.
    pub(crate) supplied: Vec<&'static str>,
    /// The body as it will be sent, after the issue link was applied.
    pub(crate) body: String,
}

/// Validate the inputs and assemble the `gh pr create` invocation.
///
/// Why: every check lives here rather than in [`run`] so the whole gate is one
/// pure function of (args, body text, session name, changelog verdict) — which
/// is what makes "each missing field exits 2" testable without a `gh` or a
/// repository.
/// What: in order — the `--head`/`--docs-only` pairing
/// ([`head_docs_only_conflict`]), the body contract and footer
/// ([`body::validate`]), the `Refs`/`Closes` rule ([`body::apply_issue_link`]),
/// the workstream label, and the changelog gate. Returns every failure found,
/// not just the first, so one run fixes them all. Both labels and the assignee
/// come from `core::policy_labels` and the resolved `agents.ticketing` block
/// (#6918), never from constants spelled here.
/// Test: `open_reports_each_missing_field`, `open_rejects_bad_footer`,
/// `open_requires_a_session_name`, `open_reports_changelog_failure`,
/// `open_docs_only_skips_the_changelog_gate`,
/// `open_head_without_docs_only_is_refused`,
/// `open_head_with_docs_only_plans`,
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

    // #7282 round 5: `--head` and the changelog gate cannot both be honoured,
    // so the pair is refused here — before `run` reaches `gh`.
    failures.extend(head_docs_only_conflict(args));

    let report = body::validate(body_text);
    failures.extend(report.failures());

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
        supplied: report.supplied.iter().map(|f| f.heading()).collect(),
        body: linked,
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
/// Test: `open_dry_run_never_calls_gh`, `open_creates_and_reports`,
/// `open_failure_exits_two_without_calling_gh`, `open_rejects_an_empty_body_file`.
pub(crate) fn run<R: GhRunner, P: Preflight>(
    gh: &R,
    args: &PrOpenArgs,
    pre: &P,
) -> anyhow::Result<i32> {
    let body_text = read_body(&args.body_file)?;
    let session = match args.session.clone() {
        Some(s) => Some(s),
        None => pre.session_name(),
    };
    let changelog = if args.docs_only {
        ChangelogVerdict::Skipped
    } else {
        pre.changelog_gate(&args.base)?
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
            return Ok(EXIT_CHECK_FAILED);
        }
    };

    if args.dry_run {
        println!("gh {}", shell_render(&plan.argv));
        return Ok(EXIT_OK);
    }

    let out = gh.run(&plan.argv)?;
    if !out.success {
        anyhow::bail!("`gh pr create` failed: {}", out.stderr.trim());
    }
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
    // #7274: the PR half of the labels/project/milestone standard, applied
    // after the PR exists because every step needs its number.
    apply_metadata(gh, args, pre, number, &plan.body);
    // #7275: record the PR so the daemon can clean up after it merges.
    record_for_cleanup(pre, url, number);
    Ok(EXIT_OK)
}

/// Apply the PR's component labels, milestone and projects — best-effort.
///
/// Why (#7274): the standard is now one rule over issues and PRs, and a PR's
/// share of it is entirely derived — labels from its own diff, project and
/// milestone from the issue its `Refs #N` names. None of it is worth failing
/// an open over: the PR already exists by this point, and a `tm pr open` that
/// reported failure after creating a PR would be worse than a warning. So each
/// step prints a named warning on failure and the command still exits 0.
/// What: reads the diff and the linked issue, calls [`metadata::plan`], and
/// runs one `gh pr edit`. Prints what it applied, and one line per thing the
/// standard wanted and this PR could not get.
/// Test: `open_applies_pr_metadata`, `open_without_refs_says_so`,
/// `open_survives_a_failed_metadata_edit`, `open_notes_an_unreadable_diff`,
/// `open_notes_an_unreadable_refs_issue`.
fn apply_metadata<R: GhRunner, P: Preflight>(
    gh: &R,
    args: &PrOpenArgs,
    pre: &P,
    pr: &str,
    body: &str,
) {
    // #7274 round 2: a failed read reaches `plan` as its own state, so the note
    // it prints names the failure rather than blaming an empty answer.
    let read = pre.changed_paths(&args.base, diff_head(args));
    let changed = match &read {
        Ok(paths) => ChangedPaths::Read(&paths[..]),
        Err(e) => {
            eprintln!("  warning: the diff could not be read: {e:#}");
            ChangedPaths::Unreadable
        }
    };
    let number = metadata::first_refs_issue(body);
    let issue = number.and_then(|n| match refs_issue(gh, args, n) {
        Ok(issue) => Some(issue),
        Err(e) => {
            eprintln!("  warning: issue #{n} could not be read: {e:#}");
            None
        }
    });
    let refs = match (number, issue.as_ref()) {
        (_, Some(issue)) => RefsLookup::Found(issue),
        (Some(n), None) => RefsLookup::Unreadable(n),
        (None, None) => RefsLookup::Absent,
    };
    let meta = metadata::plan(refs, changed, &pre.ownership());
    if !meta.is_empty() {
        let edit = metadata::edit_argv(pr, args.repo.as_deref(), &meta);
        match gh.run(&edit) {
            Ok(out) if out.success => report_applied(&meta),
            Ok(out) => eprintln!("  warning: `gh pr edit` failed: {}", out.stderr.trim()),
            Err(e) => eprintln!("  warning: `gh pr edit` could not run: {e:#}"),
        }
    }
    for note in &meta.notes {
        println!("  {note}");
    }
}

/// Print what the edit actually applied.
fn report_applied(meta: &PrMetadata) {
    if !meta.labels.is_empty() {
        println!("  component labels: {}", meta.labels.join(", "));
    }
    if let Some(milestone) = &meta.milestone {
        println!("  milestone: {milestone}");
    }
    if !meta.projects.is_empty() {
        println!("  projects: {}", meta.projects.join(", "));
    }
}

/// Read the `Refs` issue's milestone and projects through `gh`.
///
/// Why: `view_argv` is the issue audit's own field list, so this reads the
/// issue exactly the way `tm issue audit` does rather than inventing a second
/// `--json` spelling that could drift from it.
fn refs_issue<R: GhRunner>(gh: &R, args: &PrOpenArgs, number: u64) -> anyhow::Result<RefsIssue> {
    let mut view = view_argv(number);
    if let Some(repo) = args.repo.as_deref().filter(|r| !r.trim().is_empty()) {
        view.push("--repo".to_string());
        view.push(repo.to_string());
    }
    let json = gh.run(&view)?.stdout_ok(&view)?;
    let facts: IssueFacts =
        serde_json::from_str(&json).context("`gh issue view --json` returned unreadable JSON")?;
    Ok(RefsIssue::from_facts(number, &facts))
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
