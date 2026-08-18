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
//! A registered BOARD reaches it too, since #5857. It is not a unit of the
//! sweep — there is nothing to clone — so it becomes a `jira:` or `linear:`
//! section on every child's generated config, with its credential referenced as
//! `${TRUSTY_AUDIT_JIRA_TOKEN}` and delivered through the child's environment.
//! [`run::boards`] owns that split. A board with no credential in the engagement
//! config is still a gap, and still reported rather than dropped.
//!
//! Since #5979 the target set comes from `engagement.toml` rather than from
//! `state/audit-targets.toml`, which is now a working copy rebuilt from it.
//! [`crate::registry::engagement_targets`] owns that precedence; nothing else
//! in this module changed.
//!
//! The target set is read ONCE per [`audit`], in the materialize phase. The gaps
//! that reach the return package and the board sections that reach each child
//! come out of that single read: [`split_targets`] resolves, and [`collect`]
//! forwards the result to [`run::sweep_with_boards`] rather than loading the
//! registry again. Reading it twice is what the phases below make dangerous —
//! tool install and every clone sit between them, so the two reads are an
//! engagement's wall clock apart and a `taudit remove board` in another terminal
//! lands cleanly inside the window (#5857).
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
//!   the package's own README and metadata. So does every target the chain
//!   never attempted: a repository that failed to CLONE reaches the same two
//!   generated members, because the recipient opening the zip has those and not
//!   this process's console output (#5824 review).
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
use crate::registry::{self, Target};
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
    /// What this engagement targets and the chain never attempted.
    ///
    /// Two sources: a registered board [`run::boards::resolve`] could not turn
    /// into a config section, and a registered repository that failed to clone.
    /// Neither reaches the sweep, so neither can appear among
    /// [`RunReport::failures`] — which is why the chain carries them itself
    /// rather than deriving everything from the sweep.
    ///
    /// [`ReturnPackage::excluded`] is the superset: these lines PLUS the
    /// repositories the sweep attempted and failed. Non-empty here makes the
    /// run's exit status non-zero, so a silently unaudited target cannot read as
    /// a whole engagement.
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

    // #5857: the registry is read ONCE for this whole invocation. `collect` is
    // handed the resolution rather than repeating it, because the phases below
    // span an engagement's worth of wall clock and the registry is a file
    // another terminal can edit.
    let (repos, boards) =
        split_targets(work, config).map_err(|e| stopped(Phase::Materialize, e))?;
    let mut gaps = boards.gaps.clone();
    let acquired = materialize(work, &repos, progress)
        .await
        .map_err(|e| stopped(Phase::Materialize, e))?;
    // #5824: a repository that failed to clone is named ONLY on the clone
    // report. It is never selected, so the sweep cannot report it and nothing
    // downstream would otherwise learn it was registered.
    if let Some(acquired) = &acquired {
        gaps.extend(acquired.gaps.iter().cloned());
    }

    let run = collect(work, config, options, &boards, progress)
        .await
        .map_err(|e| stopped(Phase::Collect, e))?;

    let package = assemble(work, config, &gaps, options.destination.clone(), progress)
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
///
/// Why: a registered BOARD used to land here as an unconditional gap — the
/// wiring simply stopped at the repository arm (#5857). It now goes to
/// [`run::boards::resolve`], and the WHOLE resolution is returned rather than
/// only its gap lines, because [`audit`] hands it to [`collect`]: the gaps this
/// states and the sections that child gets are then one decision made once, not
/// two reads of a mutable file hours apart. What is left as a gap is narrow: a
/// board whose provider has no credential in the engagement config, and a second
/// JIRA project (tga's `jira.project_key` holds one).
/// What: repositories by name for [`materialize`], and the resolution itself.
/// Test: `super::chain_tests::a_registered_board_with_a_credential_is_collected`,
/// `super::chain_tests::a_board_with_no_configured_credential_is_still_a_gap`,
/// `super::chain_tests::a_board_removed_after_the_chain_resolved_it_still_reaches_the_child`.
fn split_targets(
    work: &WorkDir,
    config: &EngagementConfig,
) -> Result<(Vec<String>, run::boards::Boards), AuditError> {
    // #5979: the config declares the set; the working copy is the fallback for
    // an engagement that has not declared one yet.
    let targets = registry::engagement_targets(Some(config), work)?;
    let repos = targets
        .iter()
        .filter_map(|target| match target {
            Target::Repo { name_with_owner } => Some(name_with_owner.clone()),
            // #6001: a local target's id IS its path, which is exactly the spec
            // `clone_all` resolves — so acquisition takes one list, not two.
            Target::LocalRepo { .. } => Some(target.id()),
            Target::Board { .. } => None,
        })
        .collect();
    let boards = run::boards::resolve(&targets, &config.boards);
    Ok((repos, boards))
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
/// What: [`run::sweep_with_boards`], then the status check. `boards` is
/// [`split_targets`]'s resolution, forwarded so the sweep does not read the
/// registry a second time — see that function and [`audit`].
/// Test: `super::chain_tests::a_sweep_that_audited_nothing_stops_before_packaging`,
/// `super::chain_tests::a_partly_failed_chain_packages_and_still_does_not_exit_zero`,
/// `super::chain_tests::a_board_removed_after_the_chain_resolved_it_still_reaches_the_child`.
async fn collect(
    work: &WorkDir,
    config: &EngagementConfig,
    options: &ChainOptions,
    boards: &run::boards::Boards,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    let report = run::sweep_with_boards(
        work,
        config,
        &RunOptions {
            fresh: options.fresh,
        },
        boards,
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
///
/// `gaps` is threaded through rather than merged into
/// [`ReturnPackage::excluded`] afterwards, because the zip is already written by
/// the time this returns: a gap appended to the returned value would correct
/// what this process prints and leave the README and metadata INSIDE the
/// deliverable still claiming full coverage — and the zip is what the third
/// party opens (#5824 review).
/// What: announces the phase through `progress`, then assembles.
/// Test: `super::chain_tests::the_chain_installs_collects_and_packages`,
/// `super::chain_tests::progress_covers_every_phase`,
/// `cli_end_to_end::a_registered_repository_that_never_cloned_is_named_in_the_package`.
fn assemble(
    work: &WorkDir,
    config: &EngagementConfig,
    gaps: &[String],
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
    let assembled = package::from_checkpoint(work, config, gaps, &destination);
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

#[cfg(test)]
mod chain_tests {
    use std::path::Path;

    use super::*;
    use crate::progress::{ProgressUpdate, Recorder};
    use crate::registry::{Registry, TargetKind};
    use crate::run::{RepoResult, SelectedRepo};
    use crate::session::{Outcome, Session};
    use crate::tools::RequiredTool;
    use crate::workdir::Area;

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

    /// A stub `tga` that writes the manifest a real one would for every
    /// repository whose output stem is in `succeed`, and exits 1 for the rest.
    ///
    /// Matching on the stem is what lets one script produce a MIXED sweep, which
    /// is the case the fail-open test needs — a partial run, not a clean one and
    /// not a total failure.
    fn tga_stub(succeed: &[&str]) -> String {
        let cases: String = succeed
            .iter()
            .map(|stem| format!("  *{stem}*) ok=1 ;;\n"))
            .collect();
        format!(
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             ok=0\ncase \"$out\" in\n{cases}esac\n\
             [ \"$ok\" = 1 ] || exit 1\n\
             mkdir -p \"$out\"\n\
             printf '[report]\\ntitle = \"Acme\"\\n\\n[[repositories]]\\n\
             name = \"acme\"\\npath = \"/r\"\\n' > \"$out/manifest.toml\"\nexit 0\n"
        )
    }

    /// The stub binaries AND the version record, which together are what the
    /// sweep's `pinned_binaries` preflight accepts — so the install phase finds
    /// the pins satisfied and never reaches the network.
    ///
    /// #5915: `trusty-search` gets an approving stub rather than `script`. The
    /// sweep now runs `trusty-search index add <checkout>` before each tga
    /// child, and `script` describes what TGA does — sharing it would make every
    /// chain test that expects a failing tga also fail its approval, for a
    /// reason none of them is about.
    fn install_stubs(work: &WorkDir, script: &str) {
        for tool in RequiredTool::ALL {
            let path = tool.path_in(work);
            let body = if tool == RequiredTool::TrustySearch {
                "#!/bin/sh\nexit 0\n"
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
        std::fs::write(crate::tools::record_path(work), record).expect("write record");
    }

    /// A checkout on disk and a selection naming it.
    ///
    /// The chain's materialize phase falls back to the selection when the
    /// registry names no repository, which is what makes an offline test of the
    /// whole chain possible — cloning would reach GitHub.
    fn select(work: &WorkDir, names: &[&str]) {
        let repos: Vec<SelectedRepo> = names
            .iter()
            .map(|name| {
                std::fs::create_dir_all(work.path(Area::Repos).join(name)).expect("mkdir repo");
                SelectedRepo {
                    name: (*name).to_owned(),
                    path: PathBuf::from(format!("repos/{name}")),
                }
            })
            .collect();
        run::save_selection(work, &repos).expect("write selection");
    }

    /// One prepared working directory: stubs installed, checkouts on disk.
    fn prepared(dir: &Path, repos: &[&str], succeed: &[&str]) -> WorkDir {
        let work = work_in(dir);
        install_stubs(&work, &tga_stub(succeed));
        select(&work, repos);
        work
    }

    async fn run_chain(work: &WorkDir, progress: &Progress) -> Result<ChainReport, AuditError> {
        audit(work, &config(), &ChainOptions::default(), true, progress).await
    }

    /// The whole point of #5824: four verbs' worth of work from one call, ending
    /// in a file the recipient can send.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_chain_installs_collects_and_packages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);

        let report = run_chain(&work, &Progress::none())
            .await
            .expect("the chain runs");

        assert_eq!(
            report.run.status,
            RunStatus::AllSucceeded,
            "{:?}",
            report.run
        );
        assert!(report.package.path.is_file(), "the package must be a file");
        assert!(report.package.excluded.is_empty(), "{:?}", report.package);
        assert!(report.gaps.is_empty(), "{:?}", report.gaps);
        // The pins were already satisfied, so nothing was downloaded.
        assert!(report.installed.is_none());
        // Nothing registered, so the materialize phase cloned nothing and used
        // the selection already on disk.
        assert!(report.acquired.is_none());
        assert_eq!(Outcome::Audit(report).exit_code(), 0);
    }

    /// The fail-open guard, and the reason this PR needed one (#5824).
    ///
    /// The chain CONTINUES past a repository that failed — that is deliberate,
    /// and it is what makes a partial run reach the package phase at all. What
    /// must not happen is that partial run reading as a whole engagement: the
    /// package has to name the repository it does not cover, and the process
    /// exit status has to be non-zero so `taudit audit && send-it` stops.
    ///
    /// Without the `Outcome::Audit` arm of `Outcome::exit_code`, this fails on
    /// the last assertion with a zero status over a package covering one
    /// repository of two.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_partly_failed_chain_packages_and_still_does_not_exit_zero() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Only the first repository's output stem is accepted by the stub.
        let work = prepared(tmp.path(), &["acme-api", "acme-web"], &["00-acme-api"]);

        let report = run_chain(&work, &Progress::none())
            .await
            .expect("one repository failing is not a failure of the chain");

        assert_eq!(report.run.status, RunStatus::Partial, "{:?}", report.run);
        assert!(report.package.path.is_file());
        assert_eq!(report.package.excluded.len(), 1, "{:?}", report.package);
        assert!(
            report.package.excluded[0].contains("acme-web"),
            "the package must name what it does not cover: {:?}",
            report.package.excluded
        );
        assert_ne!(
            Outcome::Audit(report).exit_code(),
            0,
            "a partial engagement must not exit zero"
        );
    }

    /// The second half of the same guard: when NOTHING was audited there is no
    /// package at all, and the refusal names the phase that failed rather than
    /// the packaging it would otherwise trip one stage later.
    ///
    /// Without the `RunStatus::AllFailed` check in `collect`, the chain reaches
    /// `package::from_checkpoint`, which refuses too — so this fails on the
    /// phase assertion, reporting a collection failure as a packaging one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_that_audited_nothing_stops_before_packaging() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No stem is accepted, so every child exits 1.
        let work = prepared(tmp.path(), &["acme-api", "acme-web"], &[]);

        let err = run_chain(&work, &Progress::none())
            .await
            .expect_err("a sweep that audited nothing must not produce a package");

        let AuditError::ChainStopped { phase, source } = &err else {
            panic!("expected a phase-attributed refusal, got {err:?}");
        };
        assert_eq!(*phase, Phase::Collect, "{err}");
        assert!(
            matches!(**source, AuditError::NothingAudited { attempted: 2 }),
            "{source:?}"
        );
        assert!(
            !package::default_destination(&work).exists(),
            "a chain that audited nothing left a package behind"
        );
    }

    /// An engagement that targets nothing is a refusal naming the remedy, not an
    /// empty run — and it is attributed to the phase that could not proceed.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_engagement_with_nothing_to_audit_stops_in_materialize() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(&work, &tga_stub(&["acme-api"]));

        let err = run_chain(&work, &Progress::none())
            .await
            .expect_err("nothing is registered and nothing was cloned");

        let AuditError::ChainStopped { phase, source } = &err else {
            panic!("expected a phase-attributed refusal, got {err:?}");
        };
        assert_eq!(*phase, Phase::Materialize, "{err}");
        assert!(
            matches!(**source, AuditError::NothingRegistered { .. }),
            "{source:?}"
        );
        assert!(
            err.to_string().contains("trusty-audit add repo"),
            "the refusal must name the remedy: {err}"
        );
    }

    /// #5824 requirement 4: the operator who cloned by hand before the registry
    /// existed keeps working. An empty registry is not a refusal while a
    /// selection is on disk.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_registry_falls_back_to_an_existing_selection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);

        let report = run_chain(&work, &Progress::none())
            .await
            .expect("the chain runs");
        assert!(report.acquired.is_none(), "nothing needed cloning");
        assert_eq!(report.run.repos.len(), 1);
    }

    /// Register boards the way `taudit add board` does.
    fn register_boards(work: &WorkDir, specs: &[&str]) {
        let mut targets = Registry::default();
        for spec in specs {
            targets.insert(registry::parse(Some(TargetKind::Board), spec).expect("parses"));
        }
        targets.save(work).expect("writes");
    }

    /// 🔴 #5979: the sweep's targets come from the ENGAGEMENT. A config that
    /// declares a repository and a board reaches [`split_targets`] even with an
    /// empty working copy — which is what makes `engagement.toml` portable, and
    /// the working copy derived.
    ///
    /// Against `87b55c367` `split_targets` read only the working copy, so both
    /// assertions come back empty.
    #[test]
    fn the_engagement_declares_what_the_chain_audits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        Registry::default()
            .save(&work)
            .expect("an empty working copy");

        let declared = crate::config::with_targets(
            &format!(
                "{CONFIG}\n[boards.jira]\nurl = \"https://acme.atlassian.net\"\n\
                 email = \"auditor@acme.example\"\ntoken = \"jira-token-never-on-disk\"\n"
            ),
            &[
                registry::parse(Some(TargetKind::Repo), "acme/api").expect("parses"),
                registry::parse(Some(TargetKind::Board), "jira:ACME").expect("parses"),
            ],
            Path::new("engagement.toml"),
        )
        .expect("declares");
        let config =
            EngagementConfig::from_toml(&declared, Path::new("engagement.toml")).expect("parses");

        let (repos, boards) = split_targets(&work, &config).expect("splits");
        assert_eq!(repos, vec!["acme/api"]);
        assert!(boards.gaps.is_empty(), "{:?}", boards.gaps);
        assert!(
            boards.jira.is_some(),
            "the declared board reached the child"
        );
    }

    /// An engagement carrying the JIRA credential the registered board needs.
    fn config_with_jira() -> EngagementConfig {
        let text = format!(
            "{CONFIG}\n[boards.jira]\nurl = \"https://acme.atlassian.net\"\n\
             email = \"auditor@acme.example\"\ntoken = \"jira-token-never-on-disk\"\n"
        );
        EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses")
    }

    /// #5857's whole point: a registered board with a credential is COLLECTED.
    /// It contributes no gap, so a clean sweep over it exits zero.
    ///
    /// Against `origin/main` this fails on the gap assertion: `split_targets`
    /// turned every board into a gap line unconditionally, and the non-zero exit
    /// that followed made `taudit audit && send-it` refuse a whole engagement.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_registered_board_with_a_credential_is_collected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);
        register_boards(&work, &["jira:ACME"]);

        let report = audit(
            &work,
            &config_with_jira(),
            &ChainOptions::default(),
            true,
            &Progress::none(),
        )
        .await
        .expect("the chain runs");

        assert!(report.gaps.is_empty(), "{:?}", report.gaps);
        assert!(report.package.excluded.is_empty(), "{:?}", report.package);
        assert_eq!(
            Outcome::Audit(report).exit_code(),
            0,
            "a collected board must not hold the engagement at a non-zero exit"
        );
    }

    /// The one board case still stated as a gap: registered, but the engagement
    /// config carries no credential for its provider. Dropping it silently would
    /// leave the recipient sending a package that claims to cover an engagement
    /// it does not.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_board_with_no_configured_credential_is_still_a_gap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);
        register_boards(&work, &["jira:ACME"]);

        // `config()` names no `[boards]` table at all.
        let report = run_chain(&work, &Progress::none())
            .await
            .expect("the chain runs");

        assert_eq!(report.gaps.len(), 1, "{:?}", report.gaps);
        assert!(report.gaps[0].contains("jira:ACME"), "{:?}", report.gaps);
        assert!(report.gaps[0].contains("boards.jira"), "{:?}", report.gaps);
        assert_ne!(
            Outcome::Audit(report).exit_code(),
            0,
            "an unaudited target must not exit zero even when the sweep was clean"
        );
    }

    /// The registry is read ONCE per `audit`, so nothing that happens to it
    /// afterwards can move the coverage away from what the report states.
    ///
    /// Why this is worth a test: `audit` states its board gaps in the Materialize
    /// phase and the sweep runs after tool install and every clone — an
    /// hour-scale window. A `taudit remove board jira:ACME` inside it used to
    /// leave the report claiming coverage (no gap) while the child got no `jira:`
    /// section at all, and the engagement exited 0 having never read the board.
    ///
    /// Against `5a615fe0e` this fails on the `project_key: ACME` assertion:
    /// `run::sweep` called `Registry::load` a second time and found nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_board_removed_after_the_chain_resolved_it_still_reaches_the_child() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);
        register_boards(&work, &["jira:ACME"]);
        let config = config_with_jira();

        // Phase 2's half of `audit`: the one resolution, stating no gap.
        let (_repos, boards) = split_targets(&work, &config).expect("splits");
        assert!(boards.gaps.is_empty(), "{:?}", boards.gaps);

        // The window: install and every clone happen here, and a concurrent
        // `taudit remove board` lands inside it.
        Registry::default().save(&work).expect("writes");

        // Phase 3, driven exactly as `audit` drives it.
        let report = collect(
            &work,
            &config,
            &ChainOptions::default(),
            &boards,
            &Progress::none(),
        )
        .await
        .expect("the sweep runs");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        let generated =
            std::fs::read_to_string(work.path(Area::State).join("tga-00-acme-api.yaml"))
                .expect("the generated tga config");
        assert!(
            generated.contains("project_key: ACME"),
            "the report stated no gap for jira:ACME, so the child must have \
             collected it: {generated}"
        );
    }

    /// Resumable by construction: every phase is re-entrant, so a second run
    /// carries the first one's work over rather than repeating it.
    #[cfg(unix)]
    #[tokio::test]
    async fn re_running_the_chain_resumes_rather_than_re_collecting() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);

        let first = run_chain(&work, &Progress::none())
            .await
            .expect("the first run");
        assert_eq!(first.run.resumed().count(), 0);

        let second = run_chain(&work, &Progress::none())
            .await
            .expect("the second run");
        assert_eq!(second.run.resumed().count(), 1, "{:?}", second.run);
        assert!(second.run.repos[0].result.succeeded());
        assert!(second.package.path.is_file());
    }

    /// Closure condition 3 of #5824: the display does not fall silent partway
    /// through. Every phase that runs announces itself, including the packaging
    /// that used to happen with no output at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn progress_covers_every_phase() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);
        let (recorder, progress) = Recorder::new();

        run_chain(&work, &progress).await.expect("the chain runs");

        let operations: Vec<Operation> = recorder
            .updates()
            .into_iter()
            .filter_map(|u| match u {
                ProgressUpdate::OperationStarted { operation, .. } => Some(operation),
                _ => None,
            })
            .collect();
        assert!(operations.contains(&Operation::Sweep), "{operations:?}");
        assert!(operations.contains(&Operation::Package), "{operations:?}");
    }

    /// The chain is reachable the way every other capability is — through
    /// `Session::execute`, with no private path of its own (#5502).
    #[cfg(unix)]
    #[tokio::test]
    async fn the_chain_runs_through_the_one_dispatch_every_front_end_uses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api"], &["acme-api"]);
        let config_path = tmp.path().join("engagement.toml");
        std::fs::write(&config_path, CONFIG).expect("write config");
        let session = Session::new(work.clone()).with_config_path(&config_path);

        let outcome = session
            .execute(crate::session::Command::Audit(ChainOptions::default()))
            .await
            .expect("the chain runs through execute");
        let Outcome::Audit(report) = &outcome else {
            panic!("Audit must yield an Audit outcome: {outcome:?}");
        };
        assert!(report.package.path.is_file());
        assert_eq!(outcome.exit_code(), 0);
    }

    /// The failure a chained repository produces still reads as that
    /// repository's, not as "the audit failed" — requirement 2 of #5824 is about
    /// the target as much as the phase.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_repository_is_named_in_the_report_that_comes_back() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = prepared(tmp.path(), &["acme-api", "acme-web"], &["00-acme-api"]);

        let report = run_chain(&work, &Progress::none())
            .await
            .expect("the chain runs");

        let failed = report.run.failures().next().expect("one repository failed");
        assert_eq!(failed.repo.name, "acme-web");
        let RepoResult::Failed { reason } = &failed.result else {
            panic!("a failure must carry a reason");
        };
        assert!(
            reason.contains(&failed.log.display().to_string()),
            "the reason must point at this repository's own log: {reason}"
        );
    }
}
