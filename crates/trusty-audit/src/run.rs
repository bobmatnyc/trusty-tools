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
//! The run-progress record is written LAST, after every child has finished, and
//! a failure to write it fails the whole call — a record that cannot be written
//! must not leave the client claiming a run it cannot describe.
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
//! Test: `super::run_tests`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use crate::config::{EngagementConfig, ToolPins};
use crate::error::AuditError;
use crate::inference;
use crate::manifest::AuditManifest;
use crate::progress::{Operation, Progress, UnitOutcome};
use crate::relay::tee_and_relay;
use crate::tools::{self, RequiredTool};
use crate::workdir::{Area, WorkDir};

/// File under `state/` naming the repositories the run should audit.
///
/// Why: repository selection is separate work (#5487, #5497) and repository
/// cloning is separate again (#5215). #5555 does not implement either — it
/// defines the file both will write, so neither has to redesign this module's
/// input. The shape is deliberately the same `{ name, path }` pair as tga's own
/// manifest, because that is what a selection is.
///
/// ```toml
/// # <work-dir>/state/selected-repos.toml
/// count = 2                   # how many entries follow — REQUIRED
///
/// [[repositories]]
/// name = "acme-api"
/// path = "repos/acme-api"     # relative paths anchor to the work-dir root
///
/// [[repositories]]
/// name = "acme-web"
/// path = "repos/acme-web"
/// ```
///
/// ## Two obligations on whoever writes it
///
/// 1. **Write to a temporary file in the same directory and rename it into
///    place.** A rename is atomic; a direct write is not, and a producer that
///    crashes part-way through one leaves syntactically valid TOML holding a
///    prefix of the entries.
/// 2. **Declare `count` first**, before the `[[repositories]]` tables. TOML
///    requires top-level keys to precede tables anyway, so a truncated file
///    keeps the count and loses entries — which is exactly the direction that
///    makes the mismatch detectable. A `count` that disagrees with the number of
///    entries is [`AuditError::TruncatedSelection`], not a smaller selection.
///
/// Obligation 2 is what makes obligation 1 checkable rather than a request. A
/// sweep that silently audits three of five repositories and reports
/// `AllSucceeded` is the same fail-open shape as a sweep over none.
pub const SELECTION_FILE: &str = "selected-repos.toml";

/// File under `state/` recording what the last sweep did, per repository.
pub const PROGRESS_FILE: &str = "run-progress.toml";

/// One repository the run was asked to audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SelectedRepo {
    /// Display name, used for the output directory and the log file.
    pub name: String,
    /// Checkout path. Relative paths anchor to the working-directory root.
    pub path: PathBuf,
}

/// The `state/selected-repos.toml` document.
///
/// `count` is required and is the truncation check — see [`SELECTION_FILE`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Selection {
    count: usize,
    #[serde(default)]
    repositories: Vec<SelectedRepo>,
}

/// What happened to one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RepoResult {
    /// `tga audit` exited 0 for this repository.
    Succeeded,
    /// It did not. The reason is the child's status, or why it never started.
    Failed {
        /// One line naming what went wrong, safe to show the recipient.
        reason: String,
    },
}

impl RepoResult {
    /// Whether this repository was audited successfully.
    pub fn succeeded(&self) -> bool {
        matches!(self, RepoResult::Succeeded)
    }
}

/// One repository's run: where its output and log landed, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RepoRun {
    /// The repository, as selected.
    pub repo: SelectedRepo,
    /// Directory under `out/` holding this repository's audit output.
    pub output: PathBuf,
    /// File under `logs/` holding the child's combined stdout and stderr.
    pub log: PathBuf,
    /// Gaps `tga audit` stated in this repository's manifest (DOC-67 §9).
    ///
    /// A gap is a dimension the sweep could not assess — an unconfigured JIRA
    /// project, a repository that could not be fetched from its remote. It is
    /// ordinary for a successful run to state some, so a gap does not by itself
    /// fail the repository; see [`verify_output`] for which ones do.
    #[serde(default)]
    pub gaps: Vec<String>,
    /// How it ended.
    pub result: RepoResult,
}

/// Whether the sweep succeeded, partly succeeded, or failed outright.
///
/// Why: closure condition 3 of #5555, and DOC-67 §9's failed-but-continuing
/// model. "One repository of six failed" and "every repository failed" call for
/// different actions from the recipient, and collapsing them into a boolean is
/// how a partial run gets delivered as a whole one.
/// What: three states over the per-repo results. An empty sweep never reaches
/// here — no selection is an error, not an [`AllSucceeded`](Self::AllSucceeded).
/// Test: `super::run_tests::status_distinguishes_partial_from_total_failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RunStatus {
    /// Every selected repository was audited.
    AllSucceeded,
    /// Some repositories were audited and some were not.
    Partial,
    /// No repository was audited.
    AllFailed,
}

/// The result of one sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunReport {
    /// One entry per selected repository, in selection order.
    pub repos: Vec<RepoRun>,
    /// The sweep's overall verdict.
    pub status: RunStatus,
}

impl RunReport {
    /// Build a report and derive its status from the per-repo results.
    ///
    /// Why: the status is DERIVED, never passed in, so no caller can construct
    /// a report claiming a status its per-repo results do not support.
    /// Test: `super::run_tests::status_distinguishes_partial_from_total_failure`.
    pub fn of(repos: Vec<RepoRun>) -> Self {
        let succeeded = repos.iter().filter(|r| r.result.succeeded()).count();
        let status = if succeeded == repos.len() {
            RunStatus::AllSucceeded
        } else if succeeded == 0 {
            RunStatus::AllFailed
        } else {
            RunStatus::Partial
        };
        Self { repos, status }
    }

    /// The repositories that failed.
    pub fn failures(&self) -> impl Iterator<Item = &RepoRun> {
        self.repos.iter().filter(|r| !r.result.succeeded())
    }
}

/// Where the repository selection is read from.
pub fn selection_path(work: &WorkDir) -> PathBuf {
    work.path(Area::State).join(SELECTION_FILE)
}

/// Where the run-progress record is written.
pub fn progress_path(work: &WorkDir) -> PathBuf {
    work.path(Area::State).join(PROGRESS_FILE)
}

/// Read the repository selection.
///
/// Why: the input contract #5487/#5215 fill. Absent and empty are the same
/// state — nothing was selected — and both are a refusal rather than a
/// zero-repository success, because a sweep that audits nothing and exits 0 is
/// the fail-open shape this module exists to avoid.
///
/// A THIRD state is a refusal too: a file whose `count` does not match the
/// entries it carries. That is the truncated-write case a producer crashing
/// mid-write leaves behind, and it is indistinguishable from a smaller
/// selection unless the count says otherwise.
/// What: parses `state/`[`SELECTION_FILE`] and checks the count.
/// Test: `super::run_tests::an_absent_selection_is_a_refusal`,
/// `super::run_tests::a_truncated_selection_is_refused`.
///
/// # Errors
///
/// [`AuditError::NoRepositoriesSelected`] when the file is absent or lists
/// nothing, [`AuditError::TruncatedSelection`] when `count` disagrees with the
/// entries, [`AuditError::Read`] when it exists but cannot be read, and
/// [`AuditError::Parse`] when it does not match the schema — including when
/// `count` is absent, since a file without it cannot be checked at all.
pub fn load_selection(work: &WorkDir) -> Result<Vec<SelectedRepo>, AuditError> {
    let path = selection_path(work);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuditError::NoRepositoriesSelected { path });
        }
        Err(source) => return Err(AuditError::Read { path, source }),
    };
    let selection: Selection = toml::from_str(&text).map_err(|source| AuditError::Parse {
        path: path.clone(),
        what: "repository selection",
        source: Box::new(source),
    })?;
    if selection.repositories.is_empty() {
        return Err(AuditError::NoRepositoriesSelected { path });
    }
    // #5555: a prefix of a crashed write parses cleanly; only the count catches it.
    if selection.count != selection.repositories.len() {
        return Err(AuditError::TruncatedSelection {
            path,
            declared: selection.count,
            found: selection.repositories.len(),
        });
    }
    Ok(selection.repositories)
}

/// Record the repositories the next sweep should audit.
///
/// Why: [`SELECTION_FILE`] states two obligations on whoever writes it, and
/// #5556 found there was no writer at all — `taudit clone` acquired the
/// checkouts, `taudit run` then refused with "nothing to audit", and every
/// per-stage test passed throughout. The writer lives beside the reader so the
/// atomic-rename and `count`-first obligations are one decision rather than a
/// note each producer re-reads; #5497's picker writes through here too.
/// What: renders `count` ahead of the entries (serde field order, and `toml`
/// emits values before tables), writes to a uniquely-named temporary file in
/// the same directory, and renames it into place. The unique name is what lets
/// two writers race without either reading the other's half-written file.
/// Test: `super::run_tests::a_saved_selection_reads_back_whole`,
/// `super::run_tests::racing_writers_never_leave_a_torn_selection`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the state area cannot be made, the temporary
/// file cannot be written, or the rename fails.
pub fn save_selection(work: &WorkDir, repos: &[SelectedRepo]) -> Result<(), AuditError> {
    let path = selection_path(work);
    let dir = work.path(Area::State);
    std::fs::create_dir_all(&dir).map_err(|source| AuditError::WorkDir { path: dir, source })?;

    let selection = Selection {
        count: repos.len(),
        repositories: repos.to_vec(),
    };
    let text = toml::to_string_pretty(&selection).map_err(|e| AuditError::WorkDir {
        path: path.clone(),
        source: std::io::Error::other(e),
    })?;

    let temp = path.with_file_name(format!("{SELECTION_FILE}.{}.tmp", writer_tag()));
    std::fs::write(&temp, text).map_err(|source| AuditError::WorkDir {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, &path).map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        AuditError::WorkDir { path, source }
    })
}

/// A suffix no two concurrent writers share: process, plus thread within it.
fn writer_tag() -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    format!("{}-{}", std::process::id(), hasher.finish())
}

/// The four binaries a run drives, each proven to be at the engagement's pin.
///
/// Why: named fields rather than a lookup table, so the "tool not found" branch
/// does not exist. The obvious table version needs a fallback arm at every use
/// site, and the natural fallback — the bare binary name — is a `PATH` lookup,
/// which is the one thing this module must never do.
#[derive(Debug, Clone)]
struct PinnedBinaries {
    tga: PathBuf,
    search: PathBuf,
    analyze: PathBuf,
    review: PathBuf,
}

/// The pinned binaries this run drives, or a refusal naming what is wrong.
///
/// Why: the run must use the binaries THIS client installed and verified at the
/// version THIS engagement pins — never whatever `tga` happens to be on the
/// operator's `PATH`, and never a copy installed before the config was bumped.
/// Both are the #5454 version-skew class, and there is no fallback for either.
///
/// Three conditions, each a refusal: the file is present, the version record
/// this client wrote names it, and that recorded version equals the engagement's
/// pin. The second matters because a binary someone dropped into `tools/` by
/// hand reads as `installed` with no version — unverified is not a weaker kind
/// of installed. The third matters because install and run are separate steps,
/// so the config can change between them.
/// What: reads [`tools::status`], checks all three conditions, and returns the
/// paths by name.
/// Test: `super::run_tests::a_run_without_the_pinned_tools_is_refused`,
/// `super::run_tests::an_unverified_binary_does_not_count_as_installed`,
/// `super::run_tests::a_binary_installed_at_a_different_pin_is_refused`.
///
/// # Errors
///
/// [`AuditError::ToolsNotInstalled`] naming every tool that is missing or
/// unverified, [`AuditError::VersionMismatch`] for the first tool whose recorded
/// version is not the engagement's pin, and whatever [`tools::status`] fails
/// with.
fn pinned_binaries(work: &WorkDir, pins: &ToolPins) -> Result<PinnedBinaries, AuditError> {
    let statuses = tools::status(work)?;
    let missing: Vec<&'static str> = statuses
        .iter()
        .filter(|s| !s.installed || s.version.is_none())
        .map(|s| s.tool.binary_name())
        .collect();
    if !missing.is_empty() {
        return Err(AuditError::ToolsNotInstalled { missing });
    }

    let path_of = |tool: RequiredTool| -> Result<PathBuf, AuditError> {
        let pinned = tool.pin_in(pins).version();
        let status = statuses.iter().find(|s| s.tool == tool).ok_or_else(|| {
            AuditError::ToolsNotInstalled {
                missing: vec![tool.binary_name()],
            }
        })?;
        // `version` is Some: the missing check above rejected every None.
        match status.version.as_deref() {
            Some(v) if v == pinned => Ok(status.path.clone()),
            Some(v) => Err(AuditError::VersionMismatch {
                tool: tool.crate_name(),
                pinned: pinned.to_owned(),
                installed: v.to_owned(),
            }),
            None => Err(AuditError::ToolsNotInstalled {
                missing: vec![tool.binary_name()],
            }),
        }
    };

    Ok(PinnedBinaries {
        tga: path_of(RequiredTool::Tga)?,
        search: path_of(RequiredTool::TrustySearch)?,
        analyze: path_of(RequiredTool::TrustyAnalyze)?,
        review: path_of(RequiredTool::TrustyReview)?,
    })
}

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
#[derive(Debug, Serialize)]
struct TgaConfig {
    repositories: Vec<TgaRepository>,
    database: PathBuf,
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
/// `state/`[`PROGRESS_FILE`] records the same results. [`RunReport::status`] is
/// [`RunStatus::AllSucceeded`] only when every child exited 0 AND left the
/// artifacts [`verify_output`] requires. On `Err`, no claim is made about any
/// repository.
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
pub async fn sweep(
    work: &WorkDir,
    config: &EngagementConfig,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    sweep_with_budget(work, config, PER_REPO_TIMEOUT, progress).await
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
    budget: std::time::Duration,
    progress: &Progress,
) -> Result<RunReport, AuditError> {
    sweep_with_env(work, config, budget, progress, |name| {
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
async fn sweep_with_env<F>(
    work: &WorkDir,
    config: &EngagementConfig,
    budget: std::time::Duration,
    progress: &Progress,
    operator: F,
) -> Result<RunReport, AuditError>
where
    F: Fn(&str) -> Option<String>,
{
    work.create()?;
    let binaries = pinned_binaries(work, &config.tools)?;
    // Resolved once, before any child: a half-named selection is identical for
    // every repository, so failing per-repo would just repeat one misconfiguration.
    let inference = inference::inference_env(config, operator)?;
    let selected = load_selection(work)?;

    // #5823: the operation is announced only once the refusals above are past,
    // so a display never opens on a sweep that is not going to run.
    let total = selected.len();
    progress.operation_started(Operation::Sweep, total);
    let mut runs = Vec::with_capacity(total);
    for (index, repo) in selected.into_iter().enumerate() {
        runs.push(
            run_one(
                work, config, &binaries, &inference, index, repo, budget, progress, total,
            )
            .await?,
        );
    }

    let report = RunReport::of(runs);
    progress.operation_finished(
        Operation::Sweep,
        report.repos.iter().filter(|r| r.result.succeeded()).count(),
        total,
    );
    write_progress(work, &report)?;
    Ok(report)
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
    inference: &[(&'static str, String)],
    index: usize,
    repo: SelectedRepo,
    budget: std::time::Duration,
    progress: &Progress,
    total: usize,
) -> Result<RepoRun, AuditError> {
    let stem = stem(index, &repo.name);
    let output = work.path(Area::Output).join(&stem);
    let log = work.path(Area::Logs).join(format!("{stem}.log"));
    let checkout = absolute_checkout(work, &repo.path);
    progress.unit_started(Operation::Sweep, repo.name.as_str(), index + 1, total);

    let mut gaps = Vec::new();
    let result = match prepare(work, &output, &stem, &checkout)? {
        Err(reason) => RepoResult::Failed { reason },
        Ok(config_path) => {
            match spawn_tga(
                binaries,
                config,
                inference,
                &config_path,
                &output,
                &log,
                work.root(),
                budget,
                progress,
                &repo.name,
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
        result,
    })
}

/// The gap line `tga` writes when a collection stage failed but the sweep
/// continued (`tga::audit::gaps::sweep_gap_lines`, DOC-67 §9).
///
/// Why: `tga audit` exits 0 whenever the sweep COMPLETED, failed stages
/// included — its own docs say so. The failure reaches the manifest as prose,
/// which is the only channel tga offers today, so matching that prose is the
/// only way this client can tell "assessed" from "assessed nothing".
const COLLECT_FAILED_MARKER: &str = "stage `collect` did not complete";

/// What a zero exit is allowed to mean.
///
/// Why: the finding that made this necessary. `tga audit` returns `Ok` whenever
/// the sweep completed even with failed stages, so exit 0 alone does not say
/// anything was assessed — a collect stage that failed on auth, a rate limit or
/// an empty clone still exits 0. Believing that status is how the recipient gets
/// a report assessing nothing with every signal green.
///
/// # Postconditions
/// On `Ok`, `<output>/manifest.toml` exists, parses, names at least one
/// repository, and states no failed COLLECT stage. The returned gap lines are
/// whatever else the manifest stated. On `Err`, the string is a one-line reason
/// safe to show the recipient.
///
/// What: two checks of different confidence, and the difference is deliberate.
///
/// - **Structural**, and reliable: the manifest is there, parses, and names a
///   repository. A child that wrote nothing cannot pass this whatever tga's
///   wording does.
/// - **Textual**, and brittle: a gap line naming a failed `collect` stage. tga
///   owns that prose and could reword it, at which point this check silently
///   stops firing. It is a second layer over the structural check, never the
///   only one — and every other gap is recorded on the [`RepoRun`] and rendered,
///   so a reworded marker still reaches the operator as a stated gap rather than
///   disappearing. The durable fix is structured per-stage status in the
///   manifest, which is tga's to add.
///
/// Other failed stages (jira, dora, pr-metrics) are NOT failures here: DOC-67
/// §9 makes them named gaps on a report that is still worth delivering, and
/// failing on any gap would fail nearly every real engagement.
/// Test: `super::run_tests::a_child_that_exits_zero_having_written_nothing_fails`,
/// `super::run_tests::a_manifest_reporting_a_failed_collect_stage_fails`,
/// `super::run_tests::ordinary_gaps_do_not_fail_the_repository`.
fn verify_output(output: &Path) -> Result<Vec<String>, String> {
    let manifest_path = output.join(AuditManifest::FILE_NAME);
    let manifest = match AuditManifest::load_if_present(&manifest_path) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return Err(format!(
                "`tga audit` exited 0 but wrote no manifest to {} — nothing was assessed",
                output.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "`tga audit` exited 0 but its manifest at {} cannot be read: {e}",
                manifest_path.display()
            ));
        }
    };
    if manifest.repositories.is_empty() {
        return Err(format!(
            "`tga audit` exited 0 but its manifest at {} names no repository — nothing was assessed",
            manifest_path.display()
        ));
    }
    if let Some(gap) = manifest
        .report
        .gaps
        .iter()
        .find(|g| g.contains(COLLECT_FAILED_MARKER))
    {
        return Err(format!(
            "`tga audit` exited 0 but collection did not complete: {gap}"
        ));
    }
    Ok(manifest.report.gaps)
}

/// Everything that must be true before a child is worth starting.
///
/// The inner `Result` is the per-repo verdict: `Err(reason)` is a recorded
/// failure for this repository, while the outer `Result` is a failure of the
/// sweep itself (the working directory is not writable).
fn prepare(
    work: &WorkDir,
    output: &Path,
    stem: &str,
    checkout: &Path,
) -> Result<Result<PathBuf, String>, AuditError> {
    if !checkout.is_dir() {
        return Ok(Err(format!(
            "no checkout at {} — nothing was audited for this repository",
            checkout.display()
        )));
    }
    mkdir(output)?;
    let config_path = work.path(Area::State).join(format!("tga-{stem}.yaml"));
    let document = TgaConfig {
        repositories: vec![TgaRepository {
            path: checkout.to_path_buf(),
            name: stem.to_string(),
        }],
        database: work.path(Area::Extract).join(format!("{stem}.db")),
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

/// Spawn the pinned `tga audit` and turn its exit into a per-repo verdict.
///
/// The child inherits nothing it does not need: the four binaries are named by
/// absolute path or by the variables tga and trusty-review read
/// (`TRUSTY_SEARCH_BIN`, `TRUSTY_ANALYZE_BIN`, `TRUSTY_REVIEW_BIN`), so nothing
/// on the operator's `PATH` can be reached instead. The credential goes in the
/// environment and only there — see the module docs for what that costs.
///
/// Alongside the credential the child gets the provider and per-role model ids
/// from [`crate::inference`]: naming the key never routed anything to
/// OpenRouter on its own, because `trusty-review` defaults to Bedrock (#5671).
///
/// A child that outlives `budget` is killed and recorded as a failure, so one
/// hung repository costs that repository rather than the whole run.
///
/// #5823: the child's streams are PIPED rather than pointed straight at the log
/// file, and this function tees them — every byte still reaches the log, and the
/// progress lines the child writes on stderr additionally reach `progress`. The
/// log is unchanged as a record; what changed is that it is no longer the only
/// place the output goes.
#[allow(clippy::too_many_arguments)]
async fn spawn_tga(
    binaries: &PinnedBinaries,
    config: &EngagementConfig,
    inference: &[(&'static str, String)],
    config_path: &Path,
    output: &Path,
    log: &Path,
    cwd: &Path,
    budget: std::time::Duration,
    progress: &Progress,
    target: &str,
) -> Result<RepoResult, AuditError> {
    let file = std::fs::File::create(log).map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;
    let errors = file.try_clone().map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;

    let mut command = tokio::process::Command::new(&binaries.tga);
    command
        .arg("--config")
        .arg(config_path)
        .arg("audit")
        .arg("--output")
        .arg(output)
        .current_dir(cwd)
        .env(ENV_INFERENCE_CREDENTIAL, config.openrouter_key.expose())
        // #5823: ask the child to write its per-stage events where this process
        // can read them. A child too old to know the variable ignores it, and
        // the sweep shows the coarse per-repository progress it derives itself.
        .env(ENV_PROGRESS_RELAY, "1")
        // #5670: `tga audit` starts trusty-search and indexes each repository
        // through it. On a recipient's clean machine the pinned copy in
        // `work/tools/` is the only one there is, so without this the guard falls
        // through to a PATH lookup and refuses the run.
        .env(ENV_SEARCH_BIN, &binaries.search)
        .env(ENV_ANALYZE_BIN, &binaries.analyze)
        .env(ENV_REVIEW_BIN, &binaries.review)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // #5671: the credential alone never reached OpenRouter — trusty-review
    // defaults to Bedrock, so the provider and the three role models must be
    // named too. Resolved by `sweep_with_env`: either all four or none, never a
    // subset that could pair one provider with another's model ids.
    for (name, value) in inference {
        command.env(name, value);
    }

    let spawned = command.spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(source) => {
            return Ok(RepoResult::Failed {
                reason: format!("`tga audit` could not be started: {source}"),
            });
        }
    };

    // #5823: both streams are pumped concurrently with the wait. Reading them
    // is not optional now that they are pipes — a child that fills a pipe
    // buffer nobody drains blocks forever, which would turn every sizeable
    // sweep into the four-hour timeout.
    let mut pumps = Vec::with_capacity(2);
    if let Some(stream) = child.stdout.take() {
        pumps.push(tokio::spawn(tee_and_relay(
            stream,
            tokio::fs::File::from_std(file),
            progress.clone(),
            target.to_owned(),
        )));
    }
    if let Some(stream) = child.stderr.take() {
        pumps.push(tokio::spawn(tee_and_relay(
            stream,
            tokio::fs::File::from_std(errors),
            progress.clone(),
            target.to_owned(),
        )));
    }

    let verdict = match tokio::time::timeout(budget, child.wait()).await {
        Ok(Ok(status)) if status.success() => RepoResult::Succeeded,
        Ok(Ok(status)) => RepoResult::Failed {
            reason: format!(
                "`tga audit` exited with {}; see {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |c| format!("code {c}")),
                log.display()
            ),
        },
        Ok(Err(source)) => RepoResult::Failed {
            reason: format!("`tga audit` could not be waited on: {source}"),
        },
        Err(_elapsed) => {
            // Kill before returning: `kill_on_drop` would do it, but only once
            // the handle drops, and the reason must name a child that is gone.
            let killed = child.kill().await;
            RepoResult::Failed {
                reason: format!(
                    "`tga audit` timed out after {}s and was killed{}; see {}",
                    budget.as_secs(),
                    match killed {
                        Ok(()) => String::new(),
                        Err(e) => format!(" (kill failed: {e})"),
                    },
                    log.display()
                ),
            }
        }
    };

    // The child has exited or been killed, so both pipes are at EOF and the
    // pumps end on their own. Awaiting them is what guarantees the log holds
    // everything the child said before this function reports on it.
    Ok(join_pumps(pumps, log, verdict).await)
}

/// Wait for the output pumps, downgrading a success whose log is incomplete.
///
/// Why: the log is the only record a failed sweep is diagnosed from, and this
/// module's posture is that a run whose result cannot be recorded must not
/// return as a success (#5655). A pump that failed means the log is missing
/// bytes the child wrote, so a `Succeeded` verdict resting on it is downgraded
/// rather than reported. A verdict that was already a failure keeps its own
/// reason — the pump error is the less useful of the two.
/// What: awaits each pump; on the first error, replaces a `Succeeded` verdict.
/// A pump task that panicked is treated the same way.
/// Test: `super::run_tests::a_childs_stage_events_reach_the_progress_sink`
/// covers the whole-log obligation this protects.
async fn join_pumps(
    pumps: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
    log: &Path,
    verdict: RepoResult,
) -> RepoResult {
    let mut broken: Option<String> = None;
    for pump in pumps {
        let failure = match pump.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(e) => Some(e.to_string()),
        };
        broken = broken.or(failure);
    }
    match (broken, &verdict) {
        (Some(reason), RepoResult::Succeeded) => RepoResult::Failed {
            reason: format!(
                "`tga audit` finished but its output could not be written to {}: {reason}",
                log.display()
            ),
        },
        _ => verdict,
    }
}

/// The variable `tga audit` reads the inference credential from.
pub const ENV_INFERENCE_CREDENTIAL: &str = "OPENROUTER_API_KEY";

/// The variable that asks a child to relay its progress events (#5823).
///
/// Why: named through `trusty-progress` rather than spelled here, because the
/// producer reads the same constant — a literal in this file would be a second
/// copy of the contract, free to drift.
const ENV_PROGRESS_RELAY: &str = trusty_progress::relay::ENV_RELAY;

/// The variable `tga audit` reads its trusty-search binary from (#5670).
const ENV_SEARCH_BIN: &str = "TRUSTY_SEARCH_BIN";

/// The variable `trusty-review` reads its analyze binary from.
const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// The variable `tga audit` reads its report renderer from.
const ENV_REVIEW_BIN: &str = "TRUSTY_REVIEW_BIN";

/// Record what the sweep did, per repository.
///
/// Why: `workdir` names `state/` as where run progress lives, and #5499
/// assembles the deliverable from it. Writing it last means a record only ever
/// describes a sweep that finished.
/// What: overwrites `state/`[`PROGRESS_FILE`] with the whole report.
/// Test: `super::run_tests::the_progress_record_survives_a_partial_run`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the record cannot be written. This is NOT
/// downgraded to a warning: a run whose result cannot be recorded must not
/// return as a success (#5655).
fn write_progress(work: &WorkDir, report: &RunReport) -> Result<(), AuditError> {
    let path = progress_path(work);
    let text = toml::to_string_pretty(report).map_err(|e| AuditError::WorkDir {
        path: path.clone(),
        source: std::io::Error::other(e),
    })?;
    std::fs::write(&path, text).map_err(|source| AuditError::WorkDir { path, source })
}

/// Read the last sweep's record, or nothing when none has run.
///
/// # Errors
///
/// [`AuditError::Read`] when the record exists but cannot be read, and
/// [`AuditError::Parse`] when it is malformed.
pub fn read_progress(work: &WorkDir) -> Result<Option<RunReport>, AuditError> {
    let path = progress_path(work);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AuditError::Read { path, source }),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|source| AuditError::Parse {
            path,
            what: "run progress record",
            source: Box::new(source),
        })
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::progress::{ProgressUpdate, Recorder, StageEvent, StageState};

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

    /// Place stub binaries AND the version record, which together are what
    /// `pinned_binaries` accepts.
    fn install_stubs(work: &WorkDir, script: &str) {
        for tool in RequiredTool::ALL {
            let path = tool.path_in(work);
            std::fs::write(&path, script).expect("stub binary");
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
            },
            SelectedRepo {
                name: "acme/web".to_owned(),
                path: PathBuf::from("repos/acme/web"),
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

        let err = sweep(&work, &config(), &Progress::none())
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
            },
            output: "/o/a".into(),
            log: "/l/a.log".into(),
            gaps: Vec::new(),
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

        let report = sweep(&work, &config(), &Progress::none())
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
        assert_eq!(recorded, report);
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

        let report = sweep(&work, &config(), &Progress::none())
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

        let report = sweep(&work, &config(), &Progress::none())
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

        let report = sweep(&work, &config(), &Progress::none())
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

        let report = sweep(&work, &config(), &Progress::none())
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

        let report = sweep(&work, &config(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");
        assert_eq!(report.repos[0].gaps.len(), 1, "the gap must be surfaced");
        assert!(report.repos[0].gaps[0].contains("jira sync"));
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

        let report = sweep(&work, &config(), &Progress::none())
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
        let report = sweep(&work, &config(), &progress)
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
        let report = sweep(&work, &config(), &progress)
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

        let report = sweep(&work, &config(), &Progress::none())
            .await
            .expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded);
        assert!(report.repos[0].log.is_file(), "the log is still written");
    }
}
