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

/// The last non-blank line of `text`, trimmed.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

#[cfg(test)]
#[path = "content_step_tests.rs"]
pub(crate) mod tests;
