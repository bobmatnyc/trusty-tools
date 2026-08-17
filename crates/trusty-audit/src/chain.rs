//! The one-shot audit: register, then run one command instead of four.
//!
//! Why: #5824. An operator who receives a handoff package and registers what to
//! audit still had to type `install`, `clone`, `run` and `package` in the right
//! order, and to know what each one had left behind before the next would work.
//! `Command::Guided` walked the same steps but only REPORTED the next one; it
//! never drove past it. This module drives them.
//!
//! What: [`audit`], which chains four phases over one working directory —
//! install the pinned tools, materialize the registered targets, collect and
//! analyze each one, assemble the return package — and [`ChainReport`], what
//! each phase produced. Every phase is an existing capability called unchanged:
//! nothing here re-implements installing, cloning, sweeping or packaging, so the
//! four verbs and this one cannot drift apart.
//!
//! ## Where the registry finally reaches the sweep
//!
//! #5822 added `state/audit-targets.toml` and nothing read it. The sweep reads
//! `state/selected-repos.toml`, which only `crate::clone` writes. So a
//! registered repository was invisible to `run` until someone cloned it by hand
//! with the same name. The materialize phase closes that: it reads the registry
//! and hands the repository targets to [`crate::clone::clone_all`], which
//! records the usable checkouts as the selection. A registered repository now
//! reaches the sweep because it was registered.
//!
//! A registered BOARD does not, and is reported as a gap rather than dropped —
//! see [`board_gap`] for what passing one through would cost today.
//!
//! ## Partial success is the normal case, and it is not success
//!
//! With six repositories, one failing is ordinary. The chain CONTINUES past it,
//! because stopping would discard five audits over one failure and because that
//! is already what `clone` and `run` do on their own (DOC-68 §14 Q2, DOC-67 §9).
//! What it must never do is let a partial run read as a whole one, so three
//! things hold at once:
//!
//! - A sweep in which NOTHING was audited stops the chain in
//!   [`Phase::Collect`]. There is no package, because a package over zero
//!   audited repositories is a zip of two generated files that looks like a
//!   deliverable.
//! - A sweep in which SOME repository failed still packages, and
//!   [`crate::package::assemble`] names every repository it does not cover in
//!   the package's own README and metadata.
//! - Either way [`crate::session::Outcome::exit_code`] is non-zero, so
//!   `taudit audit && send-it` cannot chain onward over an incomplete
//!   engagement.
//!
//! ## Attribution, and resume
//!
//! Every phase failure arrives as [`AuditError::ChainStopped`], naming the
//! phase; the wrapped error names the target and the reason. And because each
//! phase is individually re-entrant — `tools::ensure` is a no-op when the pins
//! are satisfied, `clone_all` reuses a complete checkout, and the sweep resumes
//! from `crate::run`'s per-repository checkpoint (#5494) — re-running the chain
//! after an interruption continues rather than restarts.
//!
//! Test: `super::chain_tests`.

use std::fmt;
use std::path::PathBuf;

use crate::clone::{self, CloneOptions, CloneReport};
use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::package::{self, ReturnPackage};
use crate::progress::{Operation, Progress, UnitOutcome};
use crate::registry::{Registry, Target};
use crate::run::{self, RunOptions, RunReport, RunStatus};
use crate::tools::{self, InstalledTool};
use crate::workdir::WorkDir;

/// Which link of the chain a failure came from.
///
/// Why: #5824's second requirement. Four fallible phases collapsed into one
/// error is a message an operator cannot act on — "cannot read
/// /w/state/audit-targets.toml" and "`tga audit` exited with code 2" call for
/// completely different next steps, and a chain that reports both as "the audit
/// failed" makes the operator re-derive which stage they are in.
/// What: one variant per phase, carried on [`AuditError::ChainStopped`]. The
/// wrapped error names the target and the reason; this names the stage.
/// Test: `super::chain_tests::a_sweep_that_audited_nothing_stops_before_packaging`,
/// `super::chain_tests::an_engagement_with_nothing_to_audit_stops_in_materialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phase {
    /// Downloading and verifying the engagement's pinned tool set (#5495).
    InstallTools,
    /// Turning registered targets into checkouts the sweep can read (#5215).
    Materialize,
    /// Running `tga audit` over each of them (#5555).
    Collect,
    /// Assembling the deliverable to send back (#5499).
    Package,
}

impl Phase {
    /// The name this phase is reported under, e.g. `"collect"`.
    pub fn label(self) -> &'static str {
        match self {
            Phase::InstallTools => "install",
            Phase::Materialize => "materialize",
            Phase::Collect => "collect",
            Phase::Package => "package",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// How to run the chain.
///
/// Why: a struct for the same reason [`RunOptions`] is one — these are
/// operator-facing knobs reaching the crate through
/// [`crate::session::Command::Audit`], and they will grow neighbours.
/// What: the sweep's `--fresh`, forwarded unchanged, and the package
/// destination, which is the one path this client writes outside the working
/// directory. [`ChainOptions::default`] resumes and writes the default
/// destination.
/// Test: `crate::cli::cli_tests::the_one_shot_audit_resumes_by_default`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChainOptions {
    /// Audit every selected repository again, ignoring recorded progress.
    pub fresh: bool,
    /// Where the return package lands, or `None` for the default.
    pub destination: Option<PathBuf>,
}

/// What one chained run did, phase by phase.
///
/// Why: structured rather than text, like every other
/// [`crate::session::Outcome`] — the CLI renders it and the Tauri shell will
/// render the same values as a window. Keeping each phase's own report intact
/// rather than flattening them to a verdict is what lets the front end show
/// which repository failed at which stage.
/// What: one field per phase, plus the gaps the chain itself states.
/// Test: `super::chain_tests::the_chain_installs_collects_and_packages`.
#[derive(Debug)]
#[non_exhaustive]
pub struct ChainReport {
    /// What the install phase placed, or `None` when the pins were satisfied
    /// already or `--no-install` declined to reach the network.
    pub installed: Option<Vec<InstalledTool>>,
    /// What the materialize phase acquired, or `None` when the registry named
    /// no repository and an earlier `clone` had already left a selection.
    pub acquired: Option<CloneReport>,
    /// Per-repository results from the sweep.
    pub run: RunReport,
    /// The deliverable, and what it does not cover.
    pub package: ReturnPackage,
    /// What this engagement targets and the chain could not audit.
    ///
    /// Distinct from [`ReturnPackage::excluded`], which names repositories the
    /// sweep ATTEMPTED and failed. A gap here is something never attempted at
    /// all — a registered board today. Non-empty makes the run's exit status
    /// non-zero, so a silently unaudited target cannot read as a whole
    /// engagement.
    pub gaps: Vec<String>,
}

/// Run the whole engagement: install, materialize, collect, package.
///
/// Why: #5824 — the operator registers what to audit and then runs one command.
/// See the module docs for the partial-success policy and what each phase is.
///
/// # Preconditions
/// The engagement config is loaded (the caller owns that, as every other
/// capability's caller does). Either the registry names at least one repository
/// or an earlier `clone` left a selection; both absent is a refusal, not an
/// empty run.
///
/// # Postconditions
/// On `Ok`, every phase completed, at least one repository was audited, and
/// [`ChainReport::package`] is a written file. On `Err`, the error is
/// [`AuditError::ChainStopped`] naming the phase, and nothing claims a
/// repository was audited that was not — in particular no package exists over a
/// sweep that audited nothing.
///
/// What: four phases, each an existing capability called unchanged, each
/// failure wrapped with its phase.
/// Test: `super::chain_tests`.
///
/// # Errors
///
/// [`AuditError::ChainStopped`] wrapping whichever phase refused — see
/// [`Phase`]. The credential reaches `tga audit` exactly as
/// [`crate::run::sweep`] already sends it, through the child's environment;
/// nothing here opens a second path to it.
pub async fn audit(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &ChainOptions,
    auto_install: bool,
    progress: &Progress,
) -> Result<ChainReport, AuditError> {
    let installed = install(work, config, auto_install, progress)
        .await
        .map_err(|e| stopped(Phase::InstallTools, e))?;

    let (repos, gaps) = split_targets(work).map_err(|e| stopped(Phase::Materialize, e))?;
    let acquired = materialize(work, &repos, progress)
        .await
        .map_err(|e| stopped(Phase::Materialize, e))?;

    let run = collect(work, config, options, progress)
        .await
        .map_err(|e| stopped(Phase::Collect, e))?;

    let package = assemble(work, config, options.destination.clone(), progress)
        .map_err(|e| stopped(Phase::Package, e))?;

    Ok(ChainReport {
        installed,
        acquired,
        run,
        package,
        gaps,
    })
}

/// Attribute a phase's failure to that phase.
///
/// Boxed because [`AuditError`] is already large enough for
/// `clippy::result_large_err` to care, and nesting one inside another without a
/// box would grow every `Result` in the crate.
fn stopped(phase: Phase, source: AuditError) -> AuditError {
    AuditError::ChainStopped {
        phase,
        source: Box::new(source),
    }
}

/// Phase 1 — the pinned tool set, installed only if it is not already right.
///
/// The same call `run` makes (#5797), so the chain cannot install a different
/// set from the one the standalone verb installs. `--no-install` declines,
/// leaving the sweep's own fail-closed preflight to refuse in [`Phase::Collect`]
/// — which is the correct attribution: with the opt-out the operator asked for
/// the tools not to be fetched, and the phase that then cannot proceed is the
/// sweep.
async fn install(
    work: &WorkDir,
    config: &EngagementConfig,
    auto_install: bool,
    progress: &Progress,
) -> Result<Option<Vec<InstalledTool>>, AuditError> {
    if !auto_install {
        return Ok(None);
    }
    tools::ensure(work, &config.tools, progress).await
}

/// The registered repositories, and one gap line per target the chain cannot
/// audit.
fn split_targets(work: &WorkDir) -> Result<(Vec<String>, Vec<String>), AuditError> {
    let registry = Registry::load(work)?;
    let mut repos = Vec::new();
    let mut gaps = Vec::new();
    for target in registry.targets() {
        match target {
            Target::Repo { name_with_owner } => repos.push(name_with_owner.clone()),
            Target::Board { .. } => gaps.push(board_gap(target)),
        }
    }
    Ok((repos, gaps))
}

/// Why a registered board is a gap rather than a collected dimension.
///
/// Why: `tga audit` does take a board — its config has a `jira:` section and it
/// runs a `JiraSync` stage — but tga reads that section's `token` as a literal
/// string, with none of the `${VAR}` expansion its Linear and Azure DevOps
/// clients apply (`tga::collect::env_expand::expand_env_var`). Passing a JIRA
/// board through today therefore means writing the board credential into the
/// generated `state/tga-<stem>.yaml`, and this crate's whole credential posture
/// is that a secret reaches a child through its environment and never through a
/// file (DOC-68 §13, `crate::run`'s module docs). The chain states the gap
/// instead, and the durable fix is env-var expansion on tga's JIRA credential.
/// What: one line naming the board, safe to show the recipient.
/// Test: `super::chain_tests::a_registered_board_is_stated_as_a_gap`.
fn board_gap(target: &Target) -> String {
    format!(
        "{target} was not audited — this client cannot pass a board to `tga audit` without \
         writing its credential to a file, which it will not do (#5824)"
    )
}

/// Phase 2 — turn what is registered into checkouts the sweep can read.
///
/// Why: the registry-to-sweep gap, see the module docs.
///
/// An EMPTY repository set is not automatically a refusal: an operator who
/// cloned by hand before the registry existed has a selection on disk, and the
/// chain must keep working for them (#5824 requirement 4). So an empty registry
/// falls through to whatever `clone` last selected, and only the case where
/// there is no selection either is refused — with the remedy named, because
/// `NoRepositoriesSelected`'s "nothing in this file" does not tell an operator
/// to go and register something.
/// What: [`clone::clone_all`], which reuses complete checkouts and records the
/// usable ones as the selection. A repository that fails to clone is a gap on
/// its report and is NOT selected, so the sweep never records it as audited.
/// Test: `super::chain_tests::an_engagement_with_nothing_to_audit_stops_in_materialize`,
/// `super::chain_tests::an_empty_registry_falls_back_to_an_existing_selection`.
async fn materialize(
    work: &WorkDir,
    repos: &[String],
    progress: &Progress,
) -> Result<Option<CloneReport>, AuditError> {
    if repos.is_empty() {
        return match run::load_selection(work) {
            Ok(_) => Ok(None),
            Err(AuditError::NoRepositoriesSelected { .. }) => Err(AuditError::NothingRegistered {
                root: work.root().to_path_buf(),
            }),
            // A truncated or unreadable selection is a different fact, and
            // reporting it as "nothing is registered" would send the operator
            // to fix the wrong file.
            Err(other) => Err(other),
        };
    }
    clone::clone_all(work, repos, &CloneOptions::default(), progress)
        .await
        .map(Some)
}

/// Phase 3 — collect and analyze, refusing to advance over a sweep that
/// audited nothing.
///
/// Why: this is the fail-open branch #5824 names. The chain deliberately
/// continues past a repository that failed, so the guard has to be at the point
/// where continuing would produce a deliverable describing nothing:
/// [`RunStatus::AllFailed`] means every child failed, and packaging that would
/// hand the recipient a zip that looks like a completed engagement. It stops
/// here, attributed to the phase that actually failed rather than to the
/// package assembly it would otherwise trip a stage later.
///
/// A repository that FAILED among others that succeeded is not this case — that
/// package is worth sending and names what it omits.
/// What: [`run::sweep`] unchanged, then the status check.
/// Test: `super::chain_tests::a_sweep_that_audited_nothing_stops_before_packaging`,
/// `super::chain_tests::a_partly_failed_chain_packages_and_still_does_not_exit_zero`.
async fn collect(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &ChainOptions,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    let report = run::sweep(
        work,
        config,
        &RunOptions {
            fresh: options.fresh,
        },
        progress,
    )
    .await?;
    if report.status == RunStatus::AllFailed {
        return Err(AuditError::NothingAudited {
            attempted: report.repos.len(),
        });
    }
    Ok(report)
}

/// Phase 4 — assemble the deliverable, through the same completion check the
/// standalone verb uses.
///
/// Why: [`package::from_checkpoint`] rather than [`package::assemble`] over the
/// report already in hand, so the chain proves the same thing `taudit package`
/// proves — that the record on disk describes a sweep which FINISHED. Passing
/// the in-memory report would skip that check and leave two packaging paths with
/// different preconditions.
/// What: announces the phase through `progress`, then assembles.
/// Test: `super::chain_tests::the_chain_installs_collects_and_packages`,
/// `super::chain_tests::progress_covers_every_phase`.
fn assemble(
    work: &WorkDir,
    config: &EngagementConfig,
    destination: Option<PathBuf>,
    progress: &Progress,
) -> Result<ReturnPackage, AuditError> {
    let destination = destination.unwrap_or_else(|| package::default_destination(work));
    let name = destination.file_name().map_or_else(
        || destination.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    progress.operation_started(Operation::Package, 1);
    progress.unit_started(Operation::Package, name.as_str(), 1, 1);
    let assembled = package::from_checkpoint(work, config, &destination);
    progress.unit_finished(
        Operation::Package,
        name.as_str(),
        match &assembled {
            Ok(_) => UnitOutcome::Succeeded,
            Err(e) => UnitOutcome::Failed(e.to_string()),
        },
    );
    progress.operation_finished(Operation::Package, usize::from(assembled.is_ok()), 1);
    assembled
}
