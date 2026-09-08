//! The `gh` reads behind `tm issue audit` (#7097).
//!
//! Why: the audit's verdicts are pure (see [`crate::core::issue_audit`]), so
//! the only thing left is fetching the facts — and that has exactly one correct
//! spelling in this workspace. Every call here goes through
//! [`trusty_common::gh::GhCommand`], the workspace's single `gh` entry point
//! (#5475), with the project's resolved [`GhEnv`] identity binding (#1265)
//! folded on. A bare `Command::new("gh")` here would be a second answer to
//! which binary, which identity, and how a missing binary is classified.
//!
//! What: [`view_issue`] reads one issue, [`list_open_issues`] reads a window of
//! OPEN issues. Both ask for [`AUDIT_JSON_FIELDS`], so a batch run evaluates
//! the same requirements a single-issue run does.
//!
//! # Pull requests
//!
//! `gh issue list` is issue-only — GitHub's issue search excludes pull
//! requests, verified live against this repository on 2026-09-08 with PR #7101
//! open: the list returned #7104, #7103, #7102, #7100, #7099 and skipped
//! #7101. Nothing here filters PRs out, because nothing has to.
//!
//! # Failing closed
//!
//! Every function returns `Err` when `gh` is missing, unauthenticated, or
//! errors. No caller may read that as "audited, nothing wrong" — the `tm doctor`
//! check folds it to UNDETERMINED and the CLI propagates it as a nonzero exit.
//!
//! Test: `issue_audit_gh_tests.rs` covers argv construction; the live path is
//! exercised by running the verb against this repository.

use std::path::Path;

use anyhow::Context as _;
use trusty_common::gh::GhCommand;

use crate::core::gh_identity::GhEnv;
use crate::core::issue_audit::{AUDIT_JSON_FIELDS, IssueFacts, created_on_or_after};

/// Which OPEN issues a batch audit covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditWindow {
    /// The N most recently created open issues.
    Recent(usize),
    /// Every open issue created on or after this `YYYY-MM-DD` date.
    Since(String),
}

/// Upper bound on issues one `--since` window may return.
///
/// Why: `gh issue list` applies its own silent default of 30 when `--limit` is
/// omitted, which would make a wide `--since` window quietly report on a
/// fraction of its issues — the same silent-truncation trap #7067 hit with
/// `gh project list`. An explicit, generous cap makes the bound visible.
/// Test: `since_window_argv_carries_an_explicit_limit`.
pub const SINCE_LIMIT: usize = 500;

/// Fold a project's GitHub identity binding onto a [`GhCommand`].
///
/// Why: `GhEnv::apply_to` targets a `std::process::Command`, and the removal
/// must precede the set (#6668). Reproducing that order here — once — is what
/// keeps this module from becoming a second, subtly different applier.
fn bind(mut cmd: GhCommand, gh_env: &GhEnv) -> GhCommand {
    for key in gh_env.unset_vars() {
        cmd = cmd.env_remove(key);
    }
    for (key, value) in gh_env.vars() {
        cmd = cmd.env(key, value);
    }
    cmd
}

/// The argv for `gh issue view <n> --json …`.
///
/// Why: exposed so a test can assert the field list reaches gh without
/// spawning one.
/// Test: `view_argv_requests_every_audited_field`.
#[must_use]
pub fn view_argv(number: u64) -> Vec<String> {
    vec![
        "issue".to_string(),
        "view".to_string(),
        number.to_string(),
        "--json".to_string(),
        AUDIT_JSON_FIELDS.to_string(),
    ]
}

/// The argv for the batch `gh issue list --state open …`.
///
/// Why: same reason as [`view_argv`] — the `--limit` and the `--search` window
/// are the two things a silent-truncation bug would live in, so they are
/// assertable without a live `gh`.
/// What: always `--state open` (a closed issue's hygiene is not actionable) and
/// always an explicit `--limit`. A [`AuditWindow::Since`] window adds
/// `--search created:>=<date>`.
/// Test: `recent_window_argv_bounds_the_limit`,
/// `since_window_argv_carries_an_explicit_limit`.
#[must_use]
pub fn list_argv(window: &AuditWindow) -> Vec<String> {
    let (limit, search) = match window {
        AuditWindow::Recent(n) => (*n, None),
        AuditWindow::Since(date) => (SINCE_LIMIT, Some(format!("created:>={date}"))),
    };
    let mut argv = vec![
        "issue".to_string(),
        "list".to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ];
    if let Some(search) = search {
        argv.push("--search".to_string());
        argv.push(search);
    }
    argv.push("--json".to_string());
    argv.push(AUDIT_JSON_FIELDS.to_string());
    argv
}

/// Run one `gh` invocation and return its trimmed stdout.
fn run(argv: &[String], repo_dir: Option<&Path>, gh_env: &GhEnv) -> anyhow::Result<String> {
    let mut cmd = GhCommand::new(argv);
    if let Some(dir) = repo_dir {
        cmd = cmd.cwd(dir);
    }
    let out = bind(cmd, gh_env).output_blocking()?.ok()?;
    Ok(out.stdout_trimmed().to_string())
}

/// Read one issue's audited facts.
///
/// Why: the single-issue half of `tm issue audit <N>`.
/// What: one `gh issue view` for [`AUDIT_JSON_FIELDS`], parsed into
/// [`IssueFacts`]. A missing or unauthenticated `gh`, or a nonexistent issue,
/// is an `Err` — never an empty-but-successful audit.
/// Test: argv in `view_argv_requests_every_audited_field`; the live read is
/// exercised by running the verb.
pub fn view_issue(
    number: u64,
    repo_dir: Option<&Path>,
    gh_env: &GhEnv,
) -> anyhow::Result<IssueFacts> {
    let text = run(&view_argv(number), repo_dir, gh_env)
        .with_context(|| format!("could not read issue #{number} from gh"))?;
    serde_json::from_str(&text)
        .with_context(|| format!("could not parse `gh issue view {number} --json …` output"))
}

/// Read a window of OPEN issues' audited facts, newest first.
///
/// Why: the `--recent`/`--since` half, and the `tm doctor` check's whole input.
/// What: one `gh issue list` per [`list_argv`], then — for a
/// [`AuditWindow::Since`] window — a second, client-side pass through
/// [`created_on_or_after`] so the audited set equals the requested window even
/// if gh's search index over-returns.
/// Test: argv in `recent_window_argv_bounds_the_limit` /
/// `since_window_argv_carries_an_explicit_limit`; the window filter in
/// `the_since_window_keeps_the_boundary_day`.
pub fn list_open_issues(
    window: &AuditWindow,
    repo_dir: Option<&Path>,
    gh_env: &GhEnv,
) -> anyhow::Result<Vec<IssueFacts>> {
    let text =
        run(&list_argv(window), repo_dir, gh_env).context("could not list open issues from gh")?;
    let mut facts: Vec<IssueFacts> =
        serde_json::from_str(&text).context("could not parse `gh issue list --json …` output")?;
    if let AuditWindow::Since(date) = window {
        facts.retain(|f| created_on_or_after(f, date));
    }
    Ok(facts)
}

#[cfg(test)]
#[path = "issue_audit_gh_tests.rs"]
mod tests;
