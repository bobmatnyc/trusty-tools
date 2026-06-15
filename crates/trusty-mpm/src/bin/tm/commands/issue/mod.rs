//! `tm issue …` — YAML-configurable issue state-management verbs (#1246).
//!
//! Why: externalizes the Unicorn Factory's hardcoded issue state machine (label
//! set, allowed transitions, assignee model) into a YAML contract owned by
//! trusty-mpm, surfaced as `tm issue` verbs the Python harness consumes by
//! shelling out. Every operation maps to a concrete label/assignee/comment
//! mutation, so issue state stays reconstructable from GitHub artifacts alone.
//! What: the [`issue`] dispatcher that selects the `gh` backend, loads + validates
//! the model (config discovery: flag > CWD > user > embedded default), and runs
//! the requested verb (`seed-labels`, `transition`, `current`, `states`,
//! `seed-config`, `repair`). Schema types live in `config.rs`, validation in
//! `validate.rs`, the state machine in `state.rs`, the operations in `ops.rs`.
//! Test: pure logic is unit-tested in the submodules (`config`/`validate`/
//! `state`/`ops`); CLI parsing in `tests.rs`.

pub(crate) mod config;
pub(crate) mod ops;
pub(crate) mod state;
pub(crate) mod validate;

use std::path::PathBuf;

use crate::cli::IssueCmd;
use crate::commands::ticket::runner::RealCommandRunner;
use crate::commands::ticket::system::{
    GhTicketSystem, TicketSystem, TicketSystemKind, not_yet_supported,
};

use config::{StateModel, load_model};

/// `tm issue <subcommand>` dispatcher.
///
/// Why: the single operator/harness entry point for the state-management verbs.
/// What: builds the `gh`-backed [`TicketSystem`] (rejecting non-`gh` backends
/// with the shared stub error), then dispatches each [`IssueCmd`] variant —
/// loading + validating the model first for the verbs that need it.
/// Test: parsing in `tests.rs`; per-verb logic in the submodule unit tests.
pub(crate) fn issue(cmd: IssueCmd, system: TicketSystemKind) -> anyhow::Result<()> {
    // #1265: bind the active project's GitHub identity to every `gh` call this
    // verb makes (empty binding → ambient gh identity, no regression).
    let gh_env = crate::gh_identity::load_gh_env()?;
    let backend = match system {
        TicketSystemKind::Gh => {
            GhTicketSystem::new(RealCommandRunner::with_env(gh_env.vars().to_vec()))
        }
        TicketSystemKind::Jira => return Err(not_yet_supported("jira")),
        TicketSystemKind::Linear => return Err(not_yet_supported("linear")),
    };
    dispatch(&backend, cmd)
}

/// Dispatch a parsed [`IssueCmd`] against a backend (generic for testability).
///
/// Why: separating dispatch from backend construction keeps the verb wiring
/// independent of `gh`, so the orchestration could be exercised with a fake.
/// What: matches each verb, loads the model where required, runs the op, and
/// prints a human summary.
/// Test: per-verb ops are unit-tested; this is thin glue.
fn dispatch<S: TicketSystem>(backend: &S, cmd: IssueCmd) -> anyhow::Result<()> {
    match cmd {
        IssueCmd::SeedLabels { config, dry_run } => {
            let model = load_model(config.as_deref())?;
            let report = ops::seed_labels(backend, &model, dry_run)?;
            print_seed_report(&report);
        }
        IssueCmd::Transition {
            issue,
            to_state,
            config,
            note,
        } => {
            let model = load_model(config.as_deref())?;
            let report = ops::transition(backend, &model, issue, &to_state, note.as_deref())?;
            let from = report.from.as_deref().unwrap_or("(none)");
            println!("transitioned #{issue}: {from} → {}", report.to);
            if report.assignee_changed {
                println!("  assignee rule applied");
            }
        }
        IssueCmd::Current { issue, config } => {
            let model = load_model(config.as_deref())?;
            let state = ops::current(backend, &model, issue)?;
            println!("{state}");
        }
        IssueCmd::States { config } => {
            let model = load_model(config.as_deref())?;
            print_states(&model);
        }
        IssueCmd::SeedConfig { force } => {
            seed_config(force)?;
        }
        IssueCmd::Repair { issue, config } => {
            let model = load_model(config.as_deref())?;
            let kept = ops::repair(backend, &model, issue)?;
            println!("repaired #{issue}: resolved to `{kept}`");
        }
    }
    Ok(())
}

/// Print a `seed-labels` summary.
///
/// Why: operators need to see what was created vs. already present.
/// What: prints the created and already-present label lists, flagging dry-runs.
/// Test: side-effect-only (stdout); the report itself is unit-tested in `ops`.
fn print_seed_report(report: &ops::SeedReport) {
    if report.dry_run {
        println!("[dry-run] would create {} label(s):", report.created.len());
    } else {
        println!("created {} label(s):", report.created.len());
    }
    for name in &report.created {
        println!("  + {name}");
    }
    println!("already present: {} label(s)", report.already_present.len());
}

/// Print the configured states and transitions.
///
/// Why: operator introspection of the active model (reads YAML only, no `gh`).
/// What: lists each state (label, terminal flag) then each transition edge.
/// Test: side-effect-only (stdout); the model is unit-tested in `config`.
fn print_states(model: &StateModel) {
    println!("states ({}):", model.states.len());
    for s in &model.states {
        let term = if s.terminal { " [terminal]" } else { "" };
        println!("  {} → {}{}", s.name, s.label.name, term);
    }
    println!("transitions ({}):", model.transitions.len());
    for t in &model.transitions {
        let from = t.from.as_deref().unwrap_or("null");
        println!("  {from} → {} ({:?})", t.to, t.trigger);
    }
}

/// `tm issue seed-config [--force]` — write the embedded default to user config.
///
/// Why: lets operators start from a copy of the default model and edit it,
/// mirroring `tm services init` (RFC §6).
/// What: writes [`config::DEFAULT_MODEL_YAML`] to
/// `~/.trusty-tools/trusty-mpm/issue-state.yaml`, creating parent dirs; refuses
/// to overwrite an existing file unless `--force`.
/// Test: side-effect-only (filesystem); covered manually + by the dispatch glue.
fn seed_config(force: bool) -> anyhow::Result<()> {
    let path: PathBuf = config::user_config_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory for the user config"))?;
    if path.exists() && !force {
        anyhow::bail!(
            "config already exists at {} — pass --force to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!("failed to create config dir {}: {e}", parent.display())
        })?;
    }
    std::fs::write(&path, config::DEFAULT_MODEL_YAML)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    println!("wrote default issue-state model to {}", path.display());
    Ok(())
}
