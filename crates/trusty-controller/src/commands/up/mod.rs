//! `tctl up` — the first-loaded control plane boot orchestrator (DOC-12).
//!
//! Why: Per the owner directive (Bob, 2026-06-15), `tctl` is the single entry
//! point through which the whole trusty stack comes up. `tctl up` is the
//! dependency-ordered, readiness-gated boot command that brings up the always-on
//! core (memory + search), runs the background boot analysis, starts the console
//! (the single HTTP front door), and — when opted in — the trusty-mpm
//! orchestrator with a present-and-latest Claude Code runtime.
//!
//! What: This module is the entry/wiring layer. `run` performs STAGE 0
//! (preflight: load config, resolve flags>config>defaults, take the system
//! advisory lock), resolves the §4 interactive prompt to a concrete decision,
//! constructs the production seams, delegates STAGE 1–5 to `stages::run_sequence`,
//! renders the report, and returns the §5 exit code. The heavy logic lives in
//! the sibling submodules; this file stays thin.
//!
//! Test: `tests` (sibling file) covers config/policy/member/claude/lock/report
//! and the end-to-end sequence with mock seams. `run` itself (which acquires a
//! real lock and spawns real processes) is covered by the §criteria CLI parse
//! tests in `cli_tests.rs` plus the sequence tests.

pub mod claude_code;
pub mod config;
pub mod lock;
pub mod manifest;
pub mod member;
pub mod os_env;
pub mod policy;
pub mod report;
pub mod stages;
pub mod system_runner;

use claude_code::ClaudeEnv;
use config::AnalyzeMode;
use lock::BootLock;
use os_env::{OsClaudeEnv, OsConsoleLocator};
use policy::{OrchestratorDecision, ResolvedBootPolicy, TriFlag, UpFlags};
use stages::{run_sequence, OrchestratorDeps};
use system_runner::SystemRunner;

/// Parsed `tctl up` arguments handed from the CLI layer.
///
/// Why: Decouples clap (cli.rs) from the orchestrator so the entry point takes a
/// plain record and the policy fold (`policy.rs`) is the only precedence site.
///
/// What: The §4/§3.5/§6 flags plus the global `--json` / `--yes`.
///
/// Test: Constructed by `commands::up_dispatch` and in `tests`.
#[derive(Clone, Debug)]
pub struct UpArgs {
    /// `--with-mpm` (§4).
    pub with_mpm: bool,
    /// `--no-mpm` (§4).
    pub no_mpm: bool,
    /// `--analyze-core <blocking|background|skip>` (§3.5); `None` = unset.
    pub analyze_core: Option<AnalyzeMode>,
    /// `--wait` (§3.5).
    pub wait: bool,
    /// `--skip-claude-upgrade` (§6).
    pub skip_claude_upgrade: bool,
    /// Global `--yes` (suppress prompts; non-interactive).
    pub yes: bool,
    /// Global `--json`.
    pub json: bool,
}

/// Run `tctl up` and return the process exit code (DOC-12 §3 / §5).
///
/// Why: The single boot entry point; returning the exit code (rather than
/// calling `process::exit`) keeps it testable and lets `main` own the exit.
///
/// What: STAGE 0 (config + flag resolution + advisory lock), resolve the §4
/// Prompt decision against a real TTY query, build the production seams
/// (`SystemRunner`, `OsConsoleLocator`, `OsClaudeEnv`), run the sequence, render
/// the report, return `report.exit_code()`. A failure to take the lock or load
/// config is surfaced and mapped to exit `1`.
///
/// Test: Sequence behaviour is covered by `tests::sequence_*`; this wrapper is
/// smoke-exercised via the CLI integration (it spawns real members, so it is not
/// asserted on in unit tests).
pub fn run(args: UpArgs) -> i32 {
    // STAGE 0 — preflight: load boot config (#1220), tolerating an absent file.
    let cfg = match config::load_boot_config() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load boot config");
            eprintln!("tctl up: failed to load boot config: {e}");
            return 1;
        }
    };

    let claude_env = OsClaudeEnv;
    let flags = UpFlags {
        with_mpm: TriFlag::from_pair(args.with_mpm, args.no_mpm),
        analyze_core: args.analyze_core,
        skip_claude_upgrade: args.skip_claude_upgrade,
        wait: args.wait,
        is_tty: claude_env.is_tty(),
        yes: args.yes,
    };

    let manifest = manifest::default_manifest();
    let mut policy = ResolvedBootPolicy::resolve(&flags, &cfg, manifest::analyze_core_targets());

    // Resolve the §4 interactive Prompt to a concrete On/Off before sequencing.
    if policy.orchestrator == OrchestratorDecision::Prompt {
        policy.orchestrator = if prompt_with_mpm() {
            OrchestratorDecision::On
        } else {
            OrchestratorDecision::Off
        };
    }

    // STAGE 0 — take the system advisory lock for the mutating stages (§3.4).
    let _lock = match BootLock::acquire() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "failed to acquire boot lock");
            eprintln!("tctl up: could not acquire the system boot lock: {e}");
            return 1;
        }
    };

    // Build production seams and drive STAGE 1–5.
    let runner = SystemRunner::new();
    let console = OsConsoleLocator;
    let orch = OrchestratorDeps {
        claude: &claude_env,
    };
    let report = run_sequence(&manifest, &policy, &runner, &console, &orch);

    report.render(args.json);
    report.exit_code()
}

/// Interactive §4 prompt: "Start the session manager (trusty-mpm)? [y/N]".
///
/// Why: When neither flag nor config sets the opt-in and we are on a TTY, §4
/// requires an interactive prompt defaulting to No.
///
/// What: Prints the prompt to stderr (stdout is reserved for data), reads a line
/// from stdin, returns `true` only on an explicit yes; default (empty / error)
/// is No.
///
/// Test: Side-effect-only (stdin); the decision branch that calls it is unit
/// tested via the `Prompt` path in `policy::tests`.
fn prompt_with_mpm() -> bool {
    use std::io::Write;
    eprint!("Start the session manager (trusty-mpm) now? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let ans = line.trim().to_ascii_lowercase();
    ans == "y" || ans == "yes"
}

// ── Unit tests ───────────────────────────────────────────────────────────────
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
