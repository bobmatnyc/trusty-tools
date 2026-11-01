//! `tctl install` / `tctl upgrade`: fetch tm's instructional content once
//! trusty-mpm has landed (#9396).
//!
//! Why: since tm 1.7.10 (#9136) no instructional content is compiled into
//! `tm`, and nothing in tctl wrote `~/.trusty-mpm/content/content-lock.toml`,
//! so an upgrade to tm 1.7.12 left every new session failing to compose its
//! PM instructions until the operator ran `tm content update` by hand.
//! What: after the member loop, when trusty-mpm was placed, runs the
//! just-placed `tm content update` (by its resolved path, never a PATH
//! lookup) through a [`CommandRunner`]. The outcome is reported and folded
//! into the exit code; a failure names [`REMEDY`] and never rolls back or
//! aborts anything — the binaries stay placed and every other step has
//! already run. [`dry_run_command`] is what `--dry-run` reports instead.
//! #10588: [`run_content_steps`] then runs each placed [`CONTENT_CHECKS`]
//! member's check verb, so content older than a placed binary fails the run
//! naming the missing file.
//! Test: `content_step_tests.rs`, and the report folding in `install_tests.rs`
//! and `upgrade_tests.rs`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::progress_ui::narrator;

/// The member whose placement triggers the content step.
pub const MPM_CRATE: &str = "trusty-mpm";

/// The command an operator runs when the content step fails.
pub const REMEDY: &str = "tm content update";

/// The arguments the step passes to `tm`.
pub const CONTENT_UPDATE_ARGS: [&str; 2] = ["content", "update"];

/// What one subprocess run produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    /// Whether it exited 0.
    pub success: bool,
    /// Its exit code, when it exited (not killed by a signal).
    pub code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Runs one program to completion: the seam tests replace.
pub trait CommandRunner {
    /// Runs `program` with `args`; `Err` when it could not be started.
    fn run(&self, program: &Path, args: &[&str]) -> std::io::Result<RunOutput>;
}

/// The production [`CommandRunner`]: a real subprocess, stdin closed.
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &Path, args: &[&str]) -> std::io::Result<RunOutput> {
        let out = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()?;
        Ok(RunOutput {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// The content step's result, in the `--json` report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContentStepOutcome {
    /// Whether `tm content update` succeeded.
    pub ok: bool,
    /// The command that ran.
    pub command: String,
    /// What it printed on success, or why it failed and the remedy.
    pub detail: String,
}

/// The `tm` to run for trusty-mpm placed at `placed`: the `tm` beside it
/// when present, else `placed` itself (the `trusty-mpm` alias execs `tm`).
pub fn tm_beside(placed: &Path) -> PathBuf {
    let tm = placed.with_file_name("tm");
    if tm.is_file() {
        tm
    } else {
        placed.to_path_buf()
    }
}

/// The command `--dry-run` reports for `selected` crates, or `None` when
/// trusty-mpm is not among them.
///
/// Test: `dry_run_reports_the_content_step_without_running_it`.
pub fn dry_run_command<'a>(
    mut selected: impl Iterator<Item = &'a str>,
    install_dir: &Path,
) -> Option<String> {
    selected.any(|name| name == MPM_CRATE).then(|| {
        let args = CONTENT_UPDATE_ARGS.join(" ");
        format!("{} {args}", install_dir.join("tm").display())
    })
}

/// Runs `tm content update` when trusty-mpm is among `placed`.
///
/// Why: see the module doc; the subprocess is the just-placed binary so a
/// stale `tm` earlier on PATH cannot answer for it.
/// What: `None` when trusty-mpm was not placed. Otherwise runs it through
/// `runner`, narrates the result (an error line naming [`REMEDY`] on
/// failure), and returns the outcome. Never panics; a runner that cannot
/// start the program is a failed outcome like any other.
/// Test: `install_runs_content_update_after_tm_lands`,
/// `upgrade_of_trusty_mpm_runs_content_update`,
/// `a_failed_content_update_keeps_the_install_and_exits_non_zero`.
pub fn run_if_mpm_placed(
    placed: &[(String, PathBuf)],
    runner: &dyn CommandRunner,
    json: bool,
) -> Option<ContentStepOutcome> {
    let (_, placed_mpm) = placed.iter().find(|(name, _)| name == MPM_CRATE)?;
    let tm = tm_beside(placed_mpm);
    let command = format!("{} {}", tm.display(), CONTENT_UPDATE_ARGS.join(" "));
    let narr = narrator(json);
    let _ = narr.info(&format!("fetching tm's instructional content: {command}"));
    let failed = |why: String| ContentStepOutcome {
        ok: false,
        command: command.clone(),
        detail: format!(
            "{why}; the binaries are installed, but tm cannot compose a session \
             until its content is fetched — run `{REMEDY}`"
        ),
    };
    let outcome = match runner.run(&tm, &CONTENT_UPDATE_ARGS) {
        Ok(out) if out.success => ContentStepOutcome {
            ok: true,
            command: command.clone(),
            detail: last_line(&out.stdout)
                .unwrap_or("content pinned")
                .to_owned(),
        },
        Ok(out) => {
            let code = out
                .code
                .map_or_else(|| "a signal".to_owned(), |c| format!("code {c}"));
            let why = last_line(&out.stderr)
                .or_else(|| last_line(&out.stdout))
                .unwrap_or("no output");
            failed(format!("`{command}` exited with {code}: {why}"))
        }
        Err(e) => failed(format!("could not run `{command}`: {e}")),
    };
    if outcome.ok {
        let _ = narr.info(&format!("content: {}", outcome.detail));
    } else {
        let _ = narr.error(&format!("content: {}", outcome.detail));
    }
    Some(outcome)
}

/// Members that read runtime content, and the verb that checks the installed
/// content against the placed binary (#10588).
pub const CONTENT_CHECKS: [(&str, [&str; 2]); 1] = [("trusty-review", ["content", "check"])];

/// The content step of an install or upgrade: `tm content update`, then a
/// check that the content serves every placed binary that reads it.
///
/// Why: #10588 — the newest content release (2.3.0) predated a prompt
/// trusty-review 0.39.0 reads, so the update pinned it, the install reported
/// success, and the first review exited 1. tctl cannot deploy content no
/// release has, so it fails naming the missing file instead.
/// What: runs [`run_if_mpm_placed`]; a failed update is the outcome and no
/// check runs. Then each placed [`CONTENT_CHECKS`] member runs its check verb
/// (the placed binary, by path); the first failure is the outcome, its detail
/// carrying the member's last stderr line, which names the missing file.
/// Otherwise the update's outcome, else the last check's; `None` when
/// neither ran.
/// Test: `install_fails_when_content_is_older_than_a_placed_binary`,
/// `install_passes_when_content_serves_every_placed_binary`,
/// `a_failed_content_update_skips_the_content_check`,
/// `a_placed_trusty_review_alone_checks_the_installed_content`,
/// `upgrade_fails_when_content_is_older_than_a_placed_binary`.
pub fn run_content_steps(
    placed: &[(String, PathBuf)],
    runner: &dyn CommandRunner,
    json: bool,
) -> Option<ContentStepOutcome> {
    let update = run_if_mpm_placed(placed, runner, json);
    if update.as_ref().is_some_and(|u| !u.ok) {
        return update;
    }
    let mut checked = None;
    for (name, args) in &CONTENT_CHECKS {
        let Some((_, binary)) = placed.iter().find(|(member, _)| member == name) else {
            continue;
        };
        let outcome = run_check(name, binary, args, runner, json);
        if !outcome.ok {
            return Some(outcome);
        }
        checked = Some(outcome);
    }
    update.or(checked)
}

/// Runs one member's content check verb and narrates the result (#10588).
fn run_check(
    name: &str,
    binary: &Path,
    args: &[&str],
    runner: &dyn CommandRunner,
    json: bool,
) -> ContentStepOutcome {
    let command = format!("{} {}", binary.display(), args.join(" "));
    let narr = narrator(json);
    let _ = narr.info(&format!(
        "checking {name}'s instructional content: {command}"
    ));
    let failed = |why: String| ContentStepOutcome {
        ok: false,
        command: command.clone(),
        detail: format!(
            "{why}; the binaries are installed, but {name} cannot run until content that \
             serves it is installed"
        ),
    };
    let outcome = match runner.run(binary, args) {
        Ok(out) if out.success => ContentStepOutcome {
            ok: true,
            command: command.clone(),
            detail: last_line(&out.stdout)
                .unwrap_or("content serves it")
                .to_owned(),
        },
        // A release older than the verb: clap's usage error, exit 2. It
        // predates #10588, so there is no manifest to check it against.
        Ok(out) if out.code == Some(2) && out.stderr.contains(UNKNOWN_VERB) => ContentStepOutcome {
            ok: true,
            command: command.clone(),
            detail: format!("not checked: this {name} predates `{}`", args.join(" ")),
        },
        Ok(out) => {
            let why = last_line(&out.stderr)
                .or_else(|| last_line(&out.stdout))
                .unwrap_or("no output");
            failed(format!("`{command}` failed: {why}"))
        }
        Err(e) => failed(format!("could not run `{command}`: {e}")),
    };
    if outcome.ok {
        let _ = narr.info(&format!("content: {}", outcome.detail));
    } else {
        let _ = narr.error(&format!("content: {}", outcome.detail));
    }
    outcome
}

/// What clap prints for a subcommand the binary does not have.
const UNKNOWN_VERB: &str = "unrecognized subcommand";

/// The last non-blank line of `text`, trimmed.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

#[cfg(test)]
#[path = "content_step_tests.rs"]
pub(crate) mod tests;
