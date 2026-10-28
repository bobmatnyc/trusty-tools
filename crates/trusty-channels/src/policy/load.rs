//! The route loader: home, the host file, each listed project's route file,
//! the default-branch gate, then the pure merge.
//!
//! Why: #8454 S2 plan §3.5: every consumer reads one [`LoadReport`] built
//! the same way, so no consumer re-implements a deny.
//! What: [`load_effective`] runs [`prepare`] (all I/O and git) and
//! [`assemble`] (the merge plus `Missing` statuses). [`crate::policy::PolicyLoader`]
//! reuses both for reload. Fault isolation follows Q1: a home or host fault
//! denies all; a project-file fault refuses that project only.
//! Test: `src/policy/tests/load.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::gchat::routes::routes_path;
use crate::policy::fs::{read_host, read_project, ProjectRead};
use crate::policy::gate::{check_default_branch, with_deadline, GateError};
use crate::policy::host::{parse_host, HostCeiling, HostError};
use crate::policy::merge::{merge_for, ProjectInput};
use crate::policy::project_file::{parse_project_file, ProjectFile, ProjectFileError};
use crate::policy::report::{FileState, FileStatus, Finding, LoadReport};
use crate::policy::types::Channel;

/// What a load serves: the host file, home, an optional single project,
/// and the consumer's channels.
///
/// Why: gchat-mcp serves one project's gchat routes; the tm daemon serves
/// every listed project's Slack and Telegram routes (S2 plan §3.5).
/// What: `project`, when set, keeps only that listed directory.
/// Test: `project_filter_keeps_one_listed_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    /// trusty-mpm's `config.yaml`.
    pub host_path: PathBuf,
    /// The home directory; `None` denies all (`HomeUnknown`).
    pub home: Option<PathBuf>,
    /// Load only this project dir, when it is listed.
    pub project: Option<PathBuf>,
    /// The channels whose routes enter the policy.
    pub channels: Vec<Channel>,
}

impl LoadRequest {
    /// The production request: `~/.trusty-tools/trusty-mpm/config.yaml`
    /// under `dirs::home_dir()`. No home leaves `home` `None` (deny all).
    pub fn for_host(project: Option<PathBuf>, channels: &[Channel]) -> Self {
        let home = dirs::home_dir();
        // #8454 S2b: no home means no host path; the empty path is never
        // read because `home` is None and that denies first.
        let host_path = home
            .as_deref()
            .map(|h| trusty_common::crate_config::crate_config_path_at(h, "trusty-mpm"))
            .unwrap_or_default();
        Self {
            host_path,
            home,
            project,
            channels: channels.to_vec(),
        }
    }
}

/// Read and parse the host ceiling alone: no project file, no git.
///
/// Why (#10387): the egress guard reads `channels.slack_read_allowlist` once
/// per Slack read call, under the same symlink, canonical-path, size and
/// strict-parse rules as the full load, and never through the lenient
/// `TrustyToolsConfig` loader.
/// What: [`read_host`] of `~/.trusty-tools/trusty-mpm/config.yaml` under
/// `home`, then [`parse_host`]. Every fault is the [`HostError`] the full
/// load would report.
/// Test: `read_host_ceiling_reads_the_allowlist_and_refuses_faults`.
pub fn read_host_ceiling(home: &Path) -> Result<HostCeiling, HostError> {
    let path = trusty_common::crate_config::crate_config_path_at(home, "trusty-mpm");
    read_host(&path, home).and_then(|text| parse_host(&text, Some(home)))
}

/// Load the effective policy for `req`.
///
/// Why: the one entry point consumers call; a deny is always an empty
/// policy in the report, never an error a caller can drop.
/// What: [`prepare`] then [`assemble`].
/// Test: `home_unknown_denies`, `host_missing_denies_all`,
/// `adding_a_project_needs_no_code_change`, `committed_malformed_file_refused`.
pub fn load_effective(req: &LoadRequest) -> LoadReport {
    let prepared = prepare(req);
    assemble(prepared, &req.channels)
}

/// Load the effective policy for `req`, with git bounded by `deadline`.
///
/// Why: #8454: a caller with a budget (tm doctor gives a load 30 s on its
/// own thread) must get the load back, and no git process left running,
/// by its budget; one blocked step per project can otherwise outlast it.
/// What: as [`load_effective`], except each git step runs for at most
/// `min(GIT_TIMEOUT, time left)` and its whole process group is killed
/// at the deadline. Once the deadline has passed, every project not yet
/// loaded is refused with [`GateError::GitTimedOut`]; none is effective.
/// The call returns soon after `deadline`, with no git process left.
/// Test: `load_with_a_deadline_returns_within_deadline_and_margin`,
/// `load_deadline_refuses_every_project_not_yet_loaded`,
/// `no_git_process_survives_a_load_deadline`.
pub fn load_effective_until(req: &LoadRequest, deadline: Instant) -> LoadReport {
    let prepared = prepare_until(req, Some(deadline));
    assemble(prepared, &req.channels)
}

/// A project file read: its bytes, `None` when missing, or the fault's
/// message. The form the reload fingerprint records (#9808).
pub(crate) type FileRead = Result<Option<Vec<u8>>, String>;

/// One listed project's route file after the read and the gate.
#[derive(Debug, Clone)]
pub(crate) struct PreparedProject {
    pub(crate) dir: PathBuf,
    pub(crate) file: PathBuf,
    /// `None` for a missing file.
    pub(crate) parsed: Option<Result<ProjectFile, ProjectFileError>>,
    /// #9808: the bytes the gate and the parse saw; `None` when never read.
    pub(crate) read: Option<FileRead>,
    /// #9832: the commit the gate verified those bytes at; `None` when the
    /// gate did not pass.
    pub(crate) commit: Option<String>,
}

/// Everything a load read, before the merge.
///
/// Why: #9808: the reloader keeps a fingerprint only when it describes what
/// this load read, so the load records that, in the fingerprint's terms.
/// What: `host_read` is the host text or the read fault's message;
/// `canonical` is the canonical-path verdict, `None` when the host did not
/// parse; each project carries its `read`.
/// Test: `host_file_change_and_revert_during_load_reloads`,
/// `canonical_fault_cleared_and_restored_during_load_reloads`.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub(crate) host: Result<HostCeiling, HostError>,
    pub(crate) projects: Vec<PreparedProject>,
    pub(crate) findings: Vec<Finding>,
    pub(crate) host_read: Result<String, String>,
    pub(crate) canonical: Option<Result<(), String>>,
}

impl Prepared {
    /// A load denied by `e` before any project was read.
    fn denied(
        e: HostError,
        host_read: Result<String, String>,
        canonical: Option<Result<(), String>>,
    ) -> Self {
        Self {
            host: Err(e),
            projects: Vec::new(),
            findings: Vec::new(),
            host_read,
            canonical,
        }
    }
}

/// Read and gate everything a load needs. No fault is skipped: each one is
/// a host error or a project file error.
pub(crate) fn prepare(req: &LoadRequest) -> Prepared {
    prepare_until(req, None)
}

/// [`prepare`], with every git step ending by `deadline` when one is set.
fn prepare_until(req: &LoadRequest, deadline: Option<Instant>) -> Prepared {
    let Some(home) = req.home.as_deref() else {
        // #8454 S2b: no HOME, no cwd fallback.
        let e = HostError::HomeUnknown;
        return Prepared::denied(e.clone(), Err(e.to_string()), None);
    };
    let text = read_host(&req.host_path, home);
    // #9808: record the read as the reload fingerprint does.
    let host_read = text.clone().map_err(|e| e.to_string());
    let host = match text.and_then(|t| parse_host(&t, Some(home))) {
        Ok(h) => h,
        Err(e) => return Prepared::denied(e, host_read, None),
    };
    let checked = check_canonical(&host);
    // #9808: the verdict this load saw, which a later sample may not.
    let canonical = Some(checked.clone().map_err(|e| e.to_string()));
    if let Err(e) = checked {
        return Prepared::denied(e, host_read, canonical);
    }
    let mut findings = Vec::new();
    let dirs = listed_dirs(&host, &req.channels, req.project.as_deref());
    if let (Some(p), true) = (&req.project, dirs.is_empty()) {
        findings.push(Finding::ProjectUnlisted { project: p.clone() });
    }
    let projects = dirs
        .into_iter()
        .map(|dir| {
            let file = routes_path(&dir);
            let (read, parsed, commit) = match deadline {
                // #8454: past the deadline, a project not yet loaded is refused.
                // #9808: it was never read, so `read` is `None`.
                Some(d) if Instant::now() >= d => (
                    None,
                    Some(Err(gate_error(GateError::GitTimedOut { step: "load" }))),
                    None,
                ),
                _ => {
                    let gated = with_deadline(deadline, || read_gated(&dir, home));
                    (Some(gated.read), gated.parsed, gated.commit)
                }
            };
            PreparedProject {
                dir,
                file,
                parsed,
                read,
                commit,
            }
        })
        .collect();
    Prepared {
        host: Ok(host),
        projects,
        findings,
        host_read,
        canonical,
    }
}

/// The directories listed for `channels` that are enabled, in host order,
/// each once; only `filter` when set.
pub(crate) fn listed_dirs(
    host: &HostCeiling,
    channels: &[Channel],
    filter: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for c in Channel::ALL.into_iter().filter(|c| channels.contains(c)) {
        let Some(ch) = host.channel(c).filter(|ch| ch.enabled()) else {
            continue;
        };
        for p in ch.projects() {
            if !dirs.contains(p) && filter.is_none_or(|f| f == p) {
                dirs.push(p.clone());
            }
        }
    }
    dirs
}

/// #8454 S2 plan §4: every `projects` entry, on every channel, must be its
/// own canonical path. A missing dir is not refused here: it has no file.
pub(crate) fn check_canonical(host: &HostCeiling) -> Result<(), HostError> {
    for c in Channel::ALL {
        let Some(ch) = host.channel(c) else { continue };
        for (index, p) in ch.projects().iter().enumerate() {
            let fail = |reason: &str| HostError::Project {
                channel: c,
                index,
                reason: reason.into(),
            };
            match std::fs::canonicalize(p) {
                Ok(real) if real == *p => {}
                Ok(_) => return Err(fail("is not canonical (a symlinked component)")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(fail("cannot be resolved")),
            }
        }
    }
    Ok(())
}

/// One project file after [`read_gated`].
struct Gated {
    /// #9808: the read, for the fingerprint.
    read: FileRead,
    /// The parse; `None` when the file is missing.
    parsed: Option<Result<ProjectFile, ProjectFileError>>,
    /// #9832: the commit the gate verified the bytes at.
    commit: Option<String>,
}

/// Read one project's file, run the Db1 gate on the bytes read, then parse
/// those bytes.
fn read_gated(dir: &Path, home: &Path) -> Gated {
    let fault = |read, parsed| Gated {
        read,
        parsed,
        commit: None,
    };
    let bytes = match read_project(dir) {
        Ok(ProjectRead::Missing) => return fault(Ok(None), None),
        Ok(ProjectRead::Bytes(b)) => b,
        Err(e) => return fault(Err(e.to_string()), Some(Err(e))),
    };
    // #8454 Db1: gate and parse see the same bytes.
    let (parsed, commit) = match check_default_branch(dir, &bytes) {
        Err(error) => (Err(gate_error(error)), None),
        Ok(state) => (
            std::str::from_utf8(&bytes)
                .map_err(|_| ProjectFileError::NotUtf8)
                .and_then(|text| parse_project_file(text, Some(home))),
            // #9832: the commit these bytes were verified at, not a re-read.
            Some(state.commit),
        ),
    };
    Gated {
        read: Ok(Some(bytes)),
        parsed: Some(parsed),
        commit,
    }
}

fn gate_error(error: GateError) -> ProjectFileError {
    ProjectFileError::Gate { error }
}

/// Merge prepared inputs for `channels` and add a `Missing` status for each
/// project with no file, keeping host order.
pub(crate) fn assemble(prepared: Prepared, channels: &[Channel]) -> LoadReport {
    let Prepared {
        host,
        projects,
        findings,
        ..
    } = prepared;
    let order: Vec<(PathBuf, PathBuf, bool)> = projects
        .iter()
        .map(|p| (p.dir.clone(), p.file.clone(), p.parsed.is_none()))
        .collect();
    // #9832: each file's verified commit, copied onto its status below.
    let commits: HashMap<PathBuf, String> = projects
        .iter()
        .filter_map(|p| Some((p.file.clone(), p.commit.clone()?)))
        .collect();
    let inputs = projects
        .into_iter()
        .filter_map(|p| {
            p.parsed.map(|parsed| ProjectInput {
                project_dir: p.dir,
                file: p.file,
                parsed,
            })
        })
        .collect();
    let mut report = merge_for(host, inputs, channels);
    let mut merged = std::mem::take(&mut report.per_file).into_iter();
    report.per_file = order
        .into_iter()
        .filter_map(|(dir, file, missing)| {
            if missing {
                Some(FileStatus::new(&dir, &file, FileState::Missing))
            } else {
                merged.next()
            }
        })
        .collect();
    report.findings.extend(findings);
    // #9832: only a file whose routes are in effect names its commit.
    for s in &mut report.per_file {
        if matches!(s.state, FileState::Effective { .. }) {
            s.verified_commit = commits.get(&s.file).cloned();
        }
    }
    report
}
