//! `tctl ensure [--wait]` — idempotent per-project auto-config pass (DOC-8 §2, #1341).
//!
//! Why: the explicit manual entry point for the ensure-project pass. It fully
//! provisions a project for the trusty stack in three idempotent phases:
//!   1. patch the project's `.mcp.json` so Claude Code can reach the trusty MCP
//!      servers (the always-on, load-bearing core);
//!   2. register the project directory as a trusty-search index and create its
//!      trusty-memory palace (the #1341 project-setup stages); and
//!   3. when `--wait` is passed, block until the daemons report ready, returning
//!      exit 0 only when ready and the distinct exit 4 on timeout.
//!
//! Re-running must never duplicate or error — every phase is idempotent.
//!
//! What: [`run`] orchestrates the three phases, builds an [`report::EnsureReport`],
//! renders human or `--json`, and returns the report's exit code. The phases
//! themselves live in focused submodules:
//! [`mcp_patch`] (`.mcp.json`), [`project_setup`] (index/palace),
//! [`readiness`] (`--wait` polling), [`identity`] (pure name derivation),
//! [`daemon`] (address + HTTP seam), [`report`] (types + exit codes).
//!
//! Exit codes (#1341): `0` = fully provisioned (and, if `--wait`, ready);
//! `2` = a `.mcp.json` patch or a reachable-daemon project-setup stage failed;
//! `4` = `--wait` was requested but readiness was not reached before the timeout.
//!
//! Test: the submodules are unit-tested; `run` is side-effecting (filesystem +
//! network + a tokio runtime) and is exercised end-to-end by the CLI smoke path.

pub mod daemon;
pub mod identity;
pub mod mcp_patch;
pub mod project_setup;
pub mod readiness;
pub mod report;

/// Process-wide lock serialising tests that mutate global env vars.
///
/// Why: several ensure submodule tests set/remove `TRUSTY_DATA_DIR_OVERRIDE`,
/// `TCTL_ENSURE_WAIT_*`, etc. Those are process-global, so tests in different
/// modules would race each other. A single crate-shared lock — held for the
/// duration of each env-mutating test across every ensure submodule — serialises
/// them so the assertions are deterministic.
/// What: a `Mutex<()>` accessible to every ensure submodule's test code.
/// Test: used by the env-mutating tests in `daemon`, `project_setup`, `readiness`.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

use crate::commands::runtime::block_on;
use mcp_patch::MCP_FILE;
use report::EnsureReport;

/// Resolve the project root `ensure` operates on.
///
/// Why: every phase is scoped to the current project directory (where
/// `.mcp.json` lives and which becomes the index/palace identity). Resolving it
/// once keeps the phases consistent.
/// What: returns `std::env::current_dir()`, or `"."` as a last-resort fallback
/// if the cwd cannot be read (a degraded environment) so `ensure` still attempts
/// the relative `.mcp.json` patch rather than aborting.
/// Test: side-effecting (reads process cwd); the consuming logic is unit-tested.
fn project_root() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Render the report to stdout (human) or as `--json`.
///
/// Why: keeping the rendering in one helper keeps [`run`] focused on the phase
/// orchestration and exit-code return.
/// What: with `json`, writes the serialised report and returns whether it
/// succeeded; otherwise prints a human-readable per-member / per-stage summary
/// plus the readiness verdict, and always "succeeds".
/// Test: side-effecting (stdout); the report shaping is unit-tested in `report`.
fn render(report: &EnsureReport, json: bool) -> bool {
    if json {
        if crate::output::render_json(report).is_err() {
            eprintln!("tctl ensure: failed to write JSON output");
            return false;
        }
        return true;
    }
    println!("tctl ensure — {MCP_FILE} MCP servers");
    for m in &report.members {
        let mark = if m.ok { "ok" } else { "FAILED" };
        println!("  {:<18} {:<8} {}", m.member, mark, m.detail);
    }
    println!("tctl ensure — project setup");
    for s in &report.stages {
        let mark = if s.ok { "ok" } else { "FAILED" };
        println!("  {:<18} {:<8} {}", s.stage, mark, s.detail);
    }
    if report.wait {
        if report.ready {
            println!("tctl ensure — readiness: ready");
        } else {
            eprintln!(
                "tctl ensure: --wait timed out before the stack reported ready \
                 (exit 4); the patch + project-setup above completed."
            );
        }
    }
    true
}

/// Handle `tctl ensure [--wait]`.
///
/// Why: Phase-2 / #1341 entry point — fully and idempotently provision the
/// current project: `.mcp.json` patch, search-index register, memory-palace
/// create, and (optionally) block on readiness.
/// What: runs the three phases on a single tokio runtime (the project-setup
/// stages and the `--wait` poll are async), assembles the [`EnsureReport`],
/// renders it, and returns the exit code (`0`/`2`/`4` per the module note). The
/// `.mcp.json` patch is synchronous and runs first so it always happens even if
/// the daemons are down.
/// Test: side-effecting; the submodules carry the unit coverage.
pub fn run(wait: bool, json: bool) -> i32 {
    let root = project_root();

    // Phase 1 — synchronous, always-on `.mcp.json` patch.
    let members = mcp_patch::patch_all();

    // Phases 2 & 3 — async project-setup stages and optional readiness wait,
    // driven on one runtime.
    let (stages, ready) = block_on(async {
        let stages = project_setup::run_stages(&root).await;
        let ready = if wait {
            let cfg = readiness::PollConfig::from_env();
            match daemon::build_client() {
                Ok(client) => {
                    readiness::poll_until_ready(cfg, || readiness::probe_ready(&client)).await
                }
                Err(_) => false,
            }
        } else {
            // No `--wait`: readiness is not evaluated; `ready` is irrelevant to
            // the exit code (see `EnsureReport::exit_code`).
            false
        };
        (stages, ready)
    });

    let report = EnsureReport::build(members, stages, wait, ready);

    if !render(&report, json) {
        return 1;
    }
    report.exit_code()
}
