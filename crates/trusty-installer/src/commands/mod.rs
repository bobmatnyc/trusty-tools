//! Per-command handlers for `trusty-installer`.
//!
//! Why: Keeping each command in its own submodule prevents `mod.rs` from
//! becoming a monolith (500-line cap; CLAUDE.md) and lets each handler be
//! tested independently.
//!
//! What: Re-exports the public `run_*` functions so `main.rs` can dispatch
//! through a single `crate::commands::*` import. Phase-0 stubs return
//! `NotYetImplemented`; fully-implemented commands (currently only `version`)
//! perform real work.
//!
//! Test: Each module has its own test section; `cargo test -p trusty-installer`
//! runs them all.

pub mod auto_update;
pub mod config;
pub mod dependency_graph;
pub mod doctor;
pub mod ensure;
pub mod install;
pub mod install_gate;
pub mod lifecycle;
pub mod macos_signing;
pub mod passthrough;
pub mod picker;
pub mod plist_bootstrap;
pub mod plist_label;
pub mod port;
pub mod prereqs;
pub mod probe;
pub mod progress_ui;
pub mod runtime;
pub mod self_update;
pub mod service_bootstrap;
pub mod sign;
pub mod stable_set;
pub mod stack;
pub mod status;
pub mod ui;
pub mod up;
pub mod update_engine;
pub mod updates;
pub mod upgrade;
pub mod verify_tail;
pub mod version;

use crate::cli::AnalyzeCoreArg;
use up::config::AnalyzeMode;
use up::UpArgs;

/// Bridge clap's `tctl up` flags into `up::UpArgs` and run the orchestrator.
///
/// Why: Keeps `main.rs` a pure dispatcher and isolates the one place clap's
/// `AnalyzeCoreArg` value enum is translated into the orchestrator's
/// `AnalyzeMode`, so neither layer depends on the other's enum.
///
/// What: Maps the parsed flags into `UpArgs` (folding the global `--json` /
/// `--yes`), calls `up::run`, and returns its process exit code (DOC-12 §5).
///
/// Test: `up::tests` covers the orchestrator; this thin bridge is exercised via
/// the CLI parse tests in `cli_tests.rs`.
pub fn run_up(
    with_mpm: bool,
    no_mpm: bool,
    analyze_core: Option<AnalyzeCoreArg>,
    wait: bool,
    skip_claude_upgrade: bool,
    yes: bool,
    json: bool,
) -> i32 {
    let analyze_core = analyze_core.map(|a| match a {
        AnalyzeCoreArg::Blocking => AnalyzeMode::Blocking,
        AnalyzeCoreArg::Background => AnalyzeMode::Background,
        AnalyzeCoreArg::Skip => AnalyzeMode::Skip,
    });
    up::run(UpArgs {
        with_mpm,
        no_mpm,
        analyze_core,
        wait,
        skip_claude_upgrade,
        yes,
        json,
    })
}
