//! The standard GitHub Projects board: its spec, the `issue-state.yaml`
//! `project:` block, and `tm issue board init | apply | check | show | add`
//! (#10275 Phase 1, #10276 Phase 2, epic #10274).
//!
//! Why: every repo that adopts the trusty-mpm workflow gets a board in one
//! standard format, created from nothing through the API, and issues the
//! workflow files land on it by code. This is the ONE Projects module (D2);
//! `tm issue epic create`'s attach and the issue audit's board row use it, and
//! #9400 and #9401 extend it.
//! What: [`spec`] / [`validate`] / [`project`] (Phase 1: the embedded
//! `board-v1.yaml`, its load-time rules, the `project:` block); [`client`]
//! (every `gh` call) and [`snapshot`] (the reads); [`reconcile`] (plan and
//! execute, shared by `init` and `apply`); [`init`]; [`check`]; [`backfill`];
//! [`attach`] (D10 and `add`); [`readme`]; [`verbs`]; and [`run_cli`], the
//! CLI entry, which refuses a non-GitHub tracker before anything else (D9).
//! Test: `board/tests.rs` (spec), `board/verb_tests.rs` and
//! `board/attach_tests.rs` (verbs, through the fake `gh` in
//! `board/fake_gh_tests.rs`).

pub(crate) mod attach;
pub(crate) mod backfill;
pub(crate) mod check;
pub(crate) mod client;
pub(crate) mod error;
pub(crate) mod init;
pub(crate) mod project;
pub(crate) mod readme;
pub(crate) mod reconcile;
pub(crate) mod snapshot;
pub(crate) mod spec;
pub(crate) mod validate;
pub(crate) mod verbs;

#[cfg(test)]
mod attach_tests;
#[cfg(test)]
mod fake_gh_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod verb_tests;

use std::time::Duration;

use anyhow::Context as _;

use crate::cli::BoardCmd;
use crate::commands::issue::config::load_model_with_source;
use crate::commands::ticket::runner::RealCommandRunner;
use crate::commands::ticket::system::TicketSystemKind;
use trusty_mpm::core::trusty_tools_config::{TrustyToolsConfig, resolve_ticketing};

use attach::{AttachOutcome, add, resolve_target};
use backfill::{CursorFile, SleepPacer};
use client::BoardClient;
use error::BoardError;
use verbs::{ApplyFlags, apply_verb, check_verb, repo_slug, require_block, show_text};

/// D9: refuse any tracker but GitHub, naming it.
///
/// Test: `board_refuses_a_non_github_tracker_naming_it_10276`.
pub(crate) fn require_github(system: TicketSystemKind) -> Result<(), BoardError> {
    let tracker = match system {
        TicketSystemKind::Gh => return Ok(()),
        TicketSystemKind::Jira => "jira",
        TicketSystemKind::Linear => "linear",
    };
    Err(BoardError::NotGitHub {
        tracker: tracker.to_string(),
    })
}

/// `tm issue board <verb>`.
///
/// Why: the one process-facing entry: identity, config and the working
/// directory are resolved here and nowhere below.
/// What: refuses a non-GitHub tracker first (D9), then binds #1265's project
/// identity to the runner, loads the embedded spec, and runs the verb. `check`
/// exits non-zero on drift; `add` exits 0 on D10's waiver.
/// Test: `board_refuses_a_non_github_tracker_naming_it_10276`; the verbs are
/// tested through `verbs.rs`, `init.rs` and `attach.rs`.
pub(crate) fn run_cli(cmd: BoardCmd, system: TicketSystemKind) -> anyhow::Result<()> {
    require_github(system)?;
    let spec = spec::embedded_spec()?;
    if let BoardCmd::Show = cmd {
        print!("{}", show_text(&spec));
        return Ok(());
    }
    let gh_env = crate::gh_identity::load_gh_env()?;
    let runner = RealCommandRunner::with_gh_env(&gh_env);
    let client = BoardClient::new(&runner);
    let ticketing = resolve_ticketing(&TrustyToolsConfig::load())?;
    let lifecycle = ticketing.lifecycle_model.as_deref();
    let default_project = ticketing.default_project.as_deref();
    match cmd {
        BoardCmd::Show => {}
        BoardCmd::Init {
            owner,
            repo,
            title,
            drop_option,
            apply,
        } => {
            let title = title
                .unwrap_or_else(|| repo.rsplit('/').next().unwrap_or(repo.as_str()).to_string());
            let opts = init::InitOptions {
                owner,
                repo,
                title,
                drop_options: drop_option,
                apply,
            };
            print!("{}", init::init(&client, &spec, &opts)?.text);
        }
        BoardCmd::Apply {
            config,
            repo,
            apply,
            backfill,
            status_from_labels,
            pace_ms,
        } => {
            let (model, _) = load_model_with_source(config.as_deref(), lifecycle)?;
            let block = require_block(&model, default_project)?;
            let repo = repo_slug(&client, repo.as_deref())?;
            let cursor = CursorFile::for_board(&block.owner, block.number, &repo)
                .context("could not resolve the home directory for the backfill cursor")?;
            let pacer = SleepPacer(Duration::from_millis(pace_ms));
            let flags = ApplyFlags {
                apply,
                backfill,
                status_from_labels,
            };
            let text = apply_verb(
                &client,
                &spec,
                &model,
                block,
                &repo,
                flags,
                (&pacer, &cursor),
            )?;
            print!("{text}");
        }
        BoardCmd::Check { config, repo } => {
            let (model, _) = load_model_with_source(config.as_deref(), lifecycle)?;
            let block = require_block(&model, default_project)?;
            let repo = repo_slug(&client, repo.as_deref())?;
            let report = check_verb(&client, &spec, block, default_project, &repo)?;
            print!("{}", report.render());
            if !report.clean() {
                anyhow::bail!("tm issue board check: the board drifts from the spec");
            }
        }
        BoardCmd::Add {
            issue,
            config,
            repo,
        } => {
            let (model, _) = load_model_with_source(config.as_deref(), lifecycle)?;
            let repo = repo_slug(&client, repo.as_deref())?;
            let target = resolve_target(&client, model.project.as_ref(), default_project, &repo)?;
            match add(&client, &target, &repo, issue)? {
                AttachOutcome::Attached => println!(
                    "added #{issue} to {} #{} ({})",
                    target.owner, target.number, target.source
                ),
                AttachOutcome::Waived { reason, posted } => {
                    let note = if posted { "posted" } else { "already present" };
                    println!(
                        "#{issue} not added: the token lacks the project scope; `no-project:` \
                         comment {note} ({reason})"
                    );
                }
            }
        }
    }
    Ok(())
}
