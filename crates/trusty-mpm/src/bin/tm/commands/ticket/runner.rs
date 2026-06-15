//! A mockable process-runner seam for `tm ticket`.
//!
//! Why: the ticket workflow shells out to `gh` (issue fetch/comment/PR) and
//! `git` (default-branch lookup). Hiding those calls behind a trait lets the
//! orchestration logic be unit-tested with a scripted fake instead of a live
//! GitHub repo + network, while the production path uses the real binaries.
//! What: the [`CommandRunner`] trait with one `run` method returning captured
//! stdout/stderr/exit-status, a [`CommandOutput`] value type, and the
//! [`RealCommandRunner`] that executes via `std::process::Command`.
//! Test: `RealCommandRunner` is exercised by the live integration path; the
//! trait is driven by `FakeRunner` in `system.rs`/`tests` modules.

use anyhow::Context as _;

/// Captured result of running an external command.
///
/// Why: callers need the exit status AND the captured streams (stdout for issue
/// JSON / PR URLs, stderr for actionable error messages) in one value so the
/// fake and real runners are interchangeable.
/// What: holds whether the command succeeded, its stdout, and its stderr as
/// UTF-8 lossy strings.
/// Test: constructed by both runners; asserted across the orchestration tests.
#[derive(Debug, Clone)]
pub(crate) struct CommandOutput {
    /// Whether the process exited 0.
    pub(crate) success: bool,
    /// Captured stdout (UTF-8 lossy).
    pub(crate) stdout: String,
    /// Captured stderr (UTF-8 lossy).
    pub(crate) stderr: String,
}

impl CommandOutput {
    /// Return the trimmed stdout, or an error carrying stderr when the command
    /// failed.
    ///
    /// Why: most call sites want "the output if it worked, else a useful error";
    /// folding the status check here keeps every caller from repeating it.
    /// What: returns `Ok(stdout.trim())` on success, else an `anyhow` error that
    /// names the program and includes the captured stderr.
    /// Test: `command_output_ok_returns_stdout`,
    /// `command_output_err_includes_stderr` in `system.rs`.
    pub(crate) fn ok_or_stderr(&self, program: &str) -> anyhow::Result<String> {
        if self.success {
            Ok(self.stdout.trim().to_string())
        } else {
            let detail = self.stderr.trim();
            anyhow::bail!(
                "`{program}` failed{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                }
            )
        }
    }
}

/// A seam for running external programs (`gh`, `git`).
///
/// Why: decouples the ticket orchestration from real process spawning so the
/// logic is testable without GitHub or a git checkout, mirroring the
/// runtime-adapter trait seam used elsewhere in trusty-mpm.
/// What: a single `run(program, args)` method returning a [`CommandOutput`].
/// Implementors capture stdout/stderr and never inherit the parent's streams.
/// Test: `RealCommandRunner` via the live path; `FakeRunner` in the unit tests.
pub(crate) trait CommandRunner {
    /// Run `program` with `args`, capturing stdout/stderr.
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput>;
}

/// Production [`CommandRunner`] that spawns real processes.
///
/// Why: the live `tm ticket` path must actually invoke `gh` and `git`.
/// What: runs the program via `std::process::Command::output`, capturing both
/// streams; an inability to spawn (e.g. binary not installed) is surfaced as an
/// actionable `anyhow` error rather than a panic.
/// Test: covered by the end-to-end manual run; not unit-tested (would require a
/// live binary).
pub(crate) struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .with_context(|| {
                format!("failed to run `{program}` — is it installed and on your PATH?")
            })?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
