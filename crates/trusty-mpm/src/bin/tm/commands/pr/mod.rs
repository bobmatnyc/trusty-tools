//! `tm pr …` — deterministic pull-request open and merge-queue gates (#6653).
//!
//! Why: two procedures `version-control` runs on every delivery are written
//! down only as prose in `tm-workflow.md`, and both are mechanical. Opening a
//! PR means checking the seven-field body contract, the exact attribution
//! footer, the shipped `--assignee @me --label trusty-mpm --label
//! ws/<session>` defaults, and whether the diff owes a changelog fragment —
//! four judgments an agent re-derives each time and can silently skip.
//! Merging means walking a fixed stop-condition table over three `gh` reads,
//! where skipping one under time pressure is the recorded failure mode. A
//! command that returns an exit code removes both.
//!
//! What: [`run`] dispatches the two verbs to [`open`] and [`queue_check`].
//! Every `gh` call in this module goes through [`GhRunner`], whose production
//! implementation is [`RealGhRunner`] over `trusty_common::gh::GhCommand` —
//! this workspace's single `gh` entry point (#5475), carrying the
//! project-scoped identity binding. A second `Command::new("gh")` here would
//! be a defect. The trait is also the test seam: every test in the sibling
//! `tests.rs` drives a scripted fake, so nothing in this module needs a
//! network or a live `gh`.
//!
//! Exit codes are the verb surface:
//!
//! | verb | 0 | 1 | 2 |
//! |---|---|---|---|
//! | `open` | PR created (or `--dry-run` printed the argv) | — | a pre-flight check failed; `gh` was never called |
//! | `queue-check` | every listed PR is mergeable | at least one is blocked | usage or `gh` error |
//!
//! Test: the sibling `tests.rs`; `cli_parses_pr_*` in `tests.rs`.

pub(crate) mod body;
pub(crate) mod open;
pub(crate) mod queue_check;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use anyhow::Context as _;

use crate::cli::PrCmd;

/// Exit code: the verb's contract holds.
pub(crate) const EXIT_OK: i32 = 0;
/// Exit code: `queue-check` found at least one blocked PR.
pub(crate) const EXIT_BLOCKED: i32 = 1;
/// Exit code: a pre-flight check failed, or the invocation was wrong.
pub(crate) const EXIT_CHECK_FAILED: i32 = 2;

/// One completed `gh` invocation, as this module needs it.
///
/// Why: `gh pr view --json` on a missing PR and `gh pr create` on a rejected
/// push both exit non-zero, and the two want different handling, so the
/// runner hands back the triple rather than deciding.
/// What: exit success, stdout, stderr.
/// Test: `FakeGh` in `tests.rs` constructs these directly.
#[derive(Debug, Clone)]
pub(crate) struct GhRun {
    /// Whether `gh` exited zero.
    pub(crate) success: bool,
    /// Verbatim stdout.
    pub(crate) stdout: String,
    /// Verbatim stderr.
    pub(crate) stderr: String,
}

impl GhRun {
    /// stdout when the run succeeded, else an error naming the argv + stderr.
    pub(crate) fn stdout_ok(self, argv: &[String]) -> anyhow::Result<String> {
        if self.success {
            return Ok(self.stdout);
        }
        anyhow::bail!("`gh {}` failed: {}", argv.join(" "), self.stderr.trim())
    }
}

/// The seam every `gh` call in `tm pr` goes through.
///
/// Why: the verdicts these verbs compute are pure functions of `gh` output,
/// so hiding the spawn behind a trait makes the whole decision table testable
/// against canned JSON — no network, no live `gh`, no PR to create.
/// What: one method taking a fully-formed argv (without the `gh` itself).
/// Test: `FakeGh` in `tests.rs` scripts responses by argv prefix.
pub(crate) trait GhRunner {
    /// Run `gh <args>` to completion.
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun>;
}

/// Production [`GhRunner`] over `trusty_common::gh::GhCommand`.
///
/// Why: `GhCommand` is this workspace's single `gh` entry point (#5475) — it
/// decides which binary, renders the argv for errors, classifies a missing
/// binary, and never goes through a shell. Spawning `gh` here directly would
/// be a second implementation of that capability, which the repo's "common
/// entry point" rule exists to prevent.
/// What: overlays the active project's GitHub identity (`tm`'s `gh_identity`,
/// #1265) onto the child, then runs blocking and maps the outcome to
/// [`GhRun`].
/// Test: exercised live (see the `tm pr queue-check` evidence on the PR); the
/// logic it feeds is covered against [`GhRunner`] fakes.
pub(crate) struct RealGhRunner {
    /// `GH_*` overrides resolved from the active project's config.
    env: Vec<(String, String)>,
}

impl RealGhRunner {
    /// Build a runner bound to the active project's GitHub identity.
    pub(crate) fn new() -> anyhow::Result<Self> {
        let gh_env = crate::gh_identity::load_gh_env()?;
        Ok(Self {
            env: gh_env.vars().to_vec(),
        })
    }
}

impl GhRunner for RealGhRunner {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        let mut cmd = trusty_common::gh::GhCommand::new(args);
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }
        let out = cmd
            .output_blocking()
            .with_context(|| format!("`gh {}` could not be run", args.join(" ")))?;
        Ok(GhRun {
            success: out.success,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// Resolve `owner/repo` for the verbs that need the REST API.
///
/// Why: branch-protection lives at `repos/<owner>/<repo>/branches/<base>/
/// protection`, which needs the pair spelled out — unlike `gh pr view`, which
/// infers it from the cwd's remote. Asking `gh` for it keeps the answer
/// consistent with whichever remote every other call in the run resolved.
/// What: returns `--repo` verbatim when given, else `gh repo view --json
/// nameWithOwner --jq .nameWithOwner`.
/// Test: `repo_slug_prefers_the_flag`, `repo_slug_falls_back_to_gh`.
pub(crate) fn repo_slug<R: GhRunner>(gh: &R, explicit: Option<&str>) -> anyhow::Result<String> {
    if let Some(slug) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(slug.to_string());
    }
    let argv = argv(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "--jq",
        ".nameWithOwner",
    ]);
    let slug = gh.run(&argv)?.stdout_ok(&argv)?.trim().to_string();
    anyhow::ensure!(
        !slug.is_empty(),
        "`gh repo view` returned no `nameWithOwner`; pass --repo <owner/repo>"
    );
    Ok(slug)
}

/// Build an owned argv from string slices.
pub(crate) fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// Drive one `tm pr` invocation to its exit code.
///
/// Why: both verbs answer with an exit code a caller branches on, so — like
/// `tm wait` (#5843) — exiting here rather than returning a `Result` to
/// `main` is what makes that guarantee hold for the error paths too.
/// What: builds the production `gh` runner, runs the requested verb, and
/// exits. An error from either verb prints to stderr and exits
/// [`EXIT_CHECK_FAILED`].
/// Test: the verb bodies are unit-tested against [`GhRunner`] fakes; this is
/// the exit-code wrapper.
pub(crate) fn run(cmd: PrCmd) -> ! {
    let code = match run_inner(cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tm pr: {e:#}");
            EXIT_CHECK_FAILED
        }
    };
    std::process::exit(code)
}

/// The fallible body of [`run`], split out so every error leaves one way.
fn run_inner(cmd: PrCmd) -> anyhow::Result<i32> {
    let gh = RealGhRunner::new()?;
    match cmd {
        PrCmd::Open(args) => open::run(&gh, &args, &open::RealPreflight),
        PrCmd::QueueCheck(args) => queue_check::run(&gh, &args),
    }
}
