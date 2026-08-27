//! Driving `tga audit` over the selected repositories.
//!
//! Why: #5540 installs the pinned triple and #5502 built the capability seam,
//! but nothing invoked the sweep — the client could install `tga` and never run
//! it. #5555 closes that. The whole reason this crate exists is to produce a
//! due-diligence deliverable on the recipient's machine, and the sweep is the
//! step that produces it.
//!
//! What: [`sweep`] reads the repository selection from `state/`, checks that the
//! pinned triple is installed *and verified*, and runs one `tga audit` child per
//! selected repository. Each child gets its own generated tga config, its own
//! output directory, and its own log file, so a failure is attributable to one
//! repository instead of to "the run".
//!
//! ## Why one child per repository, not one sweep over all of them
//!
//! `tga audit` takes its repository set from a config file and reports one
//! overall status. Handing it all the repositories at once would satisfy the
//! invocation but not closure conditions 2 and 3 of #5555: a single exit code
//! cannot say which repository failed, and "one repo of six failed" would be
//! indistinguishable from "everything failed". One child per repository makes
//! per-repo status the natural unit rather than something reconstructed from
//! logs.
//!
//! ## Fail-closed, on both axes
//!
//! - **Per repository.** A child that exits non-zero is recorded as
//!   [`RepoResult::Failed`] with its status. It never reads as a success, and
//!   the log is kept.
//! - **Overall.** [`RunReport::status`] distinguishes [`RunStatus::AllSucceeded`]
//!   from [`RunStatus::Partial`] and [`RunStatus::AllFailed`], and
//!   `crate::cli::exit_code` maps anything other than the first onto a non-zero
//!   process exit. #5655 is the shape being avoided: `tga collect` exiting 0
//!   despite a write failure. A caller of this module cannot report success
//!   without having looked at the status.
//!
//! The run-progress record is written after EVERY child has finished, not once
//! at the end (#5494), and a failure to write it fails the whole call — a
//! record that cannot be written must not leave the client claiming a run it
//! cannot describe. That checkpoint is also what makes the sweep re-entrant:
//! see [`checkpoint`] for the resume rules and for why the unit is a repository
//! rather than a tga stage.
//!
//! ## The credential
//!
//! `tga audit` spawns `trusty-review report`, which needs inference. The
//! engagement's OpenRouter key reaches the child through its ENVIRONMENT and
//! nowhere else: it is never written to the generated tga config, never logged,
//! and [`crate::config::SecretKey`] redacts in `Debug`/`Display` so it cannot
//! reach an error message either. The honest limit of that seam: a child
//! process's environment is readable by other processes running as the same
//! user on the same machine. Passing a secret to a subprocess at all accepts
//! that; the environment is the least-bad of the available channels (a config
//! file persists on disk, and a command-line argument is world-readable in
//! `ps`).
//!
//! A registered board's credential travels the same way (#5857). The generated
//! config gets a `${TRUSTY_AUDIT_JIRA_TOKEN}`-style reference and the value goes
//! in the environment beside it; [`boards`] owns that split and the reasons for
//! it.
//!
//! Test: `super::run_tests`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::grounding;
use crate::inference;
use crate::manifest::AuditManifest;
use crate::progress::{Operation, Progress, StageEvent, StageState, UnitOutcome};
use crate::registry;
use crate::relay::Scrubber;
use crate::workdir::{Area, WorkDir};

// #5823: the selection file crossed run.rs past the 500-SLOC production cap, and
// it is the one concern here with producers outside this crate. Re-exported, so
// `crate::run::SelectedRepo` and friends stay where every caller already names
// them.
mod selection;

// #5857: what a registered board contributes to a child — the generated config
// section, the variable carrying its secret, and the gap when it cannot be
// collected. Public because `crate::chain` states the same gaps in the return
// package and must reach them through this one resolution, not a second copy.
pub mod boards;

// #5980: what a registered repository automatically contributes — its GitHub
// issues. Public for the same reason `boards` is: the generated section and
// the environment variable carrying its secret both need to reach `chain` /
// `session` call sites that construct or inspect a sweep from outside this
// module.
pub mod github_issues;

// #5494: the checkpoint and its resume rules are one subject and they are not
// this file's — `run.rs` drives children, `checkpoint.rs` decides what a re-run
// may skip.
pub mod checkpoint;

// #5494: "is this output worth believing" gained a second caller — the resume
// path asks it about a directory no child in this run wrote. Two callers of one
// judgement is what makes it a module rather than a step inside `run_one`.
mod verify;

// #5857: the pinned-tool preflight, split out when this file crossed the
// 500-SLOC production cap. It runs to completion before any child starts and
// answers one question — are the installed tools the ones this engagement pins
// — so it separates cleanly.
mod pins;

// #5915: approving each clone with `trusty-search` before `tga` tries to index
// it. Its own module for the same reason `pins` is one — this file is at the
// 500-SLOC production cap — and because the flag it deliberately does not pass
// needs somewhere to be explained and asserted.
//
// #6081: `pub(crate)` because `crate::grounding::index` reaches the same
// approval before it indexes. trusty-search is default-deny on both paths, and
// two spawns of `index add` would be two sets of refusal rules free to drift.
pub(crate) mod approve;

// #5982: the values a sweep produces, split out when `RunReport::board_gaps`
// crossed the 500-SLOC production cap. Re-exported, so `crate::run::RunReport`
// and friends stay where every caller already names them.
mod report;

// #6080: the one child this file starts per repository — the argument vector,
// the environment it hands over, the timeout and the output pumps. Split out
// when the run index crossed the 500-SLOC production cap; `run.rs` decides
// which repositories are audited, `child.rs` runs one.
mod child;

// #6244: whether a `git fetch` can authenticate without a prompt — the
// engagement-global preflight, and the one credential this crate holds that the
// child's git transport would otherwise never see.
pub(crate) mod git_credentials;

use approve::approve_for_indexing;
use child::spawn_tga;
use pins::{PinnedBinaries, pinned_binaries};
use verify::verify_output;

// Re-exported at its historical path: `crate::rerender`, `crate::distribute`,
// `crate::session` and both `cli` submodules name it as `crate::run::…`.
pub use checkpoint::{PROGRESS_FILE, Recollect, RunProgress, progress_path, read_progress};
pub use child::ENV_INFERENCE_CREDENTIAL;
pub use report::{RepoResult, RepoRun, RunOptions, RunReport, RunStatus};
pub use selection::{
    GithubLeg, SELECTION_FILE, SelectedRepo, load_selection, save_selection, selection_path,
};

/// A filename-safe, collision-free stem for one repository's files.
///
/// Why: two things at once. The name comes from a selection file this client did
/// not write, so `../` or a separator in it would place the output outside the
/// work-dir root and break `workdir`'s deletion promise. And sanitizing alone is
/// not injective — `acme/api` and `acme-api` both reduce to `acme-api`, as do
/// `Acme` and `acme` on a case-insensitive filesystem, which macOS is by default.
/// Two repositories sharing a stem share an output directory, a log file
/// (`File::create` truncates), a generated config and a database: the second
/// child overwrites the first's evidence and both report success.
///
/// What: the selection INDEX, which is unique by construction, prefixed to the
/// sanitized name. Sanitizing keeps ASCII alphanumerics, `-`, `_` and `.`; every
/// other byte becomes `-`, and a name that reduces to nothing becomes `repo`.
/// Test: `super::run_tests::a_traversing_repository_name_cannot_escape_the_root`,
/// `super::run_tests::names_that_sanitize_alike_do_not_share_a_log`.
fn stem(index: usize, name: &str) -> String {
    format!("{index:02}-{}", sanitize(name))
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        "repo".to_string()
    } else {
        cleaned
    }
}

/// The tga config document this client generates per repository.
///
/// Why: `tga audit` takes its repository set from a config file, so driving it
/// at one repository means writing one. It is generated rather than authored so
/// the recipient never has to learn tga's schema.
/// What: the two fields tga needs — the repository, and where its database goes.
/// The database is placed under `extract/`, which is the area `workdir` names
/// for exactly that, so it is inside the root that `rm -rf` cleans.
/// The engagement credential is deliberately NOT here; see the module docs.
///
/// #5857: a registered board adds a `jira:` or `linear:` section, and those
/// carry a `${VAR}` reference rather than the board's secret — the secret
/// itself still travels only in the child's environment. Both are omitted when
/// no board is registered, so a repo-only engagement generates exactly the
/// document it always did.
///
/// #5980: `github` is different from `jira`/`linear` in one respect — it is
/// never `None`. Every repository this document is generated for already
/// names itself (the one entry in `repositories`), so its own issue tracker
/// is not something that can be absent from the engagement the way an
/// unregistered board is; see `github_issues`'s module docs for why the
/// section still gets written even when no `gh` credential could be read.
#[derive(Debug, Serialize)]
struct TgaConfig {
    repositories: Vec<TgaRepository>,
    database: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    jira: Option<boards::TgaJira>,
    #[serde(skip_serializing_if = "Option::is_none")]
    linear: Option<boards::TgaLinear>,
    github: github_issues::TgaGithub,
}

#[derive(Debug, Serialize)]
struct TgaRepository {
    path: PathBuf,
    name: String,
}

/// Run `tga audit` over every selected repository.
///
/// Why: #5555 — the sweep the client installs its tooling in order to run.
///
/// # Preconditions
/// The pinned triple is installed and verified (`trusty-audit install`), and
/// `state/`[`SELECTION_FILE`] names at least one repository. Both are checked
/// here and both are refusals, not defaults.
///
/// # Postconditions
/// On `Ok`, every selected repository has an entry in [`RunReport::repos`] in
/// selection order, each child's combined output is at its `log` path, and
/// `state/`[`PROGRESS_FILE`] records the same results with
/// [`RunProgress::complete`] set. [`RunReport::status`] is
/// [`RunStatus::AllSucceeded`] only when every child exited 0 AND left the
/// artifacts [`verify_output`] requires. On `Err`, the checkpoint holds every
/// repository this call settled before the failure PLUS every one it had
/// already decided to carry over, and is marked incomplete; no claim is made
/// about the rest.
///
/// What: checks the tools, reads the selection, then per repository writes a
/// generated tga config under `state/`, spawns the pinned `tga audit` with the
/// pinned `trusty-analyze`/`trusty-review` named by environment, captures the
/// child's combined output into `logs/`, and checks what it produced. A
/// repository whose checkout is missing, whose child fails to start, times out,
/// exits non-zero, or exits 0 having produced nothing is recorded as a failure
/// and the sweep continues — DOC-67 §9's failed-but-continuing model.
/// Test: `super::run_tests`, and `crate::session::session_tests`.
///
/// # Errors
///
/// [`AuditError::ToolsNotInstalled`] or [`AuditError::VersionMismatch`] before
/// anything runs, [`AuditError::NoRepositoriesSelected`] or
/// [`AuditError::TruncatedSelection`] when the selection is unusable, and
/// [`AuditError::WorkDir`] when an output, log or state file cannot be written.
/// A failing repository is NOT an error — it is a recorded failure and a
/// non-`AllSucceeded` status.
/// `progress` is where a front end learns what the sweep is doing, including
/// the stages each `tga audit` child reports from inside itself (#5823).
/// [`Progress::none`] is a complete answer — the sweep behaves identically and
/// nothing is rendered.
/// A repository an earlier run already audited is SKIPPED rather than re-run,
/// unless [`RunOptions::fresh`] says otherwise — `checkpoint::plan` decides,
/// and every skip is announced through `progress` and marked on the report
/// (#5494).
pub async fn sweep(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &RunOptions,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    sweep_with_budget(work, config, options, None, PER_REPO_TIMEOUT, progress).await
}

/// [`sweep`], over boards the caller has already resolved.
///
/// Why: #5857 left `crate::chain` and this module each calling
/// [`boards::resolve`] against their own [`registry::Registry::load`], hours
/// apart — the chain states its board gaps in the Materialize phase and the
/// sweep runs after tool install and every clone. A `taudit remove board`
/// inside that window made the two reads disagree: the report claimed coverage
/// while the child got no board section, and the engagement exited 0 having
/// never read the board. The chain now resolves once and hands the result here,
/// so there is one resolution per invocation rather than two reads that can
/// diverge.
///
/// # Preconditions
/// `boards` was resolved from the same [`EngagementConfig::boards`] passed as
/// `config` — [`boards::Boards::env`] reads the secrets out of `config`, and a
/// section resolved against different credentials would reference a variable
/// this call does not set.
/// Test: `crate::chain::chain_tests::a_board_removed_after_the_chain_resolved_it_still_reaches_the_child`.
///
/// # Errors
///
/// Exactly [`sweep`]'s, minus the registry read this call does not perform.
pub async fn sweep_with_boards(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &RunOptions,
    boards: &boards::Boards,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    sweep_with_budget(
        work,
        config,
        options,
        Some(boards),
        PER_REPO_TIMEOUT,
        progress,
    )
    .await
}

/// [`sweep`], with the per-repository timeout as an argument.
///
/// Why: the timeout arm needs a test, and a test that waits out
/// [`PER_REPO_TIMEOUT`] is not a test. Taking the budget as an argument keeps
/// the elapsed path provable in milliseconds — the same shape as
/// [`crate::workdir::WorkDir::resolve`] taking the environment rather than
/// reading it.
/// Test: `super::run_tests::a_hung_child_is_killed_and_recorded`.
async fn sweep_with_budget(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &RunOptions,
    boards: Option<&boards::Boards>,
    budget: std::time::Duration,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    sweep_with_env(work, config, options, boards, budget, progress, |name| {
        std::env::var(name).ok()
    })
    .await
}

/// [`sweep_with_budget`], with the operator's environment as an argument.
///
/// Why: the inference selection (#5671) branches on what the operator already
/// exported, and every branch has to be provable THROUGH the real child spawn —
/// asserting on [`inference::inference_env`] alone would not catch a wiring
/// mistake between it and the `Command`. Injecting the lookup makes that
/// provable without `std::env::set_var`, which is `unsafe` in edition 2024 and
/// races every other thread in a parallel test binary.
/// Test: `super::run_tests::a_fully_set_operator_environment_is_left_alone`,
/// `super::run_tests::a_partial_operator_environment_refuses_before_any_child_runs`.
#[allow(clippy::too_many_arguments)]
async fn sweep_with_env<F>(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &RunOptions,
    resolved: Option<&boards::Boards>,
    budget: std::time::Duration,
    progress: &Progress,
    operator: F,
) -> Result<RunReport, AuditError>
where
    F: Fn(&str) -> Option<String>,
{
    // #6080: the sweep's own wall clock, for the index it writes at the end.
    let sweep_started = std::time::Instant::now();
    work.create()?;
    let binaries = pinned_binaries(work, &config.tools)?;
    // Resolved once, before any child: a half-named selection is identical for
    // every repository, so failing per-repo would just repeat one misconfiguration.
    // #6135: both halves at once — the pairs the child inherits, and the
    // identity the manifest records. See `inference::Inference`.
    // #6244: read from the injected lookup, BEFORE it is consumed below. What is
    // resolved here is only the operator's half — the `gh` login is folded in
    // after `resolve_github_access`, which is where it is read.
    let declared_credential = git_credentials::GitCredential::of_environment(&operator);
    let inference =
        inference::resolve(!config.openrouter_key.is_empty(), &config.models, operator)?;
    // #6247: resolved ONCE for the sweep, for the same reason the inference
    // selection above is — the budget is a property of the engagement, not of a
    // repository. Every child is handed this value and every manifest records
    // it, so the file cannot name a budget its own investigation never used.
    let investigation = grounding::priority::Budget::for_engagement(&config.report);
    // #5857: ONE resolution per invocation. `crate::chain` resolved the boards
    // an hour ago to state its gaps and hands that result down, so the coverage
    // the report claims and the sections the child gets cannot diverge over a
    // registry edited meanwhile. `taudit run` supplies nothing and resolves here
    // — still once, and still after the refusals above, so a truncated registry
    // is not reported ahead of a missing tool. Every child gets the same
    // sections: a board is a dimension of the engagement, not a unit of the
    // sweep, and tga correlates it against whichever repository it is auditing.
    // #5982: only the arm that resolves states the gaps — see
    // `RunReport::board_gaps`.
    let mut board_gaps = Vec::new();
    let owned;
    let boards = match resolved {
        Some(boards) => boards,
        None => {
            // #5979: the engagement config declares the targets; the working
            // copy answers only for an engagement that has declared none.
            owned = boards::resolve(
                &registry::engagement_targets(Some(config), work)?,
                &config.boards,
            );
            board_gaps.clone_from(&owned.gaps);
            &owned
        }
    };
    let selected = load_selection(work)?;
    // #5980: resolved once for the whole sweep, the same shape as `boards`
    // above — every child gets the same `gh`-derived credential, not a
    // per-repository one. See `github_issues`'s module docs for why a `gh`
    // that cannot answer still lets the sweep proceed.
    let github_access = github_issues::resolve_github_access().await;
    // #6244: whether a `git fetch` can authenticate at all, resolved once for
    // the same reason the inference selection and the boards are — a machine
    // with no credential is a fact about the engagement, not about a repository,
    // and repeating the discovery 59 times is how it ended up stated 59 times
    // and read none. The `gh` login is folded in LAST, matching tga's own order.
    let git_credential = declared_credential.with_gh_login(github_access.raw_token());
    if git_credential.sources().is_empty() {
        // Only now is the remote probe worth its subprocess per repository: with
        // a credential in hand there is nothing to refuse, so nothing to ask.
        let checkouts: Vec<(String, PathBuf)> = selected
            .iter()
            .map(|repo| (repo.name.clone(), absolute_checkout(work, &repo.path)))
            .collect();
        git_credential.refuse_if_fetching(&git_credentials::github_backed(&checkouts).await)?;
    }
    // #6244: the child's git transport reads `GITHUB_TOKEN`, which is not the
    // variable `GithubAccess::env` sets. See `GithubAccess::git_transport_env`.
    let github_access =
        github_access.supplying_git_transport(git_credential.supplies_github_token());
    // #5869: materialized once for the whole sweep — resolving reads
    // `.env.local` and opens the secure store, which is not a per-child cost,
    // let alone a per-line one.
    let scrubber = child_output_scrubber(config, &github_access);
    // #5494: decided once, against the whole selection, so a repository's fate
    // does not depend on a record another repository's child rewrote meanwhile.
    let plan = checkpoint::plan(work, &selected, options.fresh)?;

    // #5823: the operation is announced only once the refusals above are past,
    // so a display never opens on a sweep that is not going to run.
    let total = selected.len();
    progress.operation_started(Operation::Sweep, total);
    // #5494: the record starts as the PLAN, not as an empty list. A repository
    // this run will carry over is already audited and already on disk, and
    // rebuilding the record from only what this loop has visited so far would
    // erase those entries the moment a later repository ends the sweep early —
    // costing the next run the hours the checkpoint exists to save.
    let mut runs: Vec<Option<RepoRun>> = plan.iter().map(|c| c.as_ref().ok().cloned()).collect();
    for ((index, repo), carried) in selected.into_iter().enumerate().zip(plan) {
        runs[index] = Some(match carried {
            Ok(done) => skip_one(&done, index, progress, total),
            Err(why) => {
                announce_recollection(&repo.name, &why, progress);
                run_one(
                    work,
                    config,
                    &binaries,
                    &inference,
                    boards,
                    &github_access,
                    index,
                    repo,
                    budget,
                    investigation,
                    progress,
                    total,
                    &scrubber,
                )
                .await?
            }
        });
        // #5494: the record advances with the work, not after it. A crash, a
        // timeout or a Ctrl-C after this point costs the repositories still to
        // come and none of the ones already done.
        checkpoint::write_progress(
            work,
            &RunProgress::checkpoint(
                &decided(&runs),
                github_issues::GithubCredentialRecord::of(&github_access),
            ),
        )?;
    }

    let report = RunReport::of(decided(&runs)).stating(board_gaps);
    progress.operation_finished(
        Operation::Sweep,
        report.repos.iter().filter(|r| r.result.succeeded()).count(),
        total,
    );
    checkpoint::write_progress(
        work,
        &RunProgress::finished(
            &report,
            github_issues::GithubCredentialRecord::of(&github_access),
        ),
    )?;
    // #6080: written unconditionally, a one-repository sweep included, and after
    // the checkpoint so the index describes a run that is on the record. A
    // partial sweep still gets one — it is where the repositories with no report
    // are named.
    crate::index_report::write_sweep(
        work,
        &report,
        inference.selection.as_ref(),
        sweep_started.elapsed(),
    )?;
    Ok(report)
}

/// Every credential this process can name, for stripping out of a child's log.
///
/// Why: #5869. The `tga audit` child is handed the engagement's OpenRouter key
/// and spawns `trusty-review` with it, so any of them can echo it into the log —
/// and the log is both what a human opens to diagnose a failure and what the
/// planned guided-help path would excerpt into a prompt body sent to OpenRouter.
/// The needle set is deliberately WIDER than the one key this crate hands over:
/// a `gh` token embedded in a git remote URL is a different credential from a
/// different source, and scrubbing only what we passed would miss it.
/// What: the registry-wide resolved set from
/// [`trusty_common::credentials::resolved_secret_values`] — the shared entry
/// point, so a credential added there is scrubbed without this changing — plus
/// [`EngagementConfig::configured_secrets`], which is every credential this
/// engagement's TOML carries and so reaches none of that registry.
///
/// #5857: the board credentials are in that second half for the same reason the
/// OpenRouter key always was. `resolved_secret_values` walks
/// `registered_providers`, and no provider there is a board, so a child that
/// quotes a JIRA token in an auth error would otherwise write it to the log
/// verbatim. [`crate::package::secret_needles`] draws from the same list.
///
/// #5980 CRITICAL 3: `github_access`'s raw token (when `gh auth token`
/// answered) is a THIRD source, alongside `resolved_secret_values` and
/// `configured_secrets` — it comes from the recipient's `gh` keychain, never
/// from `EngagementConfig`, so neither of those two sees it. A child that
/// echoes a rejected GitHub credential back (an auth-failure HTTP body, for
/// instance) would otherwise land the Bearer token in the log unredacted —
/// the same `boards` gap #5857 closed, reopened here for a credential that
/// reaches the child a different way.
///
/// This removes only values this process already holds; see [`crate::relay`]
/// for what that leaves behind.
/// Test: `super::run_tests::a_child_that_echoes_the_key_does_not_leave_it_in_the_log`,
/// `super::run_tests::a_child_that_echoes_a_board_credential_does_not_leave_it_in_the_log`,
/// `super::run_tests::child_output_scrubber_includes_the_github_token`.
fn child_output_scrubber(
    config: &EngagementConfig,
    github_access: &github_issues::GithubAccess,
) -> Scrubber {
    let mut secrets = trusty_common::credentials::resolved_secret_values();
    secrets.extend(config.configured_secrets().into_iter().map(str::to_owned));
    if let Some(token) = github_access.raw_token() {
        secrets.push(token.to_owned());
    }
    Scrubber::over(secrets)
}

/// The entries whose fate this run has settled, in selection order.
///
/// #5494: a slot is `None` only while its repository is still to be audited, so
/// dropping those is what turns the plan-seeded vector into a record.
fn decided(runs: &[Option<RepoRun>]) -> Vec<RepoRun> {
    runs.iter().flatten().cloned().collect()
}

/// Carry an earlier sweep's result over, saying so as it happens.
///
/// #5494: a resumed repository still opens and closes its unit, because a
/// display that shows nothing for it is indistinguishable from one that lost
/// track of it — and the closing verdict carries the reason, so "why was this
/// not re-collected" is answerable from the run's own output.
fn skip_one(done: &RepoRun, index: usize, progress: &Progress, total: usize) -> RepoRun {
    progress.unit_started(Operation::Sweep, done.repo.name.as_str(), index + 1, total);
    progress.unit_finished(
        Operation::Sweep,
        done.repo.name.as_str(),
        UnitOutcome::Skipped(format!(
            "already audited by an earlier run — {}",
            done.output.display()
        )),
    );
    done.clone()
}

/// State why a repository is being audited again rather than carried over.
///
/// Why: #5494 — a resumed sweep that re-collects a repository the operator
/// believed was saved must say what made the record ineligible, or the only
/// visible difference is that the run took four hours longer than expected.
/// What: one stage line inside the unit, through the same relay a `tga audit`
/// child's own stages use. [`Recollect::NotRecorded`] is suppressed: on a first
/// run it is true of every repository and carries no information.
/// Test: `super::run_tests::a_deleted_output_is_re_audited_rather_than_reported_complete`.
fn announce_recollection(name: &str, why: &Recollect, progress: &Progress) {
    if matches!(why, Recollect::NotRecorded) {
        return;
    }
    progress.unit_stage(
        name,
        StageEvent::new("Sweep", name, StageState::Started).with_detail(why.reason()),
    );
}

/// Audit one repository, recording rather than propagating its failure.
///
/// #5823: the unit's start and its verdict bracket everything else, so a
/// display can never be left holding a repository that has already finished —
/// including the arms that never spawn a child at all.
#[allow(clippy::too_many_arguments)]
async fn run_one(
    work: &WorkDir,
    config: &EngagementConfig,
    binaries: &PinnedBinaries,
    inference: &inference::Inference,
    boards: &boards::Boards,
    github_access: &github_issues::GithubAccess,
    index: usize,
    repo: SelectedRepo,
    budget: std::time::Duration,
    investigation: grounding::priority::Budget,
    progress: &Progress,
    total: usize,
    scrubber: &Scrubber,
) -> Result<RepoRun, AuditError> {
    // #6080: the run index states how long each repository took. Started before
    // the checkout check rather than around the child alone, so the figure is
    // the whole cost of this repository — approval, child, grounding — which is
    // what a reader comparing it against the sweep's total expects.
    let started = std::time::Instant::now();
    let stem = stem(index, &repo.name);
    let output = work.path(Area::Output).join(&stem);
    let log = work.path(Area::Logs).join(format!("{stem}.log"));
    let checkout = absolute_checkout(work, &repo.path);
    progress.unit_started(Operation::Sweep, repo.name.as_str(), index + 1, total);

    let mut gaps = Vec::new();
    let result = match prepare(
        work,
        &output,
        &stem,
        &checkout,
        boards,
        github_access,
        repo.github_leg(),
        &binaries.search,
    )? {
        Err(reason) => RepoResult::Failed { reason },
        Ok(config_path) => {
            match spawn_tga(
                binaries,
                config,
                &inference.env,
                boards,
                github_access,
                &config_path,
                &output,
                &log,
                work.root(),
                budget,
                investigation,
                progress,
                &repo.name,
                scrubber,
            )
            .await?
            {
                RepoResult::Succeeded => match verify_output(&output) {
                    Ok(stated) => {
                        gaps = stated;
                        RepoResult::Succeeded
                    }
                    Err(reason) => RepoResult::Failed { reason },
                },
                failed => failed,
            }
        }
    };
    // #6081: index the checkout in trusty-search, measure it with
    // trusty-analyze, and write the ranking into the manifest the child just
    // wrote — the interface trusty-review's investigation pass reads (#6078).
    // It runs AFTER the child because the manifest is what it edits and the
    // child is what writes it, and it asks for that FILE rather than for the
    // child's exit status: a child that failed at a later stage still left a
    // manifest, and re-rendering it is the documented recovery. Gating on the
    // status skipped grounding for exactly that repository, and skipped its gap
    // with it. A repository with no manifest has nothing to ground and nothing
    // to write a gap into — its recorded failure is the trace. Fail-open: a leg
    // that degrades adds a gap line here and in the manifest, never a failed
    // repository.
    let manifest = output.join(AuditManifest::FILE_NAME);
    if manifest.is_file() {
        let tools = grounding::Tools::pinned(binaries.search.clone(), binaries.analyze.clone());
        gaps.extend(
            grounding::ground_manifest(&manifest, &tools, &checkout, &repo.name, investigation)
                .await,
        );
        // #6135: the provider and models this run used, written into the file
        // that ships. Without it a re-render on another machine resolves its own
        // provider from that machine's config — which is how a June-dated local
        // `provider = "bedrock"` hijacked an OpenRouter engagement's render.
        // After grounding, so the two writers of this file serialise; fail-open
        // for the same reason grounding is, because a manifest without the
        // section renders exactly as it did before the key existed.
        if let Some(selection) = &inference.selection
            && let Err(cause) = inference::write_into_manifest(&manifest, selection)
        {
            gaps.push(format!(
                "{}: {cause} — a re-render of this report resolves its own inference provider \
                 instead of reproducing this run's",
                repo.name
            ));
        }
    }
    progress.unit_finished(
        Operation::Sweep,
        repo.name.as_str(),
        match &result {
            RepoResult::Succeeded => UnitOutcome::Succeeded,
            RepoResult::Failed { reason } => UnitOutcome::Failed(reason.clone()),
        },
    );
    Ok(RepoRun {
        repo,
        output,
        log,
        gaps,
        resumed: false,
        duration_ms: Some(millis(started.elapsed())),
        result,
    })
}

/// A measured span as whole milliseconds, saturating rather than wrapping.
fn millis(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// Everything that must be true before a child is worth starting.
///
/// The inner `Result` is the per-repo verdict: `Err(reason)` is a recorded
/// failure for this repository, while the outer `Result` is a failure of the
/// sweep itself (the working directory is not writable).
///
/// #5915: approving the checkout with `trusty-search` belongs here rather than
/// inside the child, for two reasons. It runs before any child spawns, so a
/// refusal costs nothing; and this function already returns `Err(reason)` as a
/// per-repo verdict, which is what turns the refusal into a NAMED failure. Left
/// where it was, a refusal reached nobody — `tga audit` exits 0 whenever the
/// sweep completed, so the run reported success with an empty code-analysis leg.
#[allow(clippy::too_many_arguments)]
fn prepare(
    work: &WorkDir,
    output: &Path,
    stem: &str,
    checkout: &Path,
    boards: &boards::Boards,
    github_access: &github_issues::GithubAccess,
    github_leg: selection::GithubLeg<'_>,
    search: &Path,
) -> Result<Result<PathBuf, String>, AuditError> {
    if !checkout.is_dir() {
        return Ok(Err(format!(
            "no checkout at {} — nothing was audited for this repository",
            checkout.display()
        )));
    }
    if let Err(reason) = approve_for_indexing(search, checkout) {
        return Ok(Err(reason));
    }
    mkdir(output)?;
    let config_path = work.path(Area::State).join(format!("tga-{stem}.yaml"));
    let document = TgaConfig {
        repositories: vec![TgaRepository {
            path: checkout.to_path_buf(),
            name: stem.to_string(),
        }],
        database: work.path(Area::Extract).join(format!("{stem}.db")),
        // #5857: `${TRUSTY_AUDIT_JIRA_TOKEN}`, not the token — see
        // `boards`'s module docs for why the file may never hold the value.
        jira: boards.jira.clone(),
        linear: boards.linear.clone(),
        // #5980: always written — see `github_issues`'s module docs for why
        // this is never `None` the way `jira`/`linear` can be. #6130: what it
        // NAMES is the repository's GitHub identity, which for a local-path
        // target is not its on-disk `local/<name>` and may not exist at all.
        github: github_access.section(github_leg),
    };
    // Infallible in practice — the document is owned strings and paths with no
    // map keys — but a serializer error must not be swallowed into a default.
    let text = serde_yaml::to_string(&document).map_err(|e| AuditError::WorkDir {
        path: config_path.clone(),
        source: std::io::Error::other(e),
    })?;
    std::fs::write(&config_path, text).map_err(|source| AuditError::WorkDir {
        path: config_path.clone(),
        source,
    })?;
    Ok(Ok(config_path))
}

fn mkdir(path: &Path) -> Result<(), AuditError> {
    std::fs::create_dir_all(path).map_err(|source| AuditError::WorkDir {
        path: path.to_path_buf(),
        source,
    })
}

/// A selection path, anchored to the work-dir root when it is relative.
fn absolute_checkout(work: &WorkDir, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        work.root().join(path)
    }
}

/// How long one repository's `tga audit` may take before it is killed.
///
/// Why: the child does network collection and then LLM inference, so it is
/// legitimately slow — the epic describes an hour-scale sweep. But without a
/// ceiling a hung child blocks the sweep forever, and because the progress
/// record is written after every child finishes, an unattended run that hangs
/// leaves NOTHING in `state/` describing how far it got.
///
/// Four hours is chosen as roughly four times the longest sweep anyone has
/// described, so it cannot fire on a slow-but-working run — it exists to turn an
/// indefinite hang into a recorded failure, not to bound normal work. It is
/// per repository, not per sweep.
/// Test: `super::run_tests::a_hung_child_is_killed_and_recorded`, which uses
/// `sweep_with_timeout` rather than waiting for this value.
pub const PER_REPO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4 * 60 * 60);

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::progress::{ProgressUpdate, Recorder, StageEvent, StageState};
    use crate::tools::{self, RequiredTool};
    // The selection document itself, so a test can write a torn one by hand.
    use crate::run::selection::Selection;

    const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    fn config() -> EngagementConfig {
        EngagementConfig::from_toml(CONFIG, Path::new("engagement.toml")).expect("parses")
    }

    fn work_in(dir: &Path) -> WorkDir {
        let work = WorkDir::new(dir.join("work"));
        work.create().expect("create");
        work
    }

    fn select(work: &WorkDir, entries: &[(&str, &str)]) {
        let repositories: Vec<SelectedRepo> = entries
            .iter()
            .map(|(name, path)| SelectedRepo {
                name: (*name).to_owned(),
                path: PathBuf::from(*path),
                github_slug: None,
                github_absent: None,
            })
            .collect();
        let text = toml::to_string_pretty(&Selection {
            count: repositories.len(),
            repositories,
        })
        .expect("render");
        std::fs::write(selection_path(work), text).expect("write selection");
    }

    /// A stub `tga` that writes the manifest a real one would, so a run this
    /// test expects to succeed passes `verify_output`.
    fn writes_a_manifest(extra_gap: Option<&str>) -> String {
        let gaps = match extra_gap {
            Some(gap) => format!("gaps = [\"{gap}\"]\\n"),
            None => String::new(),
        };
        format!(
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             mkdir -p \"$out\"\n\
             printf '[report]\\ntitle = \"Acme\"\\n{gaps}\\n[[repositories]]\\n\
             name = \"acme\"\\npath = \"/r\"\\n' > \"$out/manifest.toml\"\nexit 0\n"
        )
    }

    /// #6244: the preflight refuses on a machine with no credential, and only
    /// when the engagement provably fetches. These checkouts name no remote, so
    /// the sweep must run whatever this machine happens to have — a refusal here
    /// would strand every engagement over paths on disk.
    ///
    /// The lookup answers for nothing, so no source is declared: on a machine
    /// where `gh` cannot answer either, this is the arm that reaches the remote
    /// probe and finds nothing to fetch.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_over_checkouts_with_no_remote_needs_no_credential() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep_with_operator(&work, |_| None)
            .await
            .expect("a sweep that fetches nothing needs no credential");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");
    }

    /// A stub `tga` that records the investigation budget it was handed (#6247).
    fn records_its_budget_env() -> String {
        format!(
            "{}{}",
            writes_a_manifest(None).trim_end_matches("exit 0\n"),
            "{\n  echo \"files=$TRUSTY_AUDIT_INVESTIGATE_MAX_FILES\"\n  \
             echo \"bytes=$TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES\"\n} \
             > \"$out/budget-env.txt\"\nexit 0\n",
        )
    }

    /// An engagement that asks for a wider investigation than the default.
    ///
    /// 77 rather than a round number: it matches neither
    /// [`grounding::priority::DEFAULT_MAX_FILES`] nor `trusty-review`'s own
    /// default, so a value arriving from either tier is visible as a wrong
    /// number rather than as a coincidence.
    fn config_declaring_a_budget() -> EngagementConfig {
        EngagementConfig::from_toml(
            &format!("{CONFIG}\n[report]\ninvestigate_max_files = 77\n"),
            Path::new("engagement.toml"),
        )
        .expect("parses")
    }

    /// 🔴 #6247: the budget an engagement declares must reach the process that
    /// samples the files, not just the manifest that describes the run.
    ///
    /// Asserts on the environment the SPAWNED PROCESS received, because that is
    /// the only channel that reaches `trusty-review` before it renders — the
    /// manifest is written by this child and edited afterwards, so a budget
    /// recorded there reaches a re-render and never this run's report.
    ///
    /// Against `3771644d0` this fails on the first assertion: `spawn_tga` read
    /// `Budget::from_env()`, so the child was handed the compiled default 240
    /// whatever the engagement declared, and the operator's 240-file manifest
    /// sat beside a report claiming 40.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_environment_carries_the_engagement_investigation_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &records_its_budget_env());
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(
            &work,
            &config_declaring_a_budget(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let seen = std::fs::read_to_string(report.repos[0].output.join("budget-env.txt"))
            .expect("the stub recorded its environment");
        assert!(seen.contains("files=77"), "{seen}");
        // Derived from the declared file count, so a raised file budget never
        // meets an unraised byte budget (#6148).
        assert!(
            seen.contains(&format!(
                "bytes={}",
                77 * grounding::priority::BYTES_PER_FILE
            )),
            "{seen}"
        );
    }

    /// 🔴 #6247: the manifest that ships must name the budget the investigation
    /// actually ran under — one resolution, two consumers.
    ///
    /// Against `3771644d0` the two were resolved independently: the child got
    /// `Budget::from_env()` and `ground_manifest` re-resolved from the manifest
    /// the child had just written, so the file could state a budget no sampler
    /// used. Here the child records 77 and the file must agree.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_recorded_budget_is_the_one_the_child_ran_under() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &records_its_budget_env());
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(
            &work,
            &config_declaring_a_budget(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");

        let written = std::fs::read_to_string(report.repos[0].output.join("manifest.toml"))
            .expect("the child wrote a manifest");
        let parsed: toml::Value = written.parse().expect("the manifest is TOML");
        assert_eq!(
            parsed["report"]["investigate_max_files"].as_integer(),
            Some(77),
            "{written}"
        );
        let seen = std::fs::read_to_string(report.repos[0].output.join("budget-env.txt"))
            .expect("the stub recorded its environment");
        assert!(seen.contains("files=77"), "{seen}");
    }

    /// A stub `tga` shaped like the child of the 2026-08-19 dogfood run: it
    /// writes the manifest a real one would, and THEN fails — the shape of a
    /// render stage that failed after everything before it was collected.
    fn writes_a_manifest_then_fails() -> String {
        writes_a_manifest(None).replace("\nexit 0\n", "\nexit 1\n")
    }

    /// A stub `trusty-search` that approves whatever it is asked to approve.
    ///
    /// #5915: `prepare` now runs `trusty-search index add <checkout>` before any
    /// tga child, so a stub that refuses fails the repository before the
    /// behaviour under test is reached. Every test that is about tga gets this
    /// one; the two that are about the approval install their own.
    const SEARCH_APPROVES: &str = "#!/bin/sh\nexit 0\n";

    /// Place stub binaries AND the version record, which together are what
    /// `pinned_binaries` accepts.
    ///
    /// `script` is the TGA-side behaviour under test. `trusty-search` gets
    /// [`SEARCH_APPROVES`] instead, because it answers a different question and
    /// a shared script conflates the two (#5915).
    fn install_stubs(work: &WorkDir, script: &str) {
        install_stubs_with_search(work, script, SEARCH_APPROVES);
    }

    /// [`install_stubs`], with the `trusty-search` stub named separately.
    fn install_stubs_with_search(work: &WorkDir, script: &str, search: &str) {
        for tool in RequiredTool::ALL {
            let path = tool.path_in(work);
            let body = if tool == RequiredTool::TrustySearch {
                search
            } else {
                script
            };
            std::fs::write(&path, body).expect("stub binary");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
        }
        let record = format!(
            "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"{tga}\"\n\
             [[tools]]\ncrate_name = \"trusty-search\"\nversion = \"0.47.0\"\nbinary = \"{s}\"\n\
             [[tools]]\ncrate_name = \"trusty-analyze\"\nversion = \"0.9.2\"\nbinary = \"{a}\"\n\
             [[tools]]\ncrate_name = \"trusty-review\"\nversion = \"0.15.1\"\nbinary = \"{r}\"\n",
            tga = RequiredTool::Tga.path_in(work).display(),
            s = RequiredTool::TrustySearch.path_in(work).display(),
            a = RequiredTool::TrustyAnalyze.path_in(work).display(),
            r = RequiredTool::TrustyReview.path_in(work).display(),
        );
        std::fs::write(tools::record_path(work), record).expect("write record");
    }

    fn make_repo(work: &WorkDir, name: &str) {
        std::fs::create_dir_all(work.path(Area::Repos).join(name)).expect("mkdir repo");
    }

    /// The index a sweep leaves in `out/` (#6080).
    fn index_of(work: &WorkDir) -> String {
        std::fs::read_to_string(
            work.path(Area::Output)
                .join(crate::index_report::INDEX_FILE),
        )
        .expect("the sweep writes an index")
    }

    /// 🔴 #6080: a sweep writes an index beside its reports, and a run over ONE
    /// repository gets one too — "there is only one" is a coverage fact the
    /// index is the place to state, not a reason to skip it.
    ///
    /// Against `70c52f5b5` this fails on the very first line: `out/` held the
    /// per-repository directories and nothing that said what they were, which
    /// versions produced them, or when.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_writes_an_index_beside_its_reports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let index = index_of(&work);
        assert!(index.contains("Reports: 1 of 1 repository"), "{index}");
        // The versions responsible, from the record this engagement installed.
        assert!(index.contains("## Versions"), "{index}");
        assert!(
            index.contains("| `tga` | 2.9.4 | recorded at install |"),
            "{index}"
        );
        assert!(index.contains("| `trusty-review` | 0.15.1 |"), "{index}");
        // The manifest the child wrote is linked, relative to the index itself.
        assert!(
            index.contains("[00-acme-api/manifest.toml](00-acme-api/manifest.toml)"),
            "{index}"
        );
        // And the directory explains itself.
        assert!(index.contains("## What is in this directory"), "{index}");
        assert!(index.contains("../extract/<NN>-<repo>.db"), "{index}");
    }

    /// #6080: the checkpoint records how long each repository took, so a resumed
    /// sweep can report the run that did the work rather than the second it took
    /// to verify the output.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_swept_repository_records_how_long_it_took() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        one_repo_ready(&work);

        let first = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        let measured = first.repos[0].duration_ms.expect("a measured duration");

        let second = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert!(second.repos[0].resumed, "{:?}", second.repos[0]);
        assert_eq!(
            second.repos[0].duration_ms,
            Some(measured),
            "a carried-over entry must keep the duration of the run that earned it"
        );
        assert!(
            index_of(&work).contains("carried over from an earlier run"),
            "the index must not report a resumed repository as work done now"
        );
    }

    /// 🔴 A sweep in which a repository failed still writes an index, and that
    /// index NAMES the repository with no report and why — a partial run's index
    /// being silently one section shorter is the fail-open shape this file
    /// exists to avoid.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_partial_sweep_indexes_the_repository_that_produced_no_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        two_repos_ready(&work, "acme-web");

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::Partial, "{report:?}");

        let index = index_of(&work);
        assert!(index.contains("Reports: 1 of 2 repositories"), "{index}");
        assert!(index.contains("### acme-web"), "{index}");
        assert!(index.contains("No report — `tga audit` exited"), "{index}");
        assert!(
            index.contains("[../logs/01-acme-web.log](../logs/01-acme-web.log)"),
            "the failed repository's log must be linked: {index}"
        );
    }

    /// The writer's own round trip: what [`save_selection`] leaves behind is
    /// what [`load_selection`] accepts, `count` and all.
    #[test]
    fn a_saved_selection_reads_back_whole() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let repos = vec![
            SelectedRepo {
                name: "acme/api".to_owned(),
                path: PathBuf::from("repos/acme/api"),
                github_slug: None,
                github_absent: None,
            },
            SelectedRepo {
                name: "acme/web".to_owned(),
                path: PathBuf::from("repos/acme/web"),
                github_slug: None,
                github_absent: None,
            },
        ];
        save_selection(&work, &repos).expect("the selection writes");

        let text = std::fs::read_to_string(selection_path(&work)).expect("read");
        assert!(
            text.starts_with("count = 2"),
            "the count must precede the entries, or a truncated write is undetectable:\n{text}"
        );
        assert_eq!(load_selection(&work).expect("reads back"), repos);
    }

    /// The obligation the atomic rename exists for: a reader must never see a
    /// prefix of a write, and two writers must not build the same temporary
    /// file. Both are exercised at once — readers run throughout, and every
    /// read either finds no file or finds a whole one.
    #[test]
    fn racing_writers_never_leave_a_torn_selection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let entry = |n: usize| SelectedRepo {
            name: format!("acme/repo-{n}"),
            path: PathBuf::from(format!("repos/acme/repo-{n}")),
            github_slug: None,
            github_absent: None,
        };

        std::thread::scope(|scope| {
            for writer in 1..=4usize {
                let work = &work;
                scope.spawn(move || {
                    // Different lengths, so a torn read is a mismatched count
                    // rather than an identical file written twice.
                    let repos: Vec<SelectedRepo> = (0..writer * 3).map(entry).collect();
                    for _ in 0..20 {
                        save_selection(work, &repos).expect("a racing write still succeeds");
                    }
                });
            }
            scope.spawn(|| {
                for _ in 0..200 {
                    match load_selection(&work) {
                        Ok(repos) => assert!(!repos.is_empty()),
                        // Absent is legal only before the first rename lands.
                        Err(AuditError::NoRepositoriesSelected { .. }) => {}
                        Err(e) => panic!("a reader saw a torn selection: {e}"),
                    }
                }
            });
        });

        let repos = load_selection(&work).expect("the last write is whole");
        assert!([3, 6, 9, 12].contains(&repos.len()), "{repos:?}");
        // Nothing may be left in the state area but the file itself.
        let leftovers: Vec<PathBuf> = std::fs::read_dir(work.path(Area::State))
            .expect("read state")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn an_absent_selection_is_a_refusal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let err = load_selection(&work).expect_err("nothing selected is not a zero-repo success");
        assert!(
            matches!(err, AuditError::NoRepositoriesSelected { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn an_empty_selection_is_the_same_refusal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        std::fs::write(selection_path(&work), "count = 0\nrepositories = []\n").expect("write");
        let err = load_selection(&work).expect_err("an empty list audits nothing");
        assert!(
            matches!(err, AuditError::NoRepositoriesSelected { .. }),
            "{err:?}"
        );
    }

    /// A producer that crashed mid-write leaves valid TOML holding a prefix.
    /// Without the declared count that is indistinguishable from a smaller
    /// selection, and the sweep would report success over a subset.
    #[test]
    fn a_truncated_selection_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        std::fs::write(
            selection_path(&work),
            "count = 3\n\n[[repositories]]\nname = \"acme-api\"\npath = \"repos/acme-api\"\n",
        )
        .expect("write");

        let err = load_selection(&work).expect_err("a prefix is not a selection");
        let AuditError::TruncatedSelection {
            declared, found, ..
        } = err
        else {
            panic!("expected TruncatedSelection, got {err:?}");
        };
        assert_eq!((declared, found), (3, 1));
    }

    /// A file with no count cannot be checked, so it is not a valid selection.
    #[test]
    fn a_selection_without_a_count_does_not_load() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        std::fs::write(
            selection_path(&work),
            "[[repositories]]\nname = \"acme-api\"\npath = \"repos/acme-api\"\n",
        )
        .expect("write");
        let err = load_selection(&work).expect_err("count is required");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn the_selection_contract_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        select(&work, &[("acme-api", "repos/acme-api")]);
        let selected = load_selection(&work).expect("reads");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "acme-api");
        assert_eq!(selected[0].path, PathBuf::from("repos/acme-api"));
    }

    /// The pinned tools are a precondition, and there is no PATH fallback.
    #[tokio::test]
    async fn a_run_without_the_pinned_tools_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        select(&work, &[("acme-api", "repos/acme-api")]);

        let err = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect_err("no pinned tga means no run");
        let AuditError::ToolsNotInstalled { missing } = err else {
            panic!("expected ToolsNotInstalled, got {err:?}");
        };
        assert_eq!(missing.len(), RequiredTool::ALL.len());
    }

    /// Auto-install decides whether to download from [`tools::unsatisfied`],
    /// and this preflight decides whether to run. If the two disagree,
    /// auto-install either downloads on every sweep or skips a download this
    /// preflight then refuses over — so they are asserted to agree (#5797).
    #[test]
    fn nothing_unsatisfied_is_exactly_what_the_preflight_accepts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let pins = config().tools;

        // Unsatisfied and refused.
        assert!(!tools::unsatisfied(&work, &pins).expect("reads").is_empty());
        assert!(pinned_binaries(&work, &pins).is_err());

        // Satisfied and accepted.
        install_stubs(&work, "#!/bin/sh\nexit 0\n");
        assert!(tools::unsatisfied(&work, &pins).expect("reads").is_empty());
        assert!(pinned_binaries(&work, &pins).is_ok());
    }

    /// A binary this client did not install and verify is not a usable binary.
    #[test]
    fn an_unverified_binary_does_not_count_as_installed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        for tool in RequiredTool::ALL {
            std::fs::write(tool.path_in(&work), b"stub").expect("stub");
        }
        let err = pinned_binaries(&work, &config().tools)
            .expect_err("no version record means unverified");
        let AuditError::ToolsNotInstalled { missing } = err else {
            panic!("expected ToolsNotInstalled, got {err:?}");
        };
        assert_eq!(missing.len(), RequiredTool::ALL.len());
    }

    /// Install and run are separate steps, so the config can be bumped between
    /// them. Running the older binary anyway is the #5454 skew class.
    #[test]
    fn a_binary_installed_at_a_different_pin_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, "#!/bin/sh\nexit 0\n"); // records tga 2.9.4

        let bumped = EngagementConfig::from_toml(
            &CONFIG.replace("tga = \"2.9.4\"", "tga = \"2.10.0\""),
            Path::new("engagement.toml"),
        )
        .expect("parses");

        let err = pinned_binaries(&work, &bumped.tools).expect_err("2.9.4 is not 2.10.0");
        let AuditError::VersionMismatch {
            tool,
            pinned,
            installed,
        } = err
        else {
            panic!("expected VersionMismatch, got {err:?}");
        };
        assert_eq!(
            (tool, pinned.as_str(), installed.as_str()),
            ("tga", "2.10.0", "2.9.4")
        );
    }

    #[test]
    fn a_traversing_repository_name_cannot_escape_the_root() {
        let work = WorkDir::new("/work");
        for name in ["../../etc", "a/b", "..", "", "he re"] {
            let s = stem(0, name);
            let path = work.path(Area::Output).join(&s);
            assert!(path.starts_with(work.root()), "{name:?} escaped as {s:?}");
            assert!(!s.contains('/'), "{name:?} kept a separator: {s:?}");
        }
    }

    /// Sanitizing alone is not injective. Two repositories sharing a stem share
    /// an output directory and a log file, and `File::create` truncates — the
    /// second child would destroy the first's evidence with both reporting
    /// success.
    #[test]
    fn names_that_sanitize_alike_do_not_share_a_log() {
        let colliding = [("acme/api", "acme-api"), ("Acme", "acme"), ("a b", "a-b")];
        for (i, (left, right)) in colliding.iter().enumerate() {
            let a = stem(i * 2, left);
            let b = stem(i * 2 + 1, right);
            assert_ne!(a, b, "{left:?} and {right:?} collided");
            assert_ne!(
                a.to_lowercase(),
                b.to_lowercase(),
                "{left:?} and {right:?} collide on a case-insensitive filesystem"
            );
        }
    }

    #[test]
    fn status_distinguishes_partial_from_total_failure() {
        let ok = RepoRun {
            repo: SelectedRepo {
                name: "a".into(),
                path: "a".into(),
                github_slug: None,
                github_absent: None,
            },
            output: "/o/a".into(),
            log: "/l/a.log".into(),
            gaps: Vec::new(),
            resumed: false,
            duration_ms: None,
            result: RepoResult::Succeeded,
        };
        let bad = RepoRun {
            result: RepoResult::Failed {
                reason: "exited with code 1".into(),
            },
            ..ok.clone()
        };
        assert_eq!(
            RunReport::of(vec![ok.clone()]).status,
            RunStatus::AllSucceeded
        );
        assert_eq!(
            RunReport::of(vec![ok.clone(), bad.clone()]).status,
            RunStatus::Partial
        );
        assert_eq!(RunReport::of(vec![bad]).status, RunStatus::AllFailed);
    }

    /// The error arm this module exists for: a child that exits non-zero must
    /// not read as a success, and the sweep must not stop at it.
    #[cfg(unix)]
    /// #5915: the refusal that used to reach nobody. `trusty-search` is
    /// default-deny, tga's index call uses the strict denylist check, and
    /// `tga audit` exits 0 whenever its sweep completed — so an unapproved
    /// checkout produced a SUCCESSFUL run whose code-analysis leg had read
    /// nothing. It must now be a named per-repository failure instead, and the
    /// tga child must not run at all.
    #[tokio::test]
    async fn an_unapprovable_checkout_fails_the_repository_by_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let ran = tmp.path().join("tga-ran");
        install_stubs_with_search(
            &work,
            &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", ran.display()),
            "#!/bin/sh\necho 'indexing refused: not approved for indexing' >&2\nexit 1\n",
        );
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");

        assert_eq!(report.status, RunStatus::AllFailed, "{report:?}");
        let RepoResult::Failed { reason } = &report.repos[0].result else {
            panic!("a refused approval must fail the repository: {report:?}");
        };
        assert!(reason.contains("trusty-search index add"), "{reason}");
        assert!(reason.contains("not approved for indexing"), "{reason}");
        assert!(
            !ran.exists(),
            "tga was spawned for a checkout it could never have indexed"
        );
    }

    /// The other half: an approval that succeeds leaves the sweep exactly as it
    /// was, so #5915's fix costs a working run nothing.
    #[tokio::test]
    async fn an_approved_checkout_proceeds_to_the_child() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");

        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");
    }

    #[tokio::test]
    async fn a_failing_child_is_recorded_and_the_sweep_continues() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, "#!/bin/sh\necho 'sweep failed'\nexit 3\n");
        make_repo(&work, "acme-api");
        make_repo(&work, "acme-web");
        select(
            &work,
            &[
                ("acme-api", "repos/acme-api"),
                ("acme-web", "repos/acme-web"),
            ],
        );

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllFailed);
        assert_eq!(report.repos.len(), 2, "every repository was attempted");
        for run in &report.repos {
            assert!(!run.result.succeeded());
            let log = std::fs::read_to_string(&run.log).expect("log kept");
            assert!(log.contains("sweep failed"), "{log}");
        }
        // And it is on disk, not only in the returned value.
        let recorded = read_progress(&work)
            .expect("record reads")
            .expect("present");
        assert_eq!(recorded.report(), report);
    }

    /// A checkout the selection names but that is not there fails that
    /// repository alone.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_progress_record_survives_a_partial_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(
            &work,
            &[("acme-api", "repos/acme-api"), ("gone", "repos/gone")],
        );

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::Partial);
        assert_eq!(report.failures().count(), 1);
        let failed = report.failures().next().expect("one failure");
        assert_eq!(failed.repo.name, "gone");
        assert!(
            matches!(&failed.result, RepoResult::Failed { reason } if reason.contains("no checkout")),
            "{:?}",
            failed.result
        );

        let recorded = read_progress(&work)
            .expect("record reads")
            .expect("present");
        assert_eq!(recorded.status, RunStatus::Partial);
    }

    /// Every file anywhere under the root, at any depth.
    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    /// The credential reaches the child by environment, and no file this crate
    /// writes carries it.
    ///
    /// Scope, stated honestly: this proves what THIS crate writes. The child's
    /// own artifacts are tga's contract — it redacts its configured secrets out
    /// of the manifest itself (`tga::audit::gaps`) — and with a stub standing in
    /// for `tga` this test says nothing about the real binary's output. What it
    /// does cover is every file under the whole root at any depth, `extract/`
    /// included, which is where a leak from the generated config or the log
    /// would land.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_key_reaches_the_child_by_environment_and_is_never_written_down() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let mut script = String::from(
            // #5670: the search binary is checked alongside the other two — on a
            // recipient's clean machine the pinned copy is the only one there is,
            // and without it named here `tga audit`'s search preflight falls
            // through to a PATH lookup and refuses the run.
            "#!/bin/sh\ntest -n \"$OPENROUTER_API_KEY\" || exit 9\n\
             test -n \"$TRUSTY_REVIEW_BIN\" || exit 8\n\
             test -n \"$TRUSTY_ANALYZE_BIN\" || exit 7\n\
             test -n \"$TRUSTY_SEARCH_BIN\" || exit 6\n",
        );
        script.push_str(writes_a_manifest(None).trim_start_matches("#!/bin/sh\n"));
        install_stubs(&work, &script);
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let files = files_under(work.root());
        assert!(files.len() > 3, "the walk found almost nothing: {files:?}");
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !text.contains("sk-or-v1-not-a-real-key"),
                "{} carries the key",
                path.display()
            );
        }
    }

    /// Why: #5869 — the test above walks the whole root but its stub never
    /// ECHOES the key, so it passed against a log written verbatim. This is the
    /// arm that did not hold: a child that prints the credential back, on both
    /// streams, in the two shapes a real one would — a provider's rejection body
    /// and a `git` remote URL in a clone failure.
    /// What: the key reaches neither the log nor any other file the run wrote,
    /// and the surrounding diagnostic text survives so the log is still useful.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_echoes_the_key_does_not_leave_it_in_the_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let mut script = String::from(
            "#!/bin/sh\n\
             echo \"ERROR 401 from provider: {\\\"message\\\":\\\"key $OPENROUTER_API_KEY \
             is not valid\\\"}\"\n\
             echo \"fatal: could not read from \
             https://x-access-token:$OPENROUTER_API_KEY@github.com/acme/api\" >&2\n",
        );
        script.push_str(writes_a_manifest(None).trim_start_matches("#!/bin/sh\n"));
        install_stubs(&work, &script);
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let log = std::fs::read_to_string(&report.repos[0].log).expect("read the child log");
        assert!(
            !log.contains("sk-or-v1-not-a-real-key"),
            "the log carries the key:\n{log}"
        );
        // The child really did echo it — otherwise the assertion above proves
        // nothing about the filter.
        assert_eq!(
            log.matches("[REDACTED]").count(),
            2,
            "both echoes must be masked, one per stream:\n{log}"
        );
        // The log is still a log: masking replaced the key, not the diagnosis.
        assert!(log.contains("ERROR 401 from provider"), "{log}");
        assert!(log.contains("github.com/acme/api"), "{log}");

        for path in files_under(work.root()) {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !text.contains("sk-or-v1-not-a-real-key"),
                "{} carries the key",
                path.display()
            );
        }
    }

    // ── #5857: a registered board reaches the child ──────────────────────────

    /// The JIRA API token this engagement is given. Never expected on disk.
    const JIRA_TOKEN: &str = "jira-token-never-on-disk";

    /// The Linear personal API key. Never expected on disk.
    const LINEAR_KEY: &str = "lin_api_never-on-disk";

    /// An engagement carrying both board credentials.
    fn board_config() -> EngagementConfig {
        let text = format!(
            "{CONFIG}\n[boards.jira]\nurl = \"https://acme.atlassian.net\"\n\
             email = \"auditor@acme.example\"\ntoken = \"{JIRA_TOKEN}\"\n\
             \n[boards.linear]\napi_key = \"{LINEAR_KEY}\"\n"
        );
        EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses")
    }

    /// Register boards the way `taudit add board` does, so the sweep reads them
    /// from the file it actually reads rather than from an injected value.
    fn register_boards(work: &WorkDir, specs: &[&str]) {
        let mut targets = registry::Registry::default();
        for spec in specs {
            targets
                .insert(registry::parse(Some(registry::TargetKind::Board), spec).expect("parses"));
        }
        targets.save(work).expect("write registry");
    }

    /// The generated config carries a REFERENCE to each board credential and
    /// never the credential, and the JIRA section carries both halves of the
    /// Basic-auth pair.
    ///
    /// The `username` assertion is the one that catches the silent failure:
    /// `JiraClient::new` builds its credential from `(&username, &token)` and
    /// takes the `(Some, Some)` arm only, so a section with the token alone
    /// runs UNAUTHENTICATED and reports no error at all.
    ///
    /// Against `origin/main` this fails at the first assertion — `TgaConfig` had
    /// no `jira` field, so the board never reached the document.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_generated_config_references_the_board_secret_and_never_holds_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);
        register_boards(&work, &["jira:ACME", "linear:ENG"]);

        let report = sweep(
            &work,
            &board_config(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let generated =
            std::fs::read_to_string(work.path(Area::State).join("tga-00-acme-api.yaml"))
                .expect("the generated tga config");
        assert!(
            generated.contains("${TRUSTY_AUDIT_JIRA_TOKEN}"),
            "{generated}"
        );
        assert!(
            generated.contains("${TRUSTY_AUDIT_LINEAR_API_KEY}"),
            "{generated}"
        );
        assert!(
            generated.contains("username: auditor@acme.example"),
            "a token without a username is an unauthenticated client: {generated}"
        );
        assert!(generated.contains("project_key: ACME"), "{generated}");
        assert!(generated.contains("- ENG"), "{generated}");

        // Not just this file: every file the run left anywhere under the root.
        for path in files_under(work.root()) {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !text.contains(JIRA_TOKEN) && !text.contains(LINEAR_KEY),
                "{} carries a board credential",
                path.display()
            );
        }
    }

    /// #5980: the load-bearing regression. Against `origin/main` before this
    /// change, `TgaConfig` carried no `github` field at all, so a registered
    /// repository's own issues were never collected — not because a fetch
    /// failed and was reported, but because nothing ever asked. That is the
    /// silent-empty-success shape #5982 already fixed once for a Linear board
    /// stored as an id; this proves the same shape cannot recur for a
    /// repository's own GitHub issues by asserting the section is present
    /// with NO board registered at all — the case that most directly exposes
    /// an omission, since there is no `boards.jira`/`boards.linear` machinery
    /// nearby that could be mistaken for covering it.
    #[tokio::test]
    async fn every_registered_repository_generates_a_github_section() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);
        // Deliberately no `register_boards` call — a repo-only engagement.

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let generated =
            std::fs::read_to_string(work.path(Area::State).join("tga-00-acme-api.yaml"))
                .expect("the generated tga config");
        assert!(
            generated.contains("github:"),
            "a repo-only engagement must still collect its own issues: {generated}"
        );
        assert!(generated.contains("repo: acme-api"), "{generated}");
    }

    /// The `taudit run` half of #5982. `boards::resolve` states a gap for a
    /// board it will not collect, and until now only `crate::chain` read it —
    /// this path resolved the boards, dropped `Boards::gaps` on the floor, and
    /// returned `AllSucceeded` with no `linear:` section and nothing said. The
    /// gap is a dimension of the sweep, not a repository that failed, so the
    /// status stays `AllSucceeded` and `Outcome::exit_code` is what stops it
    /// reading as a whole engagement.
    ///
    /// Against `9ee9cc386` the `board_gaps` field does not exist.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_board_the_sweep_cannot_collect_is_stated_on_the_run_path() {
        /// A Linear team id, the shape a registry written before #5982 holds.
        const TEAM_ID: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);
        register_boards(&work, &["linear:ENG", &format!("linear:{TEAM_ID}")]);

        let report = sweep(
            &work,
            &board_config(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");

        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");
        assert_eq!(report.board_gaps.len(), 1, "{:?}", report.board_gaps);
        assert!(
            report.board_gaps[0].contains(TEAM_ID),
            "{:?}",
            report.board_gaps
        );
        assert_eq!(
            crate::session::Outcome::Run(report).exit_code(),
            crate::session::EXIT_INCOMPLETE,
            "a sweep that skipped a registered board must not chain onward"
        );

        // The team that CAN collect is untouched by the one that cannot.
        let generated =
            std::fs::read_to_string(work.path(Area::State).join("tga-00-acme-api.yaml"))
                .expect("the generated tga config");
        assert!(generated.contains("- ENG"), "{generated}");
        assert!(!generated.contains(TEAM_ID), "{generated}");
    }

    /// 🔴 #6130's sweep half. A local-path target whose checkout named no
    /// GitHub remote must reach tga with NO `github.repo` and with the
    /// declaration in its place, and the sweep must still complete — the
    /// self-audit's `collect` stage failed closed on 3152 404s and the audit
    /// refused to package.
    ///
    /// Against the pre-fix code this fails at the second assertion: the section
    /// carried `repo: local/apex`, the identity that does not exist.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_repo_with_no_github_remote_declares_the_leg_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "local/apex");
        save_selection(
            &work,
            &[SelectedRepo {
                name: "local/apex".to_owned(),
                path: PathBuf::from("repos/local/apex"),
                github_slug: None,
                github_absent: Some(
                    "`local/apex` was audited from the checkout at /srv/apex, whose `origin` \
                     remote names no repository on github.com"
                        .to_owned(),
                ),
            }],
        )
        .expect("write selection");

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(
            report.status,
            RunStatus::AllSucceeded,
            "a declared-absent GitHub leg must not stop the run: {report:?}"
        );

        let generated =
            std::fs::read_to_string(work.path(Area::State).join("tga-00-local-apex.yaml"))
                .expect("the generated tga config");
        assert!(
            !generated.contains("repo: local/apex"),
            "the synthetic owner must never reach tga's github section: {generated}"
        );
        assert!(
            generated.contains("work_items_unavailable:"),
            "the absence must be declared, not silent: {generated}"
        );
        assert!(
            generated.contains("names no repository on github.com"),
            "the declaration must carry the reason: {generated}"
        );
    }

    /// A selection file written before #6130 carries neither field, so the only
    /// evidence left of what a target was is its owner segment. Both arms are
    /// pinned here because guessing wrong either way is a real defect: a
    /// `local/` entry queried as a slug is the bug, and a real `owner/repo`
    /// read as absent silently drops a leg that works.
    #[test]
    fn a_legacy_selection_still_resolves_both_shapes() {
        let legacy = |name: &str| SelectedRepo {
            name: name.to_owned(),
            path: PathBuf::from("repos").join(name),
            github_slug: None,
            github_absent: None,
        };
        assert_eq!(
            legacy("acme/api").github_leg(),
            GithubLeg::Present("acme/api")
        );
        let local = legacy("local/apex");
        let GithubLeg::Absent(reason) = local.github_leg() else {
            panic!("a `local/` owner names no GitHub repository");
        };
        assert!(reason.contains("path on disk"), "{reason}");
    }

    /// 🔴 #6130 review: a hand-edited `selected-repos.toml` carrying a blank
    /// field must not declare anything. A blank `github_absent` would reach tga
    /// as a reason naming nothing, whose own blank filter then drops the leg
    /// back into the blind-warn path this issue closes; a blank `github_slug`
    /// would write `repo: ""` into the generated config.
    #[test]
    fn a_blank_field_is_not_a_value() {
        let blank = |slug: Option<&str>, absent: Option<&str>| SelectedRepo {
            name: "local/apex".to_owned(),
            path: PathBuf::from("repos/local/apex"),
            github_slug: slug.map(str::to_owned),
            github_absent: absent.map(str::to_owned),
        };

        let blank_absent = blank(None, Some("   "));
        let GithubLeg::Absent(reason) = blank_absent.github_leg() else {
            panic!("still absent — but with a reason that says something");
        };
        assert!(reason.contains("path on disk"), "{reason}");

        let blank_slug = blank(Some(""), Some("the checkout names no GitHub remote"));
        assert_eq!(
            blank_slug.github_leg(),
            GithubLeg::Absent("the checkout names no GitHub remote"),
            "an empty slug must never reach tga as `repo: \"\"`"
        );

        // Whitespace around a real value is trimmed, not treated as content.
        let padded = SelectedRepo {
            name: "local/apex".to_owned(),
            path: PathBuf::from("repos/local/apex"),
            github_slug: Some("  acme/api  ".to_owned()),
            github_absent: None,
        };
        assert_eq!(padded.github_leg(), GithubLeg::Present("acme/api"));
    }

    /// The chain states its own board gaps, so a sweep handed a resolution says
    /// nothing about it — otherwise `taudit audit` prints each gap twice.
    ///
    /// Against `9ee9cc386` the `board_gaps` field does not exist.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_handed_its_boards_leaves_the_gaps_to_the_caller() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let config = board_config();
        let resolved = boards::resolve(
            &[
                registry::parse(None, "linear:a1b2c3d4-e5f6-7890-abcd-ef1234567890")
                    .expect("parses"),
            ],
            &config.boards,
        );
        assert_eq!(resolved.gaps.len(), 1, "{resolved:?}");

        let report = sweep_with_boards(
            &work,
            &config,
            &RunOptions::default(),
            &resolved,
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");
        assert!(report.board_gaps.is_empty(), "{:?}", report.board_gaps);
    }

    /// A stub that records the board variables it was handed.
    fn records_its_board_env() -> String {
        format!(
            "{}{}",
            writes_a_manifest(None).trim_end_matches("exit 0\n"),
            "{\n  echo \"jira=$TRUSTY_AUDIT_JIRA_TOKEN\"\n  \
             echo \"linear=$TRUSTY_AUDIT_LINEAR_API_KEY\"\n} > \"$out/board-env.txt\"\nexit 0\n",
        )
    }

    /// The `${…}` reference is worth nothing unless the variable is set, so this
    /// asserts on what the SPAWNED PROCESS received — not on what this crate
    /// computed. tga expands the reference on the far side.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_environment_carries_the_real_board_credentials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &records_its_board_env());
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);
        register_boards(&work, &["jira:ACME", "linear:ENG"]);

        let report = sweep(
            &work,
            &board_config(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let seen = std::fs::read_to_string(report.repos[0].output.join("board-env.txt"))
            .expect("the stub recorded its environment");
        assert!(seen.contains(&format!("jira={JIRA_TOKEN}")), "{seen}");
        assert!(seen.contains(&format!("linear={LINEAR_KEY}")), "{seen}");
    }

    /// An engagement that registers no board generates the document it always
    /// did, and exports no board variable — so the sections and the variables
    /// appear together or not at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_repo_only_engagement_gets_no_board_section_and_no_board_variable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &records_its_board_env());
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(
            &work,
            &board_config(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");

        let generated =
            std::fs::read_to_string(work.path(Area::State).join("tga-00-acme-api.yaml"))
                .expect("the generated tga config");
        assert!(!generated.contains("jira"), "{generated}");
        assert!(!generated.contains("linear"), "{generated}");
        let seen = std::fs::read_to_string(report.repos[0].output.join("board-env.txt"))
            .expect("the stub recorded its environment");
        assert!(seen.contains("jira=\n"), "{seen}");
        assert!(seen.contains("linear=\n"), "{seen}");
    }

    /// Why: #5869's filter is the OpenRouter test above, one credential over.
    /// [`child_output_scrubber`] builds its needles from
    /// `resolved_secret_values`, which enumerates the registered providers —
    /// openrouter, anthropic, github, slack, and no board. A board credential
    /// arrives from the engagement TOML rather than the environment, so before
    /// #5857 appended it nothing put it in the needle set and a child that
    /// quoted it wrote it to `work/logs/<repo>.log` in the clear.
    /// What: a child that echoes both board credentials back, on both streams,
    /// in the shapes a real one would — a provider rejection body quoting the
    /// token, and an auth failure quoting the key — leaves neither in the log
    /// nor in any other file the run wrote.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_echoes_a_board_credential_does_not_leave_it_in_the_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let mut script = String::from(
            "#!/bin/sh\n\
             echo \"jira 401: {\\\"message\\\":\\\"token \
             $TRUSTY_AUDIT_JIRA_TOKEN is not valid\\\"}\"\n\
             echo \"linear auth failed: key $TRUSTY_AUDIT_LINEAR_API_KEY rejected\" >&2\n",
        );
        script.push_str(writes_a_manifest(None).trim_start_matches("#!/bin/sh\n"));
        install_stubs(&work, &script);
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);
        register_boards(&work, &["jira:ACME", "linear:ENG"]);

        let report = sweep(
            &work,
            &board_config(),
            &RunOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let log = std::fs::read_to_string(&report.repos[0].log).expect("read the child log");
        assert!(
            !log.contains(JIRA_TOKEN),
            "the log carries the token:\n{log}"
        );
        assert!(!log.contains(LINEAR_KEY), "the log carries the key:\n{log}");
        // The child really did echo both — otherwise the assertions above prove
        // nothing about the filter.
        assert_eq!(
            log.matches("[REDACTED]").count(),
            2,
            "both echoes must be masked, one per stream:\n{log}"
        );
        // The log is still a log: masking replaced the credentials, not the
        // diagnosis.
        assert!(log.contains("jira 401"), "{log}");
        assert!(log.contains("linear auth failed"), "{log}");

        for path in files_under(work.root()) {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !text.contains(JIRA_TOKEN) && !text.contains(LINEAR_KEY),
                "{} carries a board credential",
                path.display()
            );
        }
    }

    /// #5980 CRITICAL 3 / MEDIUM 1: the `gh`-derived token is a THIRD
    /// credential source, alongside `resolved_secret_values` and
    /// `configured_secrets` — before `child_output_scrubber` took a
    /// `github_access` parameter, nothing put it in the needle set and a
    /// child that echoed a rejected GitHub credential wrote it to the log in
    /// the clear, the same shape #5857 already closed for `boards`.
    ///
    /// `Scrubber::scrub` itself is private to `crate::relay`, so this proves
    /// the needle set through `Scrubber`'s derived `Debug` rather than
    /// through a real spawned child — [`github_issues::GithubAccess::with_token`]
    /// exists for exactly this: constructing the token-present case without a
    /// real `gh` on `PATH`.
    #[test]
    fn child_output_scrubber_includes_the_github_token() {
        let access = github_issues::GithubAccess::with_token("ghp_test-token-in-needle-set");
        let scrubber = child_output_scrubber(&config(), &access);
        assert!(
            format!("{scrubber:?}").contains("ghp_test-token-in-needle-set"),
            "the github token must be in the scrubber's needle set: {scrubber:?}"
        );
    }

    /// The CRITICAL arm: `tga audit` exits 0 whenever the sweep COMPLETED,
    /// failed stages included, so a zero exit alone is not evidence anything was
    /// assessed. A child that wrote no manifest audited nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_exits_zero_having_written_nothing_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, "#!/bin/sh\nexit 0\n");
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllFailed, "{report:?}");
        let reason = match &report.repos[0].result {
            RepoResult::Failed { reason } => reason.clone(),
            other => panic!("a zero exit with no manifest must not succeed: {other:?}"),
        };
        assert!(reason.contains("wrote no manifest"), "{reason}");
    }

    /// The half of the same arm that exit code and file existence both miss:
    /// the manifest is there and says collection did not complete.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_manifest_reporting_a_failed_collect_stage_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(
            &work,
            &writes_a_manifest(Some(
                "Collection stage `collect` did not complete (401 Unauthorized) — the data \
                 it produces is not assessed in this report.",
            )),
        );
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllFailed, "{report:?}");
        let reason = match &report.repos[0].result {
            RepoResult::Failed { reason } => reason.clone(),
            other => panic!("a failed collect stage must not read as success: {other:?}"),
        };
        assert!(reason.contains("collection did not complete"), "{reason}");
    }

    /// And the other side of that line: DOC-67 §9 makes an unassessed optional
    /// dimension a named gap on a report still worth delivering. Failing on
    /// those would fail nearly every real engagement.
    #[cfg(unix)]
    #[tokio::test]
    async fn ordinary_gaps_do_not_fail_the_repository() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(
            &work,
            &writes_a_manifest(Some(
                "Collection stage `jira sync` did not complete (no JIRA project configured).",
            )),
        );
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");
        // #6081: grounding adds its own line for a stub sweep, so what is
        // asserted is that tga's stated gap survives — not the total count.
        assert!(
            report.repos[0].gaps.iter().any(|g| g.contains("jira sync")),
            "the gap must be surfaced: {:?}",
            report.repos[0].gaps
        );
    }

    /// 🔴 #6081: a child that failed AFTER writing its manifest must still be
    /// grounded, and must still say so.
    ///
    /// Why: the dogfood sweep of 2026-08-19. `tga audit` wrote the manifest,
    /// then its render stage failed on a truncated LLM response and the child
    /// exited 1. The call site asked the child's EXIT STATUS whether there was a
    /// manifest to ground, so grounding never ran — and because it never ran, it
    /// stated no gap either. No `inspect_priority`, no gap, no log line: the one
    /// combination `crate::grounding`'s "degrade, never silently" contract says
    /// cannot happen. The manifest survives that failure and re-rendering it is
    /// the documented recovery, so it is the file, not the status, that decides.
    /// What: a stub that writes the manifest and then exits 1. The repository
    /// still fails, and its manifest still carries a ranking or a named gap.
    /// Test: this is the test. Against `origin/main` the final assertion fails —
    /// `gaps` is empty and the manifest declares no `inspect_priority`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_fails_after_writing_a_manifest_is_still_grounded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest_then_fails());
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("a failing child is a recorded failure, not an error");

        assert_eq!(report.status, RunStatus::AllFailed, "{report:?}");
        let manifest = std::fs::read_to_string(report.repos[0].output.join("manifest.toml"))
            .expect("the manifest the child wrote survives its failure");
        assert!(
            !report.repos[0].gaps.is_empty() || manifest.contains("inspect_priority"),
            "grounding must leave a ranking or a named gap on a manifest that outlived its \
             child, never neither: gaps={:?}\n{manifest}",
            report.repos[0].gaps
        );
    }

    /// A hung child must cost its repository, not the whole run — and the
    /// progress record must still be written.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hung_child_is_killed_and_recorded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, "#!/bin/sh\nsleep 600\n");
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep_with_budget(
            &work,
            &config(),
            &RunOptions::default(),
            None,
            std::time::Duration::from_millis(200),
            &Progress::none(),
        )
        .await
        .expect("the sweep completes rather than hanging");
        assert_eq!(report.status, RunStatus::AllFailed);
        let reason = match &report.repos[0].result {
            RepoResult::Failed { reason } => reason.clone(),
            other => panic!("a hung child must not succeed: {other:?}"),
        };
        assert!(reason.contains("timed out"), "{reason}");
        assert!(
            read_progress(&work).expect("record reads").is_some(),
            "an unattended run must leave a record of how far it got"
        );
    }

    /// Every path this run writes stays inside the root that `rm -rf` cleans.
    #[cfg(unix)]
    #[tokio::test]
    async fn everything_the_run_writes_is_inside_the_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, "#!/bin/sh\nexit 0\n");
        make_repo(&work, "acme-api");
        select(&work, &[("../../escape", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        for run in &report.repos {
            assert!(run.output.starts_with(work.root()), "{:?}", run.output);
            assert!(run.log.starts_with(work.root()), "{:?}", run.log);
        }
        assert!(progress_path(&work).starts_with(work.root()));
    }

    // ── #5671: what the spawned child's environment actually carries ─────────
    //
    // These go through the real `Command`, so they cover the wiring between
    // `inference_env` and the child, not just the resolution rule. The operator
    // environment is INJECTED rather than exported: `std::env::set_var` is
    // `unsafe` in edition 2024 and races every other thread in this test binary,
    // and `serial_test` is not a dev-dependency of this crate. Injection keeps
    // the assertions deterministic while still exercising the real spawn.

    /// A stub `tga` that writes the manifest and then records the inference
    /// variables it was handed, so a test can read back the child's own view.
    fn records_its_inference_env() -> String {
        format!(
            "{}{}",
            writes_a_manifest(None).trim_end_matches("exit 0\n"),
            "{\n  echo \"provider=$TRUSTY_REVIEW_PROVIDER\"\n  \
             echo \"reviewer=$TRUSTY_REVIEW_REVIEWER_MODEL\"\n  \
             echo \"verifier=$TRUSTY_REVIEW_VERIFIER_MODEL\"\n  \
             echo \"summarizer=$TRUSTY_REVIEW_SUMMARIZER_MODEL\"\n  \
             echo \"key=$OPENROUTER_API_KEY\"\n} > \"$out/env.txt\"\nexit 0\n",
        )
    }

    /// One repository, stubs installed, ready to sweep.
    fn one_repo_ready(work: &WorkDir) {
        install_stubs(work, &records_its_inference_env());
        make_repo(work, "acme-api");
        select(work, &[("acme-api", "repos/acme-api")]);
    }

    /// The `env.txt` the stub wrote, i.e. the child's own environment.
    fn child_env(report: &RunReport) -> String {
        std::fs::read_to_string(report.repos[0].output.join("env.txt"))
            .expect("the stub recorded its environment")
    }

    async fn sweep_with_operator<F>(work: &WorkDir, operator: F) -> Result<RunReport, AuditError>
    where
        F: Fn(&str) -> Option<String>,
    {
        sweep_with_env(
            work,
            &config(),
            &RunOptions::default(),
            None,
            PER_REPO_TIMEOUT,
            &Progress::none(),
            operator,
        )
        .await
    }

    /// #5671: the child must carry the provider AND all three model ids, not
    /// just the credential. Asserts on the environment the spawned process
    /// actually received, not on the value this crate computed.
    ///
    /// Against `origin/main` every assertion below fails: `spawn_tga` set only
    /// the credential and the two binary paths, so `trusty-review` resolved
    /// `Provider::Bedrock`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_environment_selects_openrouter_and_all_three_models() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        one_repo_ready(&work);

        let report = sweep_with_operator(&work, |_| None)
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let dumped = child_env(&report);
        for expected in [
            format!("provider={}", inference::PROVIDER_OPENROUTER),
            format!("reviewer={}", inference::DEFAULT_REVIEWER_MODEL),
            format!("verifier={}", inference::DEFAULT_VERIFIER_MODEL),
            format!("summarizer={}", inference::DEFAULT_SUMMARIZER_MODEL),
            // #5663's credential must still be there — this widens that, not replaces it.
            "key=sk-or-v1-not-a-real-key".to_owned(),
        ] {
            assert!(
                dumped.contains(&expected),
                "the child environment is missing `{expected}`:\n{dumped}"
            );
        }
    }

    /// An operator who named the whole selection keeps it: this crate writes
    /// none of the four onto the child, so nothing of ours can contradict
    /// theirs. The injected lookup reports all four set without exporting them,
    /// so an emitted default would show up here as a non-empty value.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_fully_set_operator_environment_is_left_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        one_repo_ready(&work);

        let report = sweep_with_operator(&work, |_| Some("operator".to_owned()))
            .await
            .expect("a whole operator selection resolves");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let dumped = child_env(&report);
        for role in ["provider", "reviewer", "verifier", "summarizer"] {
            assert!(
                dumped.contains(&format!("{role}=\n")),
                "this crate overrode the operator's `{role}`:\n{dumped}"
            );
        }
        // The credential is not part of the selection and is still delivered.
        assert!(dumped.contains("key=sk-or-v1-not-a-real-key"), "{dumped}");
    }

    /// The HIGH finding, end to end: an operator on Bedrock who exports only
    /// `TRUSTY_REVIEW_PROVIDER` must not have OpenRouter slugs written under it.
    /// The sweep refuses, and refuses BEFORE spawning — the stub never runs, so
    /// there is no `env.txt` and no partly-audited repository.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_partial_operator_environment_refuses_before_any_child_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        one_repo_ready(&work);

        let err = sweep_with_operator(&work, |name| {
            (name == inference::ENV_PROVIDER).then(|| "bedrock".to_owned())
        })
        .await
        .expect_err("a provider without models must not be completed by guesswork");

        let AuditError::SplitInferenceSelection { set, missing, .. } = &err else {
            panic!("expected SplitInferenceSelection, got {err:?}");
        };
        assert_eq!(set, inference::ENV_PROVIDER);
        assert!(
            missing.contains("TRUSTY_REVIEW_REVIEWER_MODEL"),
            "{missing}"
        );

        // Nothing ran: no output directory, so no child was spawned.
        let outputs = work.path(Area::Output);
        let spawned = std::fs::read_dir(&outputs)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(spawned, 0, "a refused selection must not spawn any child");
    }
    /// A stub `tga` that relays the stage lines it is given, then optionally
    /// writes the manifest a real one would.
    ///
    /// It emits ONLY when `TRUSTY_PROGRESS_RELAY` is set, which is what proves
    /// the sweep asks for the relay rather than the child volunteering it.
    fn relays_stages(events: &[StageEvent], exit: i32, manifest: bool) -> String {
        let emits: String = events
            .iter()
            .map(|e| format!("  printf '%s\\n' '{}' >&2\n", e.encode()))
            .collect();
        let write = if manifest {
            "printf '[report]\\ntitle = \"Acme\"\\n\\n[[repositories]]\\nname = \"acme\"\\n\
             path = \"/r\"\\n' > \"$out/manifest.toml\"\n"
        } else {
            ""
        };
        format!(
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             mkdir -p \"$out\"\n\
             echo 'INFO tga starting' >&2\n\
             if [ -n \"$TRUSTY_PROGRESS_RELAY\" ]; then\n{emits}fi\n\
             {write}exit {exit}\n"
        )
    }

    /// Why (#5823): the whole point of the ticket. A sweep spends up to four
    /// hours inside one child, and until now every stage that child reported
    /// went into a log file nobody was reading. This proves the events reach
    /// the front end's sink — driven by a synthetic child, not a real sweep.
    ///
    /// It also proves the two things that must NOT change: the log still holds
    /// the child's whole output, relayed lines included, and the child only
    /// speaks when asked (the stub emits nothing unless the sweep sets
    /// `TRUSTY_PROGRESS_RELAY`).
    /// What: a stub emitting three stage events is swept with a recording sink.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_childs_stage_events_reach_the_progress_sink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let events = vec![
            StageEvent::new("Audit", "collect", StageState::Started)
                .with_counts(0, Some(9))
                .with_detail("stage 1 of 9"),
            StageEvent::new("Collect", "acme-api", StageState::Advanced).with_counts(12, Some(40)),
            StageEvent::new("Audit", "report", StageState::Completed).with_counts(1, Some(1)),
        ];
        install_stubs(&work, &relays_stages(&events, 0, true));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let (recorder, progress) = Recorder::new();
        let report = sweep(&work, &config(), &RunOptions::default(), &progress)
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded);

        assert_eq!(
            recorder.stages(),
            events,
            "every stage the child reported must reach the sink"
        );
        let updates = recorder.updates();
        assert!(
            matches!(
                updates.first(),
                Some(ProgressUpdate::OperationStarted {
                    operation: Operation::Sweep,
                    total: 1
                })
            ),
            "{updates:?}"
        );
        assert!(
            updates.iter().any(|u| matches!(
                u,
                ProgressUpdate::UnitFinished { target, outcome: UnitOutcome::Succeeded, .. }
                    if target == "acme-api"
            )),
            "{updates:?}"
        );
        assert!(
            matches!(
                updates.last(),
                Some(ProgressUpdate::OperationFinished {
                    succeeded: 1,
                    total: 1,
                    ..
                })
            ),
            "{updates:?}"
        );

        // The log is not a casualty of the relay: it still holds everything.
        let log = std::fs::read_to_string(&report.repos[0].log).expect("the log was written");
        assert!(log.contains("INFO tga starting"), "{log}");
        for event in &events {
            assert!(log.contains(&event.encode()), "{log}");
        }
    }

    /// Why (#5823): a child killed or crashed mid-stage is the case that wedges
    /// a display — the last thing it said was "collect started", and nothing
    /// ever contradicts it. The verdict must still arrive, and the underlying
    /// failure must not be swallowed by the display path.
    /// What: a stub that announces a stage and then exits 3 produces the started
    /// stage, a `Failed` unit verdict naming the exit code, and an `AllFailed`
    /// report with its log intact.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_dies_mid_stage_still_reports_its_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let started = StageEvent::new("Audit", "classify", StageState::Started)
            .with_counts(0, Some(9))
            .with_detail("stage 3 of 9");
        install_stubs(
            &work,
            &relays_stages(std::slice::from_ref(&started), 3, false),
        );
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let (recorder, progress) = Recorder::new();
        let report = sweep(&work, &config(), &RunOptions::default(), &progress)
            .await
            .expect("a failing child is a recorded failure, not an error");
        assert_eq!(report.status, RunStatus::AllFailed);

        assert_eq!(recorder.stages(), vec![started.clone()]);
        let verdict = recorder
            .updates()
            .into_iter()
            .find_map(|u| match u {
                ProgressUpdate::UnitFinished { outcome, .. } => Some(outcome),
                _ => None,
            })
            .expect("the unit must be closed even though the child died inside it");
        let UnitOutcome::Failed(reason) = verdict else {
            panic!("expected a failure, got {verdict:?}");
        };
        assert!(reason.contains("code 3"), "{reason}");

        // The display never becomes the only record.
        let log = std::fs::read_to_string(&report.repos[0].log).expect("the log survives");
        assert!(log.contains(&started.encode()), "{log}");
    }

    // ── #5494: the incremental checkpoint, and resuming against it ───────────

    /// A stub `tga` that appends its `--output` to [`INVOCATION_LOG`] under the
    /// work-dir root, then writes the manifest a real one would. It exits 3 for
    /// any repository whose output path ends with `fails`, so a test can make
    /// one repository of several fail without a second script.
    ///
    /// The invocation log is what "was this repository re-collected" is read
    /// from: the report says what the sweep CLAIMS, and this says what ran.
    ///
    /// #6293: every path this script touches is ABSOLUTE, interpolated from
    /// `work` when the script is generated. It used to write `invocations.txt`
    /// and read `state/run-progress.toml` relative to the child's own working
    /// directory, which is the work root for exactly one caller —
    /// [`child::spawn_tga`], the only spawner in this crate that sets one.
    /// `install_stubs` puts this same script behind `tga`, `trusty-analyze` AND
    /// `trusty-review`, and `index_report::tool_version`, `approve` and
    /// `grounding::index` all spawn with the parent's working directory, so
    /// those runs appended to whatever directory the test binary started in.
    /// Running the suite from the crate directory put the log in the checkout,
    /// and 49 blank lines of it were committed by accident (`f3ec62545`).
    ///
    /// A child handed no `--output` exits 0 having recorded nothing: the log
    /// holds the `--output` paths an audit child was given, and a `--version`
    /// probe is not one of them. Before #6293 such a run appended a blank line
    /// and then failed its way through `mkdir -p ""` and a write to
    /// `/manifest.toml`, which is what the 49 committed blank lines were.
    /// Test: `the_invocation_log_lands_in_the_work_root_whatever_the_child_cwd_is`.
    fn counts_invocations(work: &WorkDir, fails: &str) -> String {
        let log = work.root().join(INVOCATION_LOG);
        let progress = progress_path(work);
        format!(
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             if [ -z \"$out\" ]; then exit 0; fi\n\
             echo \"$out\" >> '{log}'\n\
             if [ -f '{progress}' ]; then \
             cp '{progress}' \"$out.seen.toml\"; fi\n\
             mkdir -p \"$out\"\n\
             case \"$out\" in *{fails}) exit 3;; esac\n\
             printf '[report]\\ntitle = \"Acme\"\\n\\n[[repositories]]\\n\
             name = \"acme\"\\npath = \"/r\"\\n' > \"$out/manifest.toml\"\nexit 0\n",
            log = log.display(),
            progress = progress.display(),
        )
    }

    /// A pattern `counts_invocations` can never match, for a stub that always
    /// succeeds.
    const NEVER: &str = "--no-such-repository--";

    /// The file under the work root that [`counts_invocations`] records into.
    const INVOCATION_LOG: &str = "invocations.txt";

    /// Every `--output` a stub child was handed, in the order they ran.
    ///
    /// #6293: an absent log is a panic, not an empty vector. It used to be read
    /// with `unwrap_or_default()`, so a log the stub had written somewhere else
    /// came back empty and every `assert_eq!(invocations(&work).len(), n)`
    /// compared zero against a number it never observed. A test that cannot see
    /// what ran has to fail.
    /// Test: `a_missing_invocation_log_is_a_failure_not_an_empty_count`.
    fn invocations(work: &WorkDir) -> Vec<String> {
        let path = work.root().join(INVOCATION_LOG);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("no stub invocation log at {}: {e}", path.display()))
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Why (#6293): the stub wrote its invocation log to a RELATIVE path, so it
    /// landed wherever the child happened to be started. Only
    /// [`child::spawn_tga`] sets a working directory; `install_stubs` installs
    /// this same script behind `trusty-analyze` and `trusty-review` too, and
    /// every other spawner in this crate inherits the parent's. That is how the
    /// log ended up in the checkout, and how 49 blank lines of it were
    /// committed (`f3ec62545`).
    /// What: run the generated script from a directory that is NOT the work
    /// root. The log must land under the root, and that directory must stay
    /// empty. A child given no `--output` records nothing at all.
    /// Test: this is the test.
    #[cfg(unix)]
    #[test]
    fn the_invocation_log_lands_in_the_work_root_whatever_the_child_cwd_is() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        std::fs::create_dir_all(work.root()).expect("mkdir root");
        let elsewhere = tempfile::tempdir().expect("tempdir");

        let script = work.root().join("stub.sh");
        std::fs::write(&script, counts_invocations(&work, NEVER)).expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let output = work.path(Area::Output).join("00-acme-api");
        let audit = std::process::Command::new(&script)
            .arg("--output")
            .arg(&output)
            .current_dir(elsewhere.path())
            .status()
            .expect("the stub runs");
        assert!(audit.success(), "{audit:?}");

        // The probe shape that produced the blank lines: no `--output` at all.
        // Its exit status is not the subject — what it RECORDS is.
        let _ = std::process::Command::new(&script)
            .arg("--version")
            .current_dir(elsewhere.path())
            .status()
            .expect("the stub runs");

        assert!(
            !elsewhere.path().join(INVOCATION_LOG).exists(),
            "the stub wrote its log into the directory it was started in, not the work root"
        );
        assert_eq!(
            invocations(&work),
            vec![output.display().to_string()],
            "the log must hold the one `--output` an audit child was handed, and nothing for \
             the version probe"
        );
    }

    /// Why (#6293): `invocations()` read the log with `unwrap_or_default()`, so
    /// a log written anywhere else came back as an empty vector rather than an
    /// error. Every `assert_eq!(invocations(&work).len(), n)` then compared
    /// zero against a number nothing had produced — the assertion passes only
    /// because `n` happens to be what the sweep also failed to record. A test
    /// that cannot read what ran must fail.
    /// What: read the log of a work directory no stub has ever written into.
    /// Test: this is the test.
    #[test]
    #[should_panic(expected = "no stub invocation log at")]
    fn a_missing_invocation_log_is_a_failure_not_an_empty_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let _ = invocations(&work);
    }

    fn two_repos_ready(work: &WorkDir, fails: &str) {
        install_stubs(work, &counts_invocations(work, fails));
        make_repo(work, "acme-api");
        make_repo(work, "acme-web");
        select(
            work,
            &[
                ("acme-api", "repos/acme-api"),
                ("acme-web", "repos/acme-web"),
            ],
        );
    }

    /// Why (#5494): the whole ticket. The record used to be written once, after
    /// every child had finished, so a crash mid-sweep left nothing at all. This
    /// proves the record advances WITH the work — the second child reads a
    /// checkpoint that already names the first — and that it stays marked
    /// incomplete until the sweep reaches the end of its selection, which is
    /// what stops a crashed run being packaged as a whole engagement.
    /// What: two repositories, a stub that copies the record it finds.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_checkpoint_advances_with_the_sweep_and_completes_only_at_the_end() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        two_repos_ready(&work, NEVER);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        // What the SECOND child saw when it started.
        let seen = std::fs::read_to_string(
            work.path(Area::Output)
                .join("01-acme-web.seen.toml")
                .as_path(),
        )
        .expect("the second child found a checkpoint the first one's completion wrote");
        let mid: RunProgress = toml::from_str(&seen).expect("the checkpoint parses");
        assert_eq!(mid.repos.len(), 1, "{mid:?}");
        assert_eq!(mid.repos[0].repo.name, "acme-api");
        assert!(
            !mid.complete,
            "a checkpoint written mid-sweep must not claim the sweep finished"
        );

        // And the first child found nothing, so the checkpoint is this run's.
        assert!(
            !work
                .path(Area::Output)
                .join("00-acme-api.seen.toml")
                .exists()
        );

        let final_record = read_progress(&work).expect("reads").expect("present");
        assert!(final_record.complete);
        assert_eq!(final_record.repos.len(), 2);
    }

    /// Why (#5494, the fail-open check): recording progress is now a branch
    /// that runs after every repository, and the tempting shape is to warn and
    /// carry on. That would spend hours auditing repositories nothing on disk
    /// records, and report them as audited. So the write is a refusal: the
    /// sweep stops at the first repository whose completion it could not
    /// record, and never returns a report claiming that repository succeeded.
    ///
    /// Downgrade that write to a warning and both children run, the sweep
    /// returns `Ok(AllSucceeded)`, and two repositories are reported as audited
    /// by a run nothing on disk describes. The invocation count is what
    /// separates the two behaviours.
    /// What: a non-empty directory at the record's path, which no rename can
    /// replace. The run is `fresh`, so the plan never READS that path — the
    /// only failure under test is the write.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_checkpoint_that_cannot_be_written_stops_the_sweep() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        two_repos_ready(&work, NEVER);
        let blocked = progress_path(&work);
        std::fs::create_dir_all(&blocked).expect("mkdir");
        std::fs::write(blocked.join("occupied"), b"x").expect("write");

        let (recorder, progress) = Recorder::new();
        let err = sweep(&work, &config(), &RunOptions { fresh: true }, &progress)
            .await
            .expect_err("a run whose progress cannot be recorded must not report success");
        assert!(matches!(err, AuditError::WorkDir { .. }), "{err:?}");

        let ran = invocations(&work);
        assert_eq!(
            ran.len(),
            1,
            "the sweep must stop at the repository whose completion could not be \
             recorded, not audit the rest against a record it cannot write: {ran:?}"
        );
        assert!(
            !recorder.updates().iter().any(|u| matches!(
                u,
                ProgressUpdate::OperationFinished { succeeded, .. } if *succeeded > 0
            )),
            "no repository may be reported as audited: {:?}",
            recorder.updates()
        );
    }

    /// Why (#5494): the point of the checkpoint. A re-run must not spend four
    /// hours re-auditing a repository an earlier run finished — and must retry
    /// one it recorded as failed, because a failure is usually the transient
    /// thing the operator re-ran to clear, and skipping it would make the
    /// re-run a no-op that reports the same failure forever.
    /// What: two repositories, the second failing; the second sweep runs one
    /// child. The skip is announced, not silent.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_re_run_skips_what_succeeded_and_retries_what_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        two_repos_ready(&work, "acme-web");

        let first = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(first.status, RunStatus::Partial, "{first:?}");
        assert_eq!(invocations(&work).len(), 2);
        assert_eq!(first.resumed().count(), 0);

        let (recorder, progress) = Recorder::new();
        let second = sweep(&work, &config(), &RunOptions::default(), &progress)
            .await
            .expect("the sweep completes");
        assert_eq!(second.status, RunStatus::Partial, "{second:?}");

        let ran = invocations(&work);
        assert_eq!(
            ran.len(),
            3,
            "only the failed repository may run again: {ran:?}"
        );
        assert!(ran[2].ends_with("01-acme-web"), "{ran:?}");

        assert!(second.repos[0].resumed, "{:?}", second.repos[0]);
        assert!(!second.repos[1].resumed, "{:?}", second.repos[1]);
        assert_eq!(second.resumed().count(), 1);

        // Silent skipping is the defect: the operator has to be told.
        let skipped = recorder
            .updates()
            .into_iter()
            .find_map(|u| match u {
                ProgressUpdate::UnitFinished {
                    target,
                    outcome: UnitOutcome::Skipped(why),
                    ..
                } if target == "acme-api" => Some(why),
                _ => None,
            })
            .expect("the carried-over repository must be announced");
        assert!(skipped.contains("already audited"), "{skipped}");
    }

    /// Why (#5494): a sweep that ends early must not erase what it had already
    /// decided to carry over. The checkpoint is republished whole after every
    /// repository, so building it from only the repositories the loop has
    /// VISITED drops the ones further down the selection — repositories an
    /// earlier run audited, whose output is on disk and verified, and which the
    /// next run would then re-collect from scratch. That is the four-hours-lost
    /// failure this ticket exists to prevent, reappearing one selection later.
    /// What: four repositories. `a` and `d` are re-collected (their outputs are
    /// gone), `b` and `c` carry over. `d`'s output path is occupied by a regular
    /// file, so its `mkdir` fails and the sweep ends after `a` — which is a
    /// crash's shape: the record on disk is whatever the last checkpoint wrote.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_that_ends_early_keeps_the_entries_it_was_carrying_over() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &counts_invocations(&work, NEVER));
        for name in ["a", "d", "b", "c"] {
            make_repo(&work, name);
        }
        select(
            &work,
            &[
                ("a", "repos/a"),
                ("d", "repos/d"),
                ("b", "repos/b"),
                ("c", "repos/c"),
            ],
        );

        let first = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(first.status, RunStatus::AllSucceeded, "{first:?}");

        // `a` re-collects because its output is gone. `d` re-collects for the
        // same reason AND cannot be re-collected: a regular file where its
        // output directory belongs fails `mkdir`, so the sweep ends there.
        std::fs::remove_dir_all(work.path(Area::Output).join("00-a")).expect("drop a's output");
        std::fs::remove_dir_all(work.path(Area::Output).join("01-d")).expect("drop d's output");
        std::fs::write(work.path(Area::Output).join("01-d"), b"not a directory").expect("occupy");

        let err = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect_err("a repository whose output directory cannot be made ends the sweep");
        assert!(matches!(err, AuditError::WorkDir { .. }), "{err:?}");

        let record = read_progress(&work).expect("reads").expect("present");
        assert!(!record.complete, "{record:?}");
        let names: Vec<&str> = record.repos.iter().map(|r| r.repo.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "b", "c"],
            "the sweep re-collected `a` and died on `d`; `b` and `c` were already \
             audited and carried over, and dropping them costs the next run their \
             collection time all over again"
        );
        assert!(
            record.repos[1].resumed && record.repos[2].resumed,
            "{record:?}"
        );
    }

    /// Why (#5494): resume trusts a record about files it does not re-read, so
    /// the one thing it must never do is report a repository as complete when
    /// its data is gone. The record still says `Succeeded`; the disk decides.
    /// What: a successful sweep, its output deleted, then a re-run — which
    /// audits it again and says why.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_deleted_output_is_re_audited_rather_than_reported_complete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &counts_invocations(&work, NEVER));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let first = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(first.status, RunStatus::AllSucceeded);
        std::fs::remove_dir_all(&first.repos[0].output).expect("delete the output");

        let (recorder, progress) = Recorder::new();
        let second = sweep(&work, &config(), &RunOptions::default(), &progress)
            .await
            .expect("the sweep completes");
        assert_eq!(invocations(&work).len(), 2, "the repository must run again");
        assert!(!second.repos[0].resumed, "{:?}", second.repos[0]);
        assert!(second.repos[0].output.join("manifest.toml").is_file());

        let stated = recorder
            .stages()
            .into_iter()
            .find_map(|s| s.detail)
            .expect("the re-collection must state its reason");
        assert!(stated.contains("no longer usable"), "{stated}");
    }

    /// Why (#5494): a checkpoint entry is matched to the CURRENT selection, not
    /// merely to a repository name. `stem` carries the selection index, so a
    /// reordered selection means a different `out/<stem>/` — and reusing the
    /// old entry would report a repository as audited into a directory this run
    /// never wrote.
    /// What: one repository audited, then a selection with another ahead of it.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_reordered_selection_does_not_reuse_the_wrong_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        two_repos_ready(&work, NEVER);
        select(&work, &[("acme-api", "repos/acme-api")]);

        sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(invocations(&work).len(), 1);

        select(
            &work,
            &[
                ("acme-web", "repos/acme-web"),
                ("acme-api", "repos/acme-api"),
            ],
        );
        let second = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(
            invocations(&work).len(),
            3,
            "a repository at a new position writes a new output and must be re-audited"
        );
        assert_eq!(second.resumed().count(), 0, "{second:?}");
    }

    /// Why (#5494): the operator's override. A recipient who re-cloned their
    /// repositories has outputs that verify fine and describe source that has
    /// moved on, and no automatic check can see that.
    /// What: `--fresh` re-audits a repository the record says was audited.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_fresh_run_re_audits_everything() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &counts_invocations(&work, NEVER));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(invocations(&work).len(), 1);

        let report = sweep(
            &work,
            &config(),
            &RunOptions { fresh: true },
            &Progress::none(),
        )
        .await
        .expect("the sweep completes");
        assert_eq!(
            invocations(&work).len(),
            2,
            "`--fresh` must run the child again"
        );
        assert_eq!(report.resumed().count(), 0);
    }

    /// Why (#5823): piping the child's streams to read them is the change most
    /// able to break something unrelated — a sweep that no longer works is a
    /// worse outcome than one with no display. This is the no-sink path, which
    /// is what `Session` uses unless a front end supplies one.
    /// What: a sweep with [`Progress::none`] still succeeds and still logs.
    /// Test: this is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_without_a_sink_is_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &writes_a_manifest(None));
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config(), &RunOptions::default(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded);
        assert!(report.repos[0].log.is_file(), "the log is still written");
    }
}
