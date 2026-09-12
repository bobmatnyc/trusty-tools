//! `tm issue …` — YAML-configurable issue state-management verbs (#1246).
//!
//! Why: externalizes the Unicorn Factory's hardcoded issue state machine (label
//! set, allowed transitions, assignee model) into a YAML contract owned by
//! trusty-mpm, surfaced as `tm issue` verbs the Python harness consumes by
//! shelling out. Every operation maps to a concrete label/assignee/comment
//! mutation, so issue state stays reconstructable from GitHub artifacts alone.
//! What: the [`issue`] dispatcher that selects the `gh` backend, resolves the
//! `agents.ticketing` standard (#6918), loads + validates the model (config
//! discovery: flag > CWD > `agents.ticketing.lifecycle_model` > user > embedded
//! default), and runs the requested verb (`seed-labels`, `transition`,
//! `current`, `states`, `standard`, `seed-config`, `repair`, `audit`). Schema
//! types live in `config.rs`, validation in `validate.rs`, the state machine in
//! `state.rs`, the operations in `ops.rs`, the `standard` printer in
//! `standard.rs`, the #7097 `audit` verb in `audit.rs`.
//! Test: pure logic is unit-tested in the submodules (`config`/`validate`/
//! `state`/`ops`); CLI parsing in `tests.rs`.

pub(crate) mod audit;
pub(crate) mod config;
pub(crate) mod ops;
pub(crate) mod seed_ticketing;
pub(crate) mod standard;
pub(crate) mod standard_live;
pub(crate) mod state;
pub(crate) mod validate;

#[cfg(test)]
#[path = "project_model_tests.rs"]
mod project_model_tests;

use std::path::PathBuf;

use crate::cli::IssueCmd;
use crate::commands::ticket::runner::{CommandRunner, RealCommandRunner};
use crate::commands::ticket::system::{
    GhTicketSystem, TicketSystem, TicketSystemKind, not_yet_supported,
};

use config::{ModelSource, StateModel, describe_source, load_model_with_source};
use seed_ticketing::{
    TicketingSeedOutcome, seed_outcome_result, seed_ticketing_block, ticketing_config_path,
};
use trusty_mpm::core::trusty_tools_config::{
    TICKETING_BLOCK_TEMPLATE, TrustyToolsConfig, resolve_ticketing,
};

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
    // #7067: `standard`'s live milestone/project read-back needs a runner of
    // its own — the backend owns one but does not expose it. Same binding, so
    // both halves of the verb speak to GitHub as the same identity.
    let runner = RealCommandRunner::with_gh_env(&gh_env);
    let backend = match system {
        TicketSystemKind::Gh => GhTicketSystem::new(RealCommandRunner::with_gh_env(&gh_env)),
        TicketSystemKind::Jira => return Err(not_yet_supported("jira")),
        TicketSystemKind::Linear => return Err(not_yet_supported("linear")),
    };
    dispatch(&backend, &runner, &gh_env, cmd)
}

/// Dispatch a parsed [`IssueCmd`] against a backend (generic for testability).
///
/// Why: separating dispatch from backend construction keeps the verb wiring
/// independent of `gh`, so the orchestration could be exercised with a fake.
/// `gh_env` rides along for the one verb (#7097's `audit`) whose `gh` calls go
/// through the workspace's shared `GhCommand` entry point rather than through
/// the `CommandRunner` seam the other verbs share.
/// What: matches each verb, loads the model where required, runs the op, and
/// prints a human summary.
/// Test: per-verb ops are unit-tested; this is thin glue.
fn dispatch<S: TicketSystem>(
    backend: &S,
    runner: &dyn CommandRunner,
    // #7097: `audit` reaches `gh` through `trusty_common::gh::GhCommand` rather
    // than the `CommandRunner` seam, so it needs the resolved identity itself.
    gh_env: &trusty_mpm::core::gh_identity::GhEnv,
    cmd: IssueCmd,
) -> anyhow::Result<()> {
    // #6918: resolve the operator's ticketing standard ONCE. An absent
    // `agents.ticketing` block yields the built-in defaults; a malformed one is
    // an error here rather than a silent revert to them.
    let ticketing = resolve_ticketing(&TrustyToolsConfig::load())?;
    let lifecycle = ticketing.lifecycle_model.as_deref();
    match cmd {
        IssueCmd::SeedLabels { config, dry_run } => {
            let (model, _source) = load_model_with_source(config.as_deref(), lifecycle)?;
            // #6914: the `ws/<session>` policy label needs the same session
            // name session launch labels with — the tmux session name.
            let session = crate::commands::tmux_attach::current_tmux_session_name();
            let report =
                ops::seed_labels(backend, &model, &ticketing, session.as_deref(), dry_run)?;
            print_seed_report(&report);
        }
        IssueCmd::Standard { config } => {
            let (model, _source) = load_model_with_source(config.as_deref(), lifecycle)?;
            standard::print_standard(&ticketing, &model, runner);
        }
        IssueCmd::Transition {
            issue,
            to_state,
            config,
            note,
        } => {
            // #7580: a rejected state name is the error this bug reads as, so
            // the model in force is named beside it.
            let (model, source) = load_model_with_source(config.as_deref(), lifecycle)?;
            let report = ops::transition(backend, &model, issue, &to_state, note.as_deref())
                .map_err(|e| with_source(e, &source))?;
            let from = report.from.as_deref().unwrap_or("(none)");
            println!("transitioned #{issue}: {from} → {}", report.to);
            if report.assignee_changed {
                println!("  assignee rule applied");
            }
        }
        IssueCmd::Current { issue, config } => {
            let (model, source) = load_model_with_source(config.as_deref(), lifecycle)?;
            let state =
                ops::current(backend, &model, issue).map_err(|e| with_source(e, &source))?;
            println!("{state}");
        }
        IssueCmd::States { config } => {
            // #7580: `tm issue states` is what an operator reads to learn the
            // valid names, so it says which model produced them.
            let (model, source) = load_model_with_source(config.as_deref(), lifecycle)?;
            print_states(&model, &source);
        }
        IssueCmd::SeedConfig { force } => {
            seed_config(force)?;
        }
        // #7097: reads only — no model needed, and no `gh` write.
        IssueCmd::Audit {
            issue,
            recent,
            since,
        } => {
            audit::run(&ticketing, gh_env, runner, issue, recent, since)?;
        }
        IssueCmd::Repair { issue, config } => {
            let (model, source) = load_model_with_source(config.as_deref(), lifecycle)?;
            let kept = ops::repair(backend, &model, issue).map_err(|e| with_source(e, &source))?;
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
    // #6914: an omitted ws/ label is stated, never silent.
    if report.workstream_skipped {
        println!("skipped ws/<session>: no tmux session name to derive it from");
    }
}

/// Append the model's source to a verb's failure (#7580).
///
/// Why: "unknown target state `status:merged`; valid states: [queued, …]" reads
/// as a mistyped state name. The one fact that turns it into the actual
/// diagnosis — which model produced that list — is the source, so it rides on
/// every verb failure rather than being something the operator has to go and ask
/// for.
/// What: reformats the error with [`describe_source`] on its own line. The
/// original message is kept verbatim and first.
/// Test: `verb_failure_names_the_embedded_default_source_7580`.
fn with_source(err: anyhow::Error, source: &ModelSource) -> anyhow::Error {
    anyhow::anyhow!("{err:#}\n  {}", describe_source(source))
}

/// Print the configured states and transitions.
///
/// Why: operator introspection of the active model (reads YAML only, no `gh`).
/// What: lists each state (its label, or `(no label)` for a label-less state,
/// plus the terminal flag) then each transition edge, marking the edges that
/// require a `--note`. Since #7580 the model's own source is printed first, so a
/// list of built-in states is never mistaken for the project's.
/// Test: side-effect-only (stdout); the model is unit-tested in `config`.
fn print_states(model: &StateModel, source: &ModelSource) {
    println!("{}", describe_source(source));
    println!("states ({}):", model.states.len());
    for s in &model.states {
        let term = if s.terminal { " [terminal]" } else { "" };
        let label = s.label.as_ref().map_or("(no label)", |l| l.name.as_str());
        println!("  {} → {}{}", s.name, label, term);
    }
    println!("transitions ({}):", model.transitions.len());
    for t in &model.transitions {
        let from = t.from.as_deref().unwrap_or("null");
        let note = if t.requires_note {
            " [--note required]"
        } else {
            ""
        };
        println!("  {from} → {} ({:?}){note}", t.to, t.trigger);
    }
}

/// `tm issue seed-config [--force]` — write both halves of the standard.
///
/// Why: lets operators start from a copy of the default model and edit it,
/// mirroring `tm services init` (RFC §6).
/// What: writes [`config::DEFAULT_MODEL_YAML`] to
/// `~/.trusty-tools/trusty-mpm/issue-state.yaml`, creating parent dirs; refuses
/// to overwrite an existing file unless `--force`. Then seeds the
/// [`TICKETING_BLOCK_TEMPLATE`] into `config.yaml` — the file
/// [`TrustyToolsConfig::load`] reads — via
/// [`seed_ticketing_block`] (#7067), so the milestone and project half of the
/// standard lands on disk rather than being printed for the operator to paste.
/// That half is never overwritten: an existing `agents.ticketing` is left as
/// the operator wrote it. One outcome exits NONZERO — an `agents:` key with no
/// `ticketing:` cannot be appended to, so the block is printed for a manual
/// paste and the command fails rather than reporting a seed it did not do.
/// Test: side-effect-only (filesystem/stdout); the write itself is covered by
/// the `seed_ticketing` tests, the exit split by
/// `only_the_refusal_outcome_exits_nonzero`, the template by
/// `the_seed_template_parses_to_the_builtin_defaults`.
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

    // #7067: the milestone/project keys live in a DIFFERENT file, so write
    // them there too rather than leaving the operator to paste a block.
    let config_yaml = ticketing_config_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory for the user config"))?;
    let shown = config_yaml.display();
    let outcome = seed_ticketing_block(&config_yaml)?;
    match outcome {
        TicketingSeedOutcome::Created => {
            println!("created {shown} with the agents.ticketing block");
        }
        TicketingSeedOutcome::Appended => {
            println!("appended the agents.ticketing block to {shown}");
        }
        TicketingSeedOutcome::AlreadyPresent => {
            println!("{shown} already declares agents.ticketing — left unchanged");
        }
        TicketingSeedOutcome::AgentsBlockPresent => {
            println!(
                "{shown} declares `agents:` without `ticketing:` — left unchanged, \
                 since a second `agents:` key would make the file unreadable. \
                 Add this entry under the `agents:` block by hand:\n"
            );
            print!("{TICKETING_BLOCK_TEMPLATE}");
        }
    }
    // #7067: the refusal arm exits nonzero — the standard is half-applied and a
    // script must not read that as a completed seed.
    seed_outcome_result(outcome, &config_yaml)
}

#[cfg(test)]
mod source_annotation_tests {
    use super::*;

    // #7580: a verb failure carries the model's source, so an unknown-state
    // error cannot be read as a typo when the built-in model is in force.
    #[test]
    fn verb_failure_names_the_embedded_default_source_7580() {
        let source = ModelSource::EmbeddedDefault {
            searched_from: PathBuf::from("/repo/crates/trusty-mpm"),
        };
        let err = with_source(
            anyhow::anyhow!("unknown target state `status:merged`; valid states: [queued]"),
            &source,
        );
        let text = format!("{err}");
        assert!(text.contains("unknown target state"), "{text}");
        assert!(text.contains("BUILT-IN default"), "{text}");
    }

    #[test]
    fn verb_failure_names_a_file_source() {
        let source = ModelSource::File(PathBuf::from("/repo/issue-state.yaml"));
        let text = format!("{}", with_source(anyhow::anyhow!("boom"), &source));
        assert!(
            text.contains("state model: /repo/issue-state.yaml"),
            "{text}"
        );
    }
}
