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

use super::body::{self, IssueLink};
use super::{EXIT_CHECK_FAILED, EXIT_OK, GhRunner, argv};
use crate::cli::PrOpenArgs;

/// The label every trusty-mpm-owned PR carries (`tm-workflow.md`).
const FRAMEWORK_LABEL: &str = "trusty-mpm";

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

    fn changelog_gate(&self, base: &str) -> anyhow::Result<ChangelogVerdict> {
        let root = repo_root()?;
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
/// What: in order — the body contract and footer ([`body::validate`]), the
/// `Refs`/`Closes` rule ([`body::apply_issue_link`]), the workstream label,
/// and the changelog gate. Returns every failure found, not just the first,
/// so one run fixes them all.
/// Test: `open_reports_each_missing_field`, `open_rejects_bad_footer`,
/// `open_requires_a_session_name`, `open_reports_changelog_failure`,
/// `open_docs_only_skips_the_changelog_gate`.
pub(crate) fn plan(
    args: &PrOpenArgs,
    body_text: &str,
    session: Option<&str>,
    changelog: ChangelogVerdict,
) -> Result<OpenPlan, Vec<String>> {
    let mut failures: Vec<String> = Vec::new();

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

    // `session` is Some here — an unresolved name is a failure above.
    let label = format!("ws/{}", session.unwrap_or_default());
    let mut gh_argv = argv(&["pr", "create"]);
    if let Some(repo) = args.repo.as_deref().filter(|r| !r.trim().is_empty()) {
        gh_argv.push("--repo".to_string());
        gh_argv.push(repo.to_string());
    }
    gh_argv.push("--base".to_string());
    gh_argv.push(args.base.clone());
    gh_argv.push("--title".to_string());
    gh_argv.push(args.title.clone());
    gh_argv.push("--body".to_string());
    gh_argv.push(linked.clone());
    gh_argv.push("--assignee".to_string());
    gh_argv.push("@me".to_string());
    gh_argv.push("--label".to_string());
    gh_argv.push(FRAMEWORK_LABEL.to_string());
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

    let plan = match plan(args, &body_text, session.as_deref(), changelog) {
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
    println!("  labels: {FRAMEWORK_LABEL}, {}", plan.workstream_label);
    if let Some(rung) = args.rung {
        println!("  test-ladder rung claimed: {rung}");
    }
    println!("  body fields supplied: {}", plan.supplied.join(", "));
    Ok(EXIT_OK)
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
