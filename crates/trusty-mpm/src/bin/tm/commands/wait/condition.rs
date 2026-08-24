//! The three conditions `tm wait` can poll (#5843).
//!
//! Why: the driver in [`super`] owns the budget, the slice ceiling, and the
//! exit contract; it must not also know how a PID, a sentinel file, or a
//! GitHub check rollup is inspected. Each condition is one `poll()` behind the
//! [`Condition`] trait, so the driver loop is written once and every verb is
//! unit-testable without a real process, file race, or network call.
//! What: [`Condition`] with [`Poll`] (`Met`/`Pending` + a human detail);
//! [`RunCondition`] (process liveness), [`FileCondition`] (existence, or a
//! literal substring), and [`CheckCondition`] (GitHub PR checks) over the
//! [`ChecksProbe`] seam — [`GhChecksProbe`] is the production impl, routed
//! through `trusty_common::gh::GhCommand`, this workspace's single `gh` entry
//! point.
//! Test: `run_condition_*`, `file_condition_*`, `check_condition_*` in the
//! sibling `tests.rs`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

/// One poll's verdict.
///
/// Why: the driver only needs "done or not", plus a human-readable reason it
/// can put on the status line; folding the reason in keeps the driver from
/// re-deriving it per verb.
/// What: `Met` when the condition holds, `Pending` when it does not; both carry
/// the detail string.
/// Test: asserted by every `*_condition_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Poll {
    /// The condition holds — terminal success.
    Met(String),
    /// The condition does not hold yet.
    Pending(String),
}

/// A pollable condition.
///
/// Why: the driver loop is identical for all three verbs; this is the only
/// thing that varies. A trait (rather than an enum with a match in the loop)
/// also lets the tests drive the loop with a scripted fake condition.
/// What: one `poll` returning [`Poll`], or an error for a genuine probe
/// failure (unreadable file, `gh` not installed) — never for "not yet".
/// Test: `FakeCondition` in `tests.rs` drives the loop; each real impl has its
/// own tests.
pub(crate) trait Condition {
    /// Inspect the world once.
    fn poll(&self) -> anyhow::Result<Poll>;
}

/// Wait for a process to exit.
///
/// Why: the common case is "the cold build I started in the background" —
/// the agent has a PID (or a launcher wrote one to a handle file) and needs to
/// know when it is gone.
/// What: resolves the PID once per poll (a handle file is re-read every poll,
/// so a handle written after the wait started still works), then asks
/// `trusty_mpm::core::process::is_process_alive` — the workspace's single
/// `kill(pid, 0)` liveness entry point. Met when the process is gone.
/// Note the limit this inherits: a non-child process has no reapable exit
/// status, so this reports DISAPPEARANCE, never an exit code.
/// Test: `run_condition_met_for_dead_pid`, `run_condition_pending_for_self`,
/// `run_condition_reads_handle_file`, `run_condition_rejects_unparsable_handle`.
pub(crate) struct RunCondition {
    /// Directly supplied PID, when the caller passed `--pid`.
    pid: Option<u32>,
    /// Handle file holding the PID, when the caller passed `--handle`.
    handle: Option<PathBuf>,
}

impl RunCondition {
    /// Build from the parsed `--pid` / `--handle` selector.
    pub(crate) fn new(pid: Option<u32>, handle: Option<PathBuf>) -> Self {
        Self { pid, handle }
    }

    /// Resolve the PID to watch for this poll.
    fn resolve(&self) -> anyhow::Result<u32> {
        if let Some(pid) = self.pid {
            return Ok(pid);
        }
        let path = self
            .handle
            .as_deref()
            .context("`tm wait --for run` needs --pid or --handle")?;
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read job handle {}", path.display()))?;
        parse_handle_pid(&raw)
            .with_context(|| format!("no PID found in job handle {}", path.display()))
    }
}

impl Condition for RunCondition {
    fn poll(&self) -> anyhow::Result<Poll> {
        let pid = self.resolve()?;
        if trusty_mpm::core::process::is_process_alive(pid) {
            Ok(Poll::Pending(format!("pid {pid} still running")))
        } else {
            Ok(Poll::Met(format!("pid {pid} is gone")))
        }
    }
}

/// Extract a PID from a job-handle file's contents.
///
/// Why: launchers write handles in two shapes — a bare number, or a
/// `key=value` block. Accepting both means the agent never has to reformat a
/// handle it did not write.
/// What: returns the first line that is all digits, else the value of the
/// first `pid = <digits>` line. Whitespace around the key, the `=`, and the
/// value is all ignored, because a handle written by a shell script and one
/// written by a config writer do not agree on it.
/// Test: `parse_handle_pid_accepts_bare_and_keyed`.
pub(crate) fn parse_handle_pid(raw: &str) -> Option<u32> {
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(pid) = line.parse::<u32>() {
            return Some(pid);
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("pid")
            && let Ok(pid) = value.trim().parse::<u32>()
        {
            return Some(pid);
        }
    }
    None
}

/// Wait for a sentinel file to exist, optionally carrying a substring.
///
/// Why: the guard-compliant shape for "did my backgrounded command finish" is
/// `<cmd> > /tmp/gate.txt 2>&1; echo EXIT=$? >> /tmp/gate.txt` plus a wait on
/// `EXIT=` appearing in that file. That needs existence AND content.
/// What: `Pending` while the path is absent; with no `--contains`, `Met` the
/// moment it exists; with `--contains`, `Met` once the file's text holds that
/// LITERAL substring (not a regex — a literal keeps `EXIT=0` and `DONE` from
/// needing escaping). Non-UTF-8 bytes are lossily decoded rather than erroring.
/// Test: `file_condition_pending_when_absent`, `file_condition_met_on_existence`,
/// `file_condition_waits_for_substring`.
pub(crate) struct FileCondition {
    /// The sentinel path.
    path: PathBuf,
    /// Literal substring the file must contain, when one was requested.
    contains: Option<String>,
}

impl FileCondition {
    /// Build from the parsed `--path` / `--contains` selector.
    pub(crate) fn new(path: PathBuf, contains: Option<String>) -> Self {
        Self { path, contains }
    }
}

impl Condition for FileCondition {
    fn poll(&self) -> anyhow::Result<Poll> {
        let shown = self.path.display();
        if !self.path.exists() {
            return Ok(Poll::Pending(format!("{shown} does not exist yet")));
        }
        let Some(needle) = self.contains.as_deref() else {
            return Ok(Poll::Met(format!("{shown} exists")));
        };
        let bytes =
            std::fs::read(&self.path).with_context(|| format!("cannot read sentinel {shown}"))?;
        let text = String::from_utf8_lossy(&bytes);
        if text.contains(needle) {
            Ok(Poll::Met(format!("{shown} contains {needle:?}")))
        } else {
            Ok(Poll::Pending(format!(
                "{shown} exists but does not contain {needle:?} yet"
            )))
        }
    }
}

/// A seam for reading one PR's check rollup.
///
/// Why: `tm wait --for check` must never stream (`gh pr checks --watch` costs
/// an entire CI run's output in context). It takes ONE-SHOT reads instead, and
/// hiding that read behind a trait lets the settling logic — including the
/// eventual-consistency guard — be tested against canned JSON.
/// What: one method returning the raw `gh pr view --json …` stdout.
/// Test: `FakeProbe` in `tests.rs`.
pub(crate) trait ChecksProbe {
    /// Return `gh pr view <pr> --json state,statusCheckRollup` stdout.
    fn pr_view_json(&self, pr: u64, repo: Option<&str>) -> anyhow::Result<String>;
}

/// Production [`ChecksProbe`] over `trusty_common::gh::GhCommand`.
///
/// Why: `GhCommand` is this workspace's single `gh` entry point (#5475) — it
/// carries the per-project identity binding and never goes through a shell.
/// Spawning `gh` here directly would be a second implementation of that
/// capability.
/// What: runs `gh pr view <pr> --json state,statusCheckRollup`, requiring a
/// zero exit and non-blank stdout.
/// Test: exercised live; the parsing it feeds is covered by `check_condition_*`.
pub(crate) struct GhChecksProbe;

impl ChecksProbe for GhChecksProbe {
    fn pr_view_json(&self, pr: u64, repo: Option<&str>) -> anyhow::Result<String> {
        let out = trusty_common::gh::GhCommand::new([
            "pr",
            "view",
            &pr.to_string(),
            "--json",
            "state,statusCheckRollup",
        ])
        .repo(repo)
        .nonempty_stdout_blocking()
        .with_context(|| format!("`gh pr view {pr}` failed"))?;
        Ok(out)
    }
}

/// Wait for a GitHub PR's checks to settle.
///
/// Why: an agent that pushes and then wants to know whether CI settled has one
/// safe shape — repeated one-shot reads. This is that shape, with the recorded
/// false-DONE trap closed.
/// What: parses `gh pr view --json state,statusCheckRollup` each poll. A check
/// is settled only on its OWN terminal fields — a `CheckRun` needs
/// `status == COMPLETED` AND a non-empty `conclusion`; a `StatusContext` needs
/// a terminal `state`. `bucket` is deliberately never consulted: GitHub
/// surfaces a bucketed-complete value before the check has actually settled.
/// An EMPTY rollup is likewise treated as unsettled unless the caller opted in
/// with `--allow-empty-checks`, because GitHub reports zero check runs for a
/// window after a push. A PR that is MERGED or CLOSED is `Met` — there is
/// nothing left to wait for.
/// Test: `check_condition_pending_until_all_settled`,
/// `check_condition_ignores_bucket`, `check_condition_empty_rollup_is_pending`,
/// `check_condition_merged_pr_is_met`.
pub(crate) struct CheckCondition<P: ChecksProbe> {
    /// The `gh` seam.
    probe: P,
    /// PR number to inspect.
    pr: u64,
    /// `owner/repo`, or `None` to let `gh` use the cwd's remote.
    repo: Option<String>,
    /// Whether an empty rollup counts as settled (off by default).
    allow_empty: bool,
}

impl<P: ChecksProbe> CheckCondition<P> {
    /// Build from the parsed `--pr` / `--repo` / `--allow-empty-checks` selector.
    pub(crate) fn new(probe: P, pr: u64, repo: Option<String>, allow_empty: bool) -> Self {
        Self {
            probe,
            pr,
            repo,
            allow_empty,
        }
    }
}

impl<P: ChecksProbe> Condition for CheckCondition<P> {
    fn poll(&self) -> anyhow::Result<Poll> {
        let json = self.probe.pr_view_json(self.pr, self.repo.as_deref())?;
        let view: PrView = serde_json::from_str(&json)
            .with_context(|| format!("cannot parse `gh pr view {}` JSON", self.pr))?;
        Ok(settle(&view, self.allow_empty))
    }
}

/// One PR's settle-relevant fields.
#[derive(Debug, Deserialize)]
pub(crate) struct PrView {
    /// `OPEN` / `MERGED` / `CLOSED`.
    #[serde(default)]
    state: Option<String>,
    /// The check rollup; `None` when GitHub has not reported one yet.
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<RollupEntry>>,
}

/// One entry in a PR's check rollup.
///
/// Why: GitHub returns two shapes in one array — `CheckRun` (Actions, carrying
/// `status`/`conclusion`) and `StatusContext` (legacy commit statuses, carrying
/// `state`). Both must be judged on their own terminal field.
/// What: the identifying name plus every field that can prove terminality.
/// `bucket` is NOT deserialised at all: not reading it is a stronger guarantee
/// than reading it and remembering not to trust it.
/// Test: `check_condition_ignores_bucket` feeds a bucketed-complete entry that
/// is not actually settled.
#[derive(Debug, Deserialize)]
struct RollupEntry {
    /// `CheckRun` or `StatusContext`.
    #[serde(default, rename = "__typename")]
    typename: Option<String>,
    /// `CheckRun` display name.
    #[serde(default)]
    name: Option<String>,
    /// `StatusContext` display name.
    #[serde(default)]
    context: Option<String>,
    /// `CheckRun` lifecycle: `QUEUED` / `IN_PROGRESS` / `COMPLETED`.
    #[serde(default)]
    status: Option<String>,
    /// `CheckRun` result, present only once it has completed.
    #[serde(default)]
    conclusion: Option<String>,
    /// `StatusContext` result: `PENDING` / `EXPECTED` / `SUCCESS` / `FAILURE` / `ERROR`.
    #[serde(default)]
    state: Option<String>,
}

impl RollupEntry {
    /// The name to show for this entry.
    fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.context.clone())
            .or_else(|| self.typename.clone())
            .unwrap_or_else(|| "<unnamed check>".to_string())
    }

    /// Whether this entry has genuinely finished.
    ///
    /// Why: fails CLOSED. An entry carrying neither a completed `status` nor a
    /// terminal `state` — an unrecognised shape, or a truncated response — is
    /// unsettled, so a wait keeps waiting rather than declaring a false DONE.
    /// What: `CheckRun` needs `COMPLETED` plus a non-empty `conclusion`;
    /// anything else needs a terminal `state`.
    /// Test: `check_condition_pending_until_all_settled`.
    fn settled(&self) -> bool {
        let completed = self
            .status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("COMPLETED"))
            && self
                .conclusion
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty());
        let terminal_state = self.state.as_deref().is_some_and(|s| {
            ["SUCCESS", "FAILURE", "ERROR"]
                .iter()
                .any(|t| s.eq_ignore_ascii_case(t))
        });
        completed || terminal_state
    }

    /// Whether this settled entry reports a failure.
    fn failed(&self) -> bool {
        let bad = |v: &str| ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"].contains(&v);
        self.conclusion
            .as_deref()
            .is_some_and(|c| bad(&c.to_ascii_uppercase()))
            || self
                .state
                .as_deref()
                .is_some_and(|s| bad(&s.to_ascii_uppercase()))
    }
}

/// Decide whether a parsed PR view counts as settled.
///
/// Why: separating the decision from the `gh` call is what makes the
/// eventual-consistency guard testable against canned JSON.
/// What: MERGED/CLOSED short-circuits to `Met`; an absent or empty rollup is
/// `Pending` unless `allow_empty`; otherwise every entry must be
/// [`RollupEntry::settled`].
/// Test: the `check_condition_*` family.
fn settle(view: &PrView, allow_empty: bool) -> Poll {
    let pr_state = view.state.as_deref().unwrap_or("UNKNOWN");
    if pr_state.eq_ignore_ascii_case("MERGED") || pr_state.eq_ignore_ascii_case("CLOSED") {
        return Poll::Met(format!(
            "PR is {} — nothing left to settle",
            pr_state.to_ascii_uppercase()
        ));
    }

    let entries = view.status_check_rollup.as_deref().unwrap_or(&[]);
    if entries.is_empty() {
        return if allow_empty {
            Poll::Met("no checks reported; --allow-empty-checks was passed".to_string())
        } else {
            Poll::Pending(
                "no checks reported yet — an empty rollup is not settled (pass \
                 --allow-empty-checks for a PR that runs none)"
                    .to_string(),
            )
        };
    }

    let unsettled: Vec<String> = entries
        .iter()
        .filter(|e| !e.settled())
        .map(RollupEntry::label)
        .collect();
    if unsettled.is_empty() {
        let failed = entries.iter().filter(|e| e.failed()).count();
        Poll::Met(format!(
            "{} check(s) settled, {failed} failing (state={pr_state})",
            entries.len()
        ))
    } else {
        Poll::Pending(format!(
            "{} of {} check(s) unsettled: {}",
            unsettled.len(),
            entries.len(),
            unsettled.join(", ")
        ))
    }
}

/// Build the condition named by the parsed args.
///
/// Why: the driver takes a `Box<dyn Condition>` so its loop is written once;
/// this is the single place selector validation lives, so a missing `--path`
/// is a usage error before the first poll rather than a panic during one.
/// What: maps the verb + selectors onto a boxed condition, erroring when the
/// selector for that verb is absent or ambiguous.
/// Test: `build_condition_requires_a_selector`,
/// `build_condition_rejects_pid_and_handle_together`.
pub(crate) fn build(args: &crate::cli::WaitArgs) -> anyhow::Result<Box<dyn Condition>> {
    use crate::cli::WaitFor;
    match args.condition {
        WaitFor::Run => {
            anyhow::ensure!(
                !(args.pid.is_some() && args.handle.is_some()),
                "`tm wait --for run` takes --pid OR --handle, not both"
            );
            anyhow::ensure!(
                args.pid.is_some() || args.handle.is_some(),
                "`tm wait --for run` needs --pid <n> or --handle <file>"
            );
            Ok(Box::new(RunCondition::new(args.pid, args.handle.clone())))
        }
        WaitFor::File => {
            let path = args
                .path
                .clone()
                .context("`tm wait --for file` needs --path <file>")?;
            Ok(Box::new(FileCondition::new(path, args.contains.clone())))
        }
        WaitFor::Check => {
            let pr = args
                .pr
                .context("`tm wait --for check` needs --pr <number>")?;
            Ok(Box::new(CheckCondition::new(
                GhChecksProbe,
                pr,
                args.repo.clone(),
                args.allow_empty_checks,
            )))
        }
    }
}

/// Render the condition as a stable key for the cross-invocation budget file.
///
/// Why: the hard timeout must span re-runs, so two invocations naming the SAME
/// condition have to land on the same budget record — and two invocations
/// naming DIFFERENT conditions must not.
/// What: a canonical `verb:selector` string; paths are absolutised where
/// possible so `./gate.txt` and `/abs/gate.txt` share one budget.
/// Test: `spec_is_stable_and_discriminating`.
pub(crate) fn spec(args: &crate::cli::WaitArgs) -> String {
    use crate::cli::WaitFor;
    let abs = |p: &Path| {
        std::path::absolute(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
            .to_string()
    };
    match args.condition {
        WaitFor::Run => match (&args.pid, &args.handle) {
            (Some(pid), _) => format!("run:pid={pid}"),
            (None, Some(h)) => format!("run:handle={}", abs(h)),
            (None, None) => "run:unspecified".to_string(),
        },
        WaitFor::File => {
            let path = args.path.as_deref().map(abs).unwrap_or_default();
            match &args.contains {
                Some(c) => format!("file:{path}:contains={c}"),
                None => format!("file:{path}"),
            }
        }
        WaitFor::Check => {
            let repo = args.repo.as_deref().unwrap_or("<cwd-remote>");
            format!("check:{repo}#{}", args.pr.unwrap_or_default())
        }
    }
}
