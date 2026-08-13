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

use crate::config::EngagementConfig;
use crate::error::AuditError;
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
/// [[repositories]]
/// name = "acme-api"
/// path = "repos/acme-api"     # relative paths anchor to the work-dir root
/// ```
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Selection {
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
/// What: parses `state/`[`SELECTION_FILE`].
/// Test: `super::run_tests::an_absent_selection_is_a_refusal`.
///
/// # Errors
///
/// [`AuditError::NoRepositoriesSelected`] when the file is absent or lists
/// nothing, [`AuditError::Read`] when it exists but cannot be read, and
/// [`AuditError::Parse`] when it does not match the schema.
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
    Ok(selection.repositories)
}

/// The pinned binaries this run drives, or a refusal naming what is missing.
///
/// Why: the run must use the binaries THIS client installed and verified, never
/// whatever `tga` happens to be on the operator's `PATH`. An unpinned tool is
/// the version-skew mismatch #5454 cost us, and there is no fallback here on
/// purpose — a missing pinned tool stops the run and says to install it.
///
/// A tool counts as usable only when the file is present AND the version record
/// this client wrote names it. A binary someone dropped into `tools/` by hand
/// reads as `installed` with no version, and this refuses it: unverified is not
/// a weaker kind of installed.
/// What: [`RequiredTool::ALL`] paired with its path under `tools/`.
/// Test: `super::run_tests::a_run_without_the_pinned_tools_is_refused`,
/// `super::run_tests::an_unverified_binary_does_not_count_as_installed`.
///
/// # Errors
///
/// [`AuditError::ToolsNotInstalled`] naming every tool that is missing or
/// unverified, and whatever [`tools::status`] fails with.
fn pinned_binaries(work: &WorkDir) -> Result<Vec<(RequiredTool, PathBuf)>, AuditError> {
    let statuses = tools::status(work)?;
    let missing: Vec<&'static str> = statuses
        .iter()
        .filter(|s| !s.installed || s.version.is_none())
        .map(|s| s.tool.binary_name())
        .collect();
    if !missing.is_empty() {
        return Err(AuditError::ToolsNotInstalled { missing });
    }
    Ok(statuses.iter().map(|s| (s.tool, s.path.clone())).collect())
}

/// A filename-safe form of a repository name.
///
/// Why: the name comes from a selection file this client did not write, and it
/// becomes a directory name and a log filename under the work-dir root. A name
/// containing `../` or a separator would place those outside the root and break
/// `workdir`'s deletion promise.
/// What: keeps ASCII alphanumerics, `-`, `_` and `.`; every other byte becomes
/// `-`. A name that reduces to nothing, or to only dots, becomes `repo`.
/// Test: `super::run_tests::a_traversing_repository_name_cannot_escape_the_root`.
fn slug(name: &str) -> String {
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
/// [`RunStatus::AllSucceeded`] only when every child exited 0. On `Err`, no
/// claim is made about any repository.
///
/// What: checks the tools, reads the selection, then per repository writes a
/// generated tga config under `state/`, spawns the pinned `tga audit` with the
/// pinned `trusty-analyze`/`trusty-review` named by environment, and captures
/// the child's combined output into `logs/`. A repository whose checkout is
/// missing, or whose child fails to start or exits non-zero, is recorded as a
/// failure and the sweep continues — DOC-67 §9's failed-but-continuing model.
/// Test: `super::run_tests`, and `crate::session::session_tests`.
///
/// # Errors
///
/// [`AuditError::ToolsNotInstalled`] before anything runs,
/// [`AuditError::NoRepositoriesSelected`] when nothing is selected,
/// [`AuditError::WorkDir`] when an output, log or state file cannot be written.
/// A failing repository is NOT an error — it is a recorded failure and a
/// non-`AllSucceeded` status.
pub async fn sweep(work: &WorkDir, config: &EngagementConfig) -> Result<RunReport, AuditError> {
    work.create()?;
    let binaries = pinned_binaries(work)?;
    let selected = load_selection(work)?;

    let mut runs = Vec::with_capacity(selected.len());
    for repo in selected {
        runs.push(run_one(work, config, &binaries, repo).await?);
    }

    let report = RunReport::of(runs);
    write_progress(work, &report)?;
    Ok(report)
}

/// Audit one repository, recording rather than propagating its failure.
async fn run_one(
    work: &WorkDir,
    config: &EngagementConfig,
    binaries: &[(RequiredTool, PathBuf)],
    repo: SelectedRepo,
) -> Result<RepoRun, AuditError> {
    let slug = slug(&repo.name);
    let output = work.path(Area::Output).join(&slug);
    let log = work.path(Area::Logs).join(format!("{slug}.log"));
    let checkout = absolute_checkout(work, &repo.path);

    let result = match prepare(work, &output, &slug, &checkout)? {
        Err(reason) => RepoResult::Failed { reason },
        Ok(config_path) => {
            spawn_tga(binaries, config, &config_path, &output, &log, work.root()).await?
        }
    };
    Ok(RepoRun {
        repo,
        output,
        log,
        result,
    })
}

/// Everything that must be true before a child is worth starting.
///
/// The inner `Result` is the per-repo verdict: `Err(reason)` is a recorded
/// failure for this repository, while the outer `Result` is a failure of the
/// sweep itself (the working directory is not writable).
fn prepare(
    work: &WorkDir,
    output: &Path,
    slug: &str,
    checkout: &Path,
) -> Result<Result<PathBuf, String>, AuditError> {
    if !checkout.is_dir() {
        return Ok(Err(format!(
            "no checkout at {} — nothing was audited for this repository",
            checkout.display()
        )));
    }
    mkdir(output)?;
    let config_path = work.path(Area::State).join(format!("tga-{slug}.yaml"));
    let document = TgaConfig {
        repositories: vec![TgaRepository {
            path: checkout.to_path_buf(),
            name: slug.to_string(),
        }],
        database: work.path(Area::Extract).join(format!("{slug}.db")),
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

/// Spawn the pinned `tga audit` and turn its exit into a per-repo verdict.
///
/// The child inherits nothing it does not need: the three binaries are named by
/// absolute path or by the variables tga and trusty-review read
/// (`TRUSTY_ANALYZE_BIN`, `TRUSTY_REVIEW_BIN`), so nothing on the operator's
/// `PATH` can be reached instead. The credential goes in the environment and
/// only there — see the module docs for what that costs.
async fn spawn_tga(
    binaries: &[(RequiredTool, PathBuf)],
    config: &EngagementConfig,
    config_path: &Path,
    output: &Path,
    log: &Path,
    cwd: &Path,
) -> Result<RepoResult, AuditError> {
    let binary = |tool: RequiredTool| -> PathBuf {
        binaries
            .iter()
            .find(|(t, _)| *t == tool)
            .map(|(_, p)| p.clone())
            // Unreachable: `pinned_binaries` returns one entry per
            // `RequiredTool::ALL` or refuses, so every tool is present.
            .unwrap_or_else(|| PathBuf::from(tool.binary_name()))
    };

    let file = std::fs::File::create(log).map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;
    let errors = file.try_clone().map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;

    let status = tokio::process::Command::new(binary(RequiredTool::Tga))
        .arg("--config")
        .arg(config_path)
        .arg("audit")
        .arg("--output")
        .arg(output)
        .current_dir(cwd)
        .env(
            crate::run::ENV_INFERENCE_CREDENTIAL,
            config.openrouter_key.expose(),
        )
        .env(ENV_ANALYZE_BIN, binary(RequiredTool::TrustyAnalyze))
        .env(ENV_REVIEW_BIN, binary(RequiredTool::TrustyReview))
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(errors))
        .status()
        .await;

    Ok(match status {
        Ok(status) if status.success() => RepoResult::Succeeded,
        Ok(status) => RepoResult::Failed {
            reason: format!(
                "`tga audit` exited with {}; see {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |c| format!("code {c}")),
                log.display()
            ),
        },
        Err(source) => RepoResult::Failed {
            reason: format!("`tga audit` could not be started: {source}"),
        },
    })
}

/// The variable `tga audit` reads the inference credential from.
pub const ENV_INFERENCE_CREDENTIAL: &str = "OPENROUTER_API_KEY";

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

    const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
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
        let text = toml::to_string_pretty(&Selection { repositories }).expect("render");
        std::fs::write(selection_path(work), text).expect("write selection");
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
             [[tools]]\ncrate_name = \"trusty-analyze\"\nversion = \"0.9.2\"\nbinary = \"{a}\"\n\
             [[tools]]\ncrate_name = \"trusty-review\"\nversion = \"0.15.1\"\nbinary = \"{r}\"\n",
            tga = RequiredTool::Tga.path_in(work).display(),
            a = RequiredTool::TrustyAnalyze.path_in(work).display(),
            r = RequiredTool::TrustyReview.path_in(work).display(),
        );
        std::fs::write(tools::record_path(work), record).expect("write record");
    }

    fn make_repo(work: &WorkDir, name: &str) {
        std::fs::create_dir_all(work.path(Area::Repos).join(name)).expect("mkdir repo");
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
        std::fs::write(selection_path(&work), "repositories = []\n").expect("write");
        let err = load_selection(&work).expect_err("an empty list audits nothing");
        assert!(
            matches!(err, AuditError::NoRepositoriesSelected { .. }),
            "{err:?}"
        );
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

        let err = sweep(&work, &config())
            .await
            .expect_err("no pinned tga means no run");
        let AuditError::ToolsNotInstalled { missing } = err else {
            panic!("expected ToolsNotInstalled, got {err:?}");
        };
        assert_eq!(missing.len(), RequiredTool::ALL.len());
    }

    /// A binary this client did not install and verify is not a usable binary.
    #[test]
    fn an_unverified_binary_does_not_count_as_installed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        for tool in RequiredTool::ALL {
            std::fs::write(tool.path_in(&work), b"stub").expect("stub");
        }
        let err = pinned_binaries(&work).expect_err("no version record means unverified");
        let AuditError::ToolsNotInstalled { missing } = err else {
            panic!("expected ToolsNotInstalled, got {err:?}");
        };
        assert_eq!(missing.len(), RequiredTool::ALL.len());
    }

    #[test]
    fn a_traversing_repository_name_cannot_escape_the_root() {
        let work = WorkDir::new("/work");
        for name in ["../../etc", "a/b", "..", "", "he re"] {
            let s = slug(name);
            let path = work.path(Area::Output).join(&s);
            assert!(path.starts_with(work.root()), "{name:?} escaped as {s:?}");
            assert!(!s.contains('/'), "{name:?} kept a separator: {s:?}");
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

        let report = sweep(&work, &config()).await.expect("the sweep completes");
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
        install_stubs(&work, "#!/bin/sh\nexit 0\n");
        make_repo(&work, "acme-api");
        select(
            &work,
            &[("acme-api", "repos/acme-api"), ("gone", "repos/gone")],
        );

        let report = sweep(&work, &config()).await.expect("the sweep completes");
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

    /// The credential reaches the child and nothing else does — no generated
    /// file may carry it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_key_reaches_the_child_by_environment_and_is_never_written_down() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_stubs(
            &work,
            "#!/bin/sh\ntest -n \"$OPENROUTER_API_KEY\" || exit 9\n\
             test -n \"$TRUSTY_REVIEW_BIN\" || exit 8\n\
             test -n \"$TRUSTY_ANALYZE_BIN\" || exit 7\nexit 0\n",
        );
        make_repo(&work, "acme-api");
        select(&work, &[("acme-api", "repos/acme-api")]);

        let report = sweep(&work, &config()).await.expect("the sweep completes");
        assert_eq!(report.status, RunStatus::AllSucceeded, "{report:?}");

        // Nothing this run wrote may contain the plaintext key.
        for area in [Area::State, Area::Logs, Area::Output] {
            for entry in std::fs::read_dir(work.path(area))
                .expect("read area")
                .flatten()
            {
                if entry.path().is_file() {
                    let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
                    assert!(
                        !text.contains("sk-or-v1-not-a-real-key"),
                        "{} carries the key",
                        entry.path().display()
                    );
                }
            }
        }
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

        let report = sweep(&work, &config()).await.expect("the sweep completes");
        for run in &report.repos {
            assert!(run.output.starts_with(work.root()), "{:?}", run.output);
            assert!(run.log.starts_with(work.root()), "{:?}", run.log);
        }
        assert!(progress_path(&work).starts_with(work.root()));
    }
}
