//! `tm wait` — the sanctioned in-turn wait primitive (#5843).
//!
//! Why: an agent has no guard-compliant way to wait for a long-running
//! background operation. Foreground `sleep` is rejected, and the harness
//! auto-backgrounds a foreground call near ~120s, so an agent that needs to
//! wait an hour ends its turn and loses the work. `tm wait` polls a CONDITION
//! instead of sleeping, and returns before the harness ceiling with a
//! machine-readable "still pending — re-run me" line, so the wait spans as
//! many invocations as the condition needs while every single invocation stays
//! inside the ceiling.
//! What: [`WaitFor`] selects the verb (`run`/`file`/`check`); [`WaitArgs`]
//! carries the per-verb selector plus the budget knobs (`--timeout`,
//! `--interval`, `--slice`).
//! Test: `cli_parses_wait_*` in `tests_behavior_a.rs`; the semantics live in
//! `commands::wait`.

use std::path::PathBuf;

/// Which condition `tm wait` polls.
///
/// Why: three conditions cover every wait an agent actually needs — a
/// background process finishing, a sentinel file appearing, and a PR's checks
/// settling. Typing it as a clap `ValueEnum` rejects anything else at parse
/// time rather than at poll time.
/// What: `Run` (a PID or file-backed job handle), `File` (existence, or a
/// literal substring in the file), `Check` (GitHub PR checks settling).
/// Test: `cli_parses_wait_for_run`, `cli_parses_wait_for_file`,
/// `cli_parses_wait_for_check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum WaitFor {
    /// Wait for a process to exit (`--pid`, or `--handle <file holding a pid>`).
    Run,
    /// Wait for a file to exist (`--path`), optionally containing `--contains`.
    File,
    /// Wait for a GitHub PR's checks to settle (`--pr`, optional `--repo`).
    Check,
}

/// Flags for `tm wait`.
///
/// Why: a struct variant keeps the ten-flag surface out of `Command` (500-SLOC
/// production cap) and lets `commands::wait` take one value instead of ten
/// positional parameters.
/// What: the verb (`--for`), the per-verb selectors, and the three budget
/// knobs. `--timeout` is the HARD ceiling across every re-run; `--slice` is how
/// long a SINGLE invocation may block before returning "pending"; `--interval`
/// is the poll spacing, floored per verb so a wait cannot become a blind-poll
/// storm.
/// Test: `cli_parses_wait_*` in `tests_behavior_a.rs`.
#[derive(Debug, clap::Args)]
pub(crate) struct WaitArgs {
    /// Condition to poll: `run`, `file`, or `check`.
    #[arg(long = "for", value_enum)]
    pub(crate) condition: WaitFor,

    /// `--for run`: PID to wait for. Met when the process is gone.
    #[arg(long)]
    pub(crate) pid: Option<u32>,

    /// `--for run`: file-backed job handle — a file whose contents carry the
    /// PID (bare digits, or a `pid=<n>` line). Met when that process is gone.
    #[arg(long)]
    pub(crate) handle: Option<PathBuf>,

    /// `--for file`: the sentinel path. Met when it exists.
    #[arg(long)]
    pub(crate) path: Option<PathBuf>,

    /// `--for file`: additionally require this literal substring in the file.
    #[arg(long)]
    pub(crate) contains: Option<String>,

    /// `--for check`: PR number whose checks must settle.
    #[arg(long)]
    pub(crate) pr: Option<u64>,

    /// `--for check`: `owner/repo` (defaults to the cwd's git remote).
    #[arg(long)]
    pub(crate) repo: Option<String>,

    /// `--for check`: treat an EMPTY check rollup as settled.
    ///
    /// Off by default on purpose: GitHub reports zero check runs for a short
    /// window after a push, and reading that as "settled" is the recorded
    /// false-DONE trap. Pass this only for a PR you know runs no checks.
    #[arg(long)]
    pub(crate) allow_empty_checks: bool,

    /// Hard timeout in seconds, spanning every re-run (default 1800, max 86400).
    #[arg(long, default_value_t = 1800)]
    pub(crate) timeout: u64,

    /// Poll spacing in seconds. Defaults and floors are per verb: run/file
    /// default 5 (floor 1), check defaults 20 (floor 10).
    #[arg(long)]
    pub(crate) interval: Option<u64>,

    /// Seconds ONE invocation may block before returning `pending`
    /// (default 100, clamped to 5..=500 to stay under the harness ceiling).
    #[arg(long, default_value_t = 100)]
    pub(crate) slice: u64,

    /// Directory holding the cross-invocation budget file
    /// (default `$TMPDIR/tm-wait`).
    #[arg(long)]
    pub(crate) state_dir: Option<PathBuf>,

    /// Discard any recorded budget for this condition and start the clock over.
    #[arg(long)]
    pub(crate) reset: bool,
}
