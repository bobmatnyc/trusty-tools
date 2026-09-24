//! `tm issue epic …` — file and maintain an epic tracker (#8447).
//!
//! Why: `TICKETING.md`'s `epics.*` standard and the bundled `tm-epic` skill
//! both describe an epic authored entirely by hand — eleven ordered `gh` calls
//! for creation, and a markdown table retyped by an LLM on every phase
//! transition. The retyping IS the drift the pattern's second rule forbids, so
//! this module makes the deterministic half code. The hand-run procedure it
//! replaces is `crates/trusty-mpm/src/assets/skills/tm-epic/references/manual-procedure.md`,
//! including the guard it grew after an `awk` fail-open wiped a tracker body:
//! never write an empty body. Here that guard lives once, in
//! [`backend::EpicBackend::set_body`]; the marker half is structural, since
//! [`render::replace_block`] copies both marker lines through rather than
//! re-serialising the body.
//! What: the [`EpicCmd`] dispatcher — `create` (`create.rs`), `sync`
//! (`sync.rs`), `defer` (`defer.rs`) and `close` (`close.rs`) — over the
//! [`backend::EpicBackend`] seam (D7), with the plan parser in `plan.rs` and
//! every title, marker and table rendering in `render.rs`. Two more entry points
//! are called from outside this verb family (#8448): `hook.rs` is the
//! `tm issue transition` side effect that regenerates a phase's tracker, and
//! `audit_rows.rs` is the set-level rows `tm issue audit <tracker>` appends.
//! Test: orchestration in `tests.rs` against a scripted fake backend; CLI
//! parsing in `bin/tm/tests.rs` (`cli_parses_issue_epic_*`).

pub(crate) mod audit_rows;
pub(crate) mod backend;
pub(crate) mod close;
pub(crate) mod create;
pub(crate) mod defer;
pub(crate) mod hook;
pub(crate) mod plan;
pub(crate) mod render;
pub(crate) mod sync;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::path::Path;

use crate::cli::EpicCmd;
use crate::commands::issue::config::load_model_with_source;
use crate::commands::ticket::runner::RealCommandRunner;

use backend::GhEpicBackend;
use create::{CreateOptions, CreateReport};

/// Run one `tm issue epic` verb.
///
/// Why: the seam between clap's parsed shape and the backend-agnostic
/// orchestration. It resolves the three inputs a plan document cannot supply —
/// the milestone, the component label(s) and the `ws/<session>` name — and
/// refuses when any is missing, because an issue filed without them is a
/// standard violation `tm issue audit` would report.
/// What: builds a [`GhEpicBackend`] over a runner bound to the resolved
/// per-project GitHub identity (#1265), then dispatches. The two verbs that
/// render the `phases` block (`create`, `sync`) load the state model the way
/// `tm issue transition` does, for its status-label prefix (#8448);
/// `lifecycle` is the configured `agents.ticketing.lifecycle_model`.
/// Test: `epic_create_requires_a_milestone`, `epic_create_requires_a_component`,
/// `epic_create_requires_a_session_name`.
pub(crate) fn run(
    cmd: EpicCmd,
    gh_env: &trusty_mpm::core::gh_identity::GhEnv,
    lifecycle: Option<&Path>,
) -> anyhow::Result<()> {
    let backend = GhEpicBackend::new(RealCommandRunner::with_gh_env(gh_env));
    match cmd {
        EpicCmd::Create {
            from,
            milestone,
            component,
            phase_type,
            project,
            session,
            tracker,
            dry_run,
        } => {
            let opts = CreateOptions {
                plan_path: from,
                milestone: require_milestone(milestone)?,
                components: require_components(component)?,
                phase_type,
                project,
                session: require_session(session)?,
                tracker,
                dry_run,
                status_prefix: status_prefix(lifecycle)?,
            };
            let report = create::create(&backend, &opts)?;
            print_create(&report);
        }
        EpicCmd::Sync { epic } => {
            let report = sync::sync(&backend, epic, &status_prefix(lifecycle)?)?;
            if report.unchanged {
                println!(
                    "#{}: phases block already matches its {} child issue(s) — nothing written",
                    report.tracker, report.rows
                );
            } else {
                println!(
                    "#{}: phases block regenerated from {} child issue(s)",
                    report.tracker, report.rows
                );
            }
        }
        // #8448: the deferred block is amended, one row per run.
        EpicCmd::Defer {
            epic,
            item,
            why,
            destination,
        } => {
            let opts = defer::DeferOptions {
                item,
                why,
                destination,
            };
            let report = defer::defer(&backend, epic, &opts)?;
            if report.unchanged {
                println!(
                    "#{}: deferred block already carries this row — nothing written",
                    report.tracker
                );
            } else {
                println!("#{}: deferred row appended: {}", report.tracker, report.row);
            }
        }
        // #8448: refuses while a child is open; posts the outcome→evidence
        // comment; closes.
        EpicCmd::Close { epic, evidence } => {
            let report = close::close(&backend, epic, &evidence)?;
            let posted = if report.comment_posted {
                "closing comment posted"
            } else {
                "closing comment was already on the record"
            };
            println!(
                "#{}: closed — {} phase issue(s) closed, {} outcome line(s), {posted}",
                report.tracker,
                report.phases.len(),
                report.outcomes
            );
        }
    }
    Ok(())
}

/// The status-label prefix of the state model in force (#8448).
///
/// Why: the State cell strips this prefix from a child's lifecycle label, and
/// it differs per project (`status:` here, `unicorn:` in the crate default) —
/// so it is read from the same model `tm issue transition` resolves, never
/// hardcoded. The epic verbs carry no `--config` flag, so the flag slot is
/// `None` and discovery runs from the working directory upward.
/// Test: `phases_table_uses_the_configured_status_prefix` covers the renderer;
/// the load itself is `load_model_in`'s.
fn status_prefix(lifecycle: Option<&Path>) -> anyhow::Result<String> {
    let (model, _source) = load_model_with_source(None, lifecycle)?;
    Ok(model.label_config.status_prefix)
}

/// The milestone every issue in the run carries, or the refusal.
///
/// Test: `epic_create_requires_a_milestone`.
fn require_milestone(milestone: Option<String>) -> anyhow::Result<String> {
    milestone.filter(|m| !m.trim().is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "`--milestone <TITLE>` is required — every issue this files needs one. Run \
             `tm issue standard` for the live list"
        )
    })
}

/// One or more component labels, or the refusal.
///
/// Test: `epic_create_requires_a_component`.
fn require_components(components: Vec<String>) -> anyhow::Result<Vec<String>> {
    // #8447: drop blanks FIRST. Checking only that some entry is non-blank let
    // `--component "" --component api` pass an empty label through to `gh`.
    let components: Vec<String> = components
        .into_iter()
        .filter(|c| !c.trim().is_empty())
        .collect();
    if components.is_empty() {
        anyhow::bail!(
            "`--component <LABEL>` is required at least once — every issue this files needs one. \
             Run `tm issue standard` for the labels this workspace's crates map to"
        );
    }
    Ok(components)
}

/// The workstream session name, from the flag or from tmux.
///
/// Why: AC7 requires `ws/<session>` on every issue the run files, and the
/// harness derives that name from the tmux session. Outside tmux there is no
/// name to derive, so the flag is the only way to satisfy the requirement —
/// and filing without it is a refusal rather than a silently unlabelled issue.
/// Test: `epic_create_requires_a_session_name`.
fn require_session(session: Option<String>) -> anyhow::Result<String> {
    session
        .filter(|s| !s.trim().is_empty())
        .or_else(crate::commands::tmux_attach::current_tmux_session_name)
        .ok_or_else(|| anyhow::anyhow!(
            "no workstream session name — this shell is not inside tmux, so `ws/<session>` cannot \
             be derived. Pass `--session <NAME>`"
        ))
}

/// Print a `create` summary.
///
/// Test: side-effect only; [`CreateReport`] itself is asserted in `tests.rs`.
fn print_create(report: &CreateReport) {
    let prefix = if report.dry_run { "[dry-run] " } else { "" };
    match report.tracker {
        Some(n) => println!("{prefix}ISSUE: #{n}"),
        None => println!("{prefix}ISSUE: (would be filed)"),
    }
    for (phase, issue) in &report.filed {
        if report.dry_run {
            println!("{prefix}PHASE_{phase} → (would be filed)");
        } else {
            println!("PHASE_{phase} → #{issue}");
        }
    }
    for title in &report.skipped {
        println!("{prefix}already filed, skipped: {title}");
    }
    for line in &report.waived {
        println!("{prefix}no project attached — waiver comment posted on {line}");
    }
    println!("{prefix}plan: {}", report.plan_url);
}
