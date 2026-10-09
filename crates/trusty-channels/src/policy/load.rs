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

use std::path::{Path, PathBuf};

use crate::gchat::routes::routes_path;
use crate::policy::fs::{read_host, read_project, ProjectRead};
use crate::policy::gate::{check_default_branch, GateError};
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

/// One listed project's route file after the read and the gate.
#[derive(Debug, Clone)]
pub(crate) struct PreparedProject {
    pub(crate) dir: PathBuf,
    pub(crate) file: PathBuf,
    /// `None` for a missing file.
    pub(crate) parsed: Option<Result<ProjectFile, ProjectFileError>>,
}

/// Everything a load read, before the merge.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub(crate) host: Result<HostCeiling, HostError>,
    pub(crate) projects: Vec<PreparedProject>,
    pub(crate) findings: Vec<Finding>,
}

/// Read and gate everything a load needs. No fault is skipped: each one is
/// a host error or a project file error.
pub(crate) fn prepare(req: &LoadRequest) -> Prepared {
    let denied = |e: HostError| Prepared {
        host: Err(e),
        projects: Vec::new(),
        findings: Vec::new(),
    };
    // RED STUB (#8454 S2b): no home or canonical check yet.
    let home = req.home.as_deref().unwrap_or(Path::new(""));
    let host = match read_host(&req.host_path).and_then(|t| parse_host(&t, Some(home))) {
        Ok(h) => h,
        Err(e) => return denied(e),
    };
    let _ = check_canonical;
    let mut findings = Vec::new();
    let dirs = listed_dirs(&host, &req.channels, req.project.as_deref());
    if let (Some(p), true) = (&req.project, dirs.is_empty()) {
        findings.push(Finding::ProjectUnlisted { project: p.clone() });
    }
    let projects = dirs
        .into_iter()
        .map(|dir| {
            let file = routes_path(&dir);
            let parsed = read_gated(&dir, home);
            PreparedProject { dir, file, parsed }
        })
        .collect();
    Prepared {
        host: Ok(host),
        projects,
        findings,
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
fn check_canonical(host: &HostCeiling) -> Result<(), HostError> {
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

/// Read one project's file, run the Db1 gate on the bytes read, then parse
/// those bytes. `None` when the file is missing.
fn read_gated(dir: &Path, home: &Path) -> Option<Result<ProjectFile, ProjectFileError>> {
    let bytes = match read_project(dir) {
        Ok(ProjectRead::Missing) => return None,
        Ok(ProjectRead::Bytes(b)) => b,
        Err(e) => return Some(Err(e)),
    };
    // #8454 Db1: gate and parse see the same bytes.
    if let Err(error) = check_default_branch(dir, &bytes) {
        return Some(Err(gate_error(error)));
    }
    let parsed = String::from_utf8(bytes)
        .map_err(|_| ProjectFileError::NotUtf8)
        .and_then(|text| parse_project_file(&text, Some(home)).or(Ok(ProjectFile::default()))); // RED STUB
    Some(parsed)
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
    } = prepared;
    let order: Vec<(PathBuf, PathBuf, bool)> = projects
        .iter()
        .map(|p| (p.dir.clone(), p.file.clone(), p.parsed.is_none()))
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
    report
}
