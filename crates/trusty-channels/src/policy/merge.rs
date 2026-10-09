//! The pure merge of the host ceiling and the project route files.
//!
//! Why: a route takes effect only when its file is sound, its channel is
//! enabled, its project is listed for that channel, and it is no looser
//! than the ceiling (#8454 plan §3.4). Every other route is dropped with a
//! named finding. Keeping this pure (no I/O, no clock, no env) lets every
//! rule be tested from strings.
//! What: [`merge`] builds a [`LoadReport`]. Fault isolation follows the Q1
//! ruling: a host fault or a cross-file overlap denies all; a broken file
//! refuses only its own routes.
//! Test: `src/policy/tests/merge.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::gchat::routes::Connection;
use crate::policy::error::PolicyError;
use crate::policy::host::{HostCeiling, HostChannel, HostError};
use crate::policy::project_file::{ProjectFile, ProjectFileError, ProjectRoute};
use crate::policy::report::{FileState, FileStatus, Finding, FindingScope, LoadReport, Origin};
use crate::policy::table::{ChannelPolicy, PolicySpec};
use crate::policy::types::{Channel, RateLimitSpec, RouteSpec};

/// One project's route file, already read and parsed (S2b does the I/O).
///
/// Why: the merge needs to know which project a file speaks for, to check
/// the per-channel `projects` list (Architect Q7), and where it lives, to
/// name it in findings.
/// What: the project directory as listed in the host ceiling, the file
/// path, and the parse result; a parse error refuses only this file.
/// Test: `broken_file_in_one_project_leaves_other_project_effective`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectInput {
    /// The project directory, compared by path components with the
    /// ceiling's `projects` entries.
    pub project_dir: PathBuf,
    /// The route file path, for findings.
    pub file: PathBuf,
    /// The parse result.
    pub parsed: Result<ProjectFile, ProjectFileError>,
}

/// Red-commit stub: an empty, allowed load.
pub fn merge(_host: Result<HostCeiling, HostError>, _projects: Vec<ProjectInput>) -> LoadReport {
    LoadReport {
        policy: ChannelPolicy::default(),
        denied: false,
        findings: Vec::new(),
        per_file: Vec::new(),
    }
}

/// Red-commit stub.
pub(crate) fn attribute(
    err: PolicyError,
    _origins: &HashMap<String, Origin>,
    _fallback: &Path,
) -> Finding {
    Finding::RouteRejected {
        origin: Origin::new(Path::new(""), ""),
        error: err,
    }
}
