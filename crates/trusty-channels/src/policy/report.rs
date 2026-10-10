//! What a route load produced: the effective policy, every finding, and
//! each project file's state.
//!
//! Why: a dropped route must never vanish silently, and a caller must not be
//! able to forget a deny (#8454 plan §3.4, §3.5). The report always carries
//! a [`ChannelPolicy`]; on a deny it is the empty one.
//! What: [`LoadReport`], [`Finding`] (each with a [`FindingScope`]),
//! [`FileStatus`], [`FileState`] and [`Origin`].
//! Test: `src/policy/tests/merge.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::gchat::routes::Connection;
use crate::policy::error::PolicyError;
use crate::policy::host::HostError;
use crate::policy::project_file::ProjectFileError;
use crate::policy::table::ChannelPolicy;
use crate::policy::types::Channel;

/// The result of merging the host ceiling with the project route files.
///
/// Why: one value holds the policy to enforce and the reasons for every
/// route that did not make it, so `tm doctor` and the consumers read the
/// same thing (#8454 plan §3.5).
/// What: `policy` is empty whenever `denied` is true. `findings` name every
/// dropped route or refused file; `per_file` has one entry per input file,
/// in input order.
/// Test: `host_unknown_key_denies_all`, `overlap_across_files_names_both_files`,
/// `broken_file_in_one_project_leaves_other_project_effective`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LoadReport {
    /// The routes in effect. Empty when `denied`.
    pub policy: ChannelPolicy,
    /// True when the whole load was denied: a host-ceiling fault or a
    /// cross-file overlap.
    pub denied: bool,
    /// Every dropped route, refused file and deny, in the order found.
    pub findings: Vec<Finding>,
    /// One status per input project file, in input order.
    pub per_file: Vec<FileStatus>,
}

impl LoadReport {
    /// A placeholder with no route, replaced by the first load.
    pub(crate) fn empty() -> Self {
        Self {
            policy: ChannelPolicy::default(),
            denied: true,
            findings: Vec::new(),
            per_file: Vec::new(),
        }
    }

    /// A denied load: the empty policy, `finding` as the cause, and every
    /// project file `Withheld`.
    pub(crate) fn deny_all(
        finding: Finding,
        mut findings: Vec<Finding>,
        per_file: Vec<FileStatus>,
    ) -> Self {
        findings.push(finding);
        let per_file = per_file
            .into_iter()
            .map(|mut s| {
                if s.state.contributes() {
                    s.state = FileState::Withheld;
                    s.gchat_connection = None;
                    s.gchat_spaces.clear();
                }
                s
            })
            .collect();
        Self {
            policy: ChannelPolicy::default(),
            denied: true,
            findings,
            per_file,
        }
    }
}

/// Where a finding points: a project file and one entry in it.
///
/// Why: an operator fixes a file, so every project-file finding names the
/// file and the entry, never an index into a merged list (#8454 S2a).
/// What: `entry` reads like `slack.routes[0] "bob"` or `gchat.connection`.
/// Test: `overlap_across_files_names_both_files`, `kind_outside_ceiling_fails_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The project route file.
    pub file: PathBuf,
    /// The entry inside it.
    pub entry: String,
}

impl Origin {
    pub(crate) fn new(file: &Path, entry: impl Into<String>) -> Self {
        Self {
            file: file.to_path_buf(),
            entry: entry.into(),
        }
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.file.display(), self.entry)
    }
}

/// How far a finding reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingScope {
    /// The whole load is denied; no route is in effect.
    DenyAll,
    /// One project file is refused; none of its routes are in effect.
    File,
    /// One route is dropped; the rest of its file stands.
    Route,
}

/// One reason a route, a file or the whole load did not take effect.
///
/// Why: every dropped route yields a named finding (#8454 plan §3.4); doctor
/// prints them. No variant carries a credential value.
/// What: each variant names its scope through [`Finding::scope`]. `Stale`
/// comes only from [`crate::policy::PolicyLoader`]'s reload (Architect Q3).
/// Test: `disabled_channel_drops_routes_with_finding`,
/// `unlisted_project_has_no_routes`, `widening_rate_limit_fails_that_file_only`,
/// `overlap_across_files_names_both_files`, `host_faults_deny_all`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Finding {
    /// The host ceiling is missing, malformed or invalid.
    #[error("host ceiling refused: {error}")]
    HostRefused {
        /// Why.
        error: HostError,
    },
    /// A route's channel is absent from the ceiling or disabled.
    #[error("{origin}: channel {channel} is not enabled in the host ceiling; route dropped")]
    ChannelDisabled {
        /// The dropped route.
        origin: Origin,
        /// Its channel.
        channel: Channel,
    },
    /// A route's project is not in its channel's `projects` list.
    #[error("{origin}: project {} is not listed for {channel}; route dropped", project.display())]
    ProjectNotListed {
        /// The dropped route.
        origin: Origin,
        /// Its channel.
        channel: Channel,
        /// The project directory.
        project: PathBuf,
    },
    /// The project file did not parse or failed a file-level rule.
    #[error("{}: file refused: {error}", file.display())]
    FileRefused {
        /// The project route file.
        file: PathBuf,
        /// Why.
        error: ProjectFileError,
    },
    /// A route broke a [`PolicyError`] rule; its file is refused.
    #[error("{origin}: file refused: {error}")]
    RouteRejected {
        /// The offending route.
        origin: Origin,
        /// The rule it broke. Its own entry text indexes the file's routes.
        error: PolicyError,
    },
    /// A route or connection is looser than the host ceiling; its file is
    /// refused.
    #[error("{origin}: file refused: {field} widens the host ceiling: {detail}")]
    Widening {
        /// The offending entry.
        origin: Origin,
        /// `kinds`, `rate_limit` or `gchat.connection`.
        field: &'static str,
        /// What is looser, without any connection value.
        detail: String,
    },
    /// gchat routes are in effect but neither the host nor the file names a
    /// gchat connection; the file is refused.
    #[error("{}: file refused: gchat routes but no gchat connection in the host or the file", file.display())]
    NoGchatConnection {
        /// The project route file.
        file: PathBuf,
    },
    /// Two routes share a name or a (channel, recipient). In one file the
    /// file is refused; across two files the whole load is denied.
    #[error("{first} and {second} share the {field} {value:?}")]
    Overlap {
        /// The earlier route.
        first: Origin,
        /// The later route.
        second: Origin,
        /// `name` or `recipient`.
        field: &'static str,
        /// The shared value.
        value: String,
    },
    /// S2b: a reload failed for this file; its last-good routes stay.
    #[error("{}: reload refused; last-good routes stay in effect", file.display())]
    Stale {
        /// The project route file.
        file: PathBuf,
    },
    /// S2b: the one project a load asked for is not listed for any of the
    /// consumer's enabled channels; it has no routes.
    #[error("project {} is not listed for any enabled channel this load serves", project.display())]
    ProjectUnlisted {
        /// The project directory asked for.
        project: PathBuf,
    },
}

impl Finding {
    /// How far this finding reaches.
    ///
    /// Why: the Q1 ruling: a file fault isolates to its file, a host fault
    /// or a cross-file overlap denies all.
    /// What: an `Overlap` is `DenyAll` when its two origins are in
    /// different files, `File` otherwise.
    /// Test: `overlap_across_files_names_both_files`,
    /// `broken_file_in_one_project_leaves_other_project_effective`.
    pub fn scope(&self) -> FindingScope {
        match self {
            Self::HostRefused { .. } => FindingScope::DenyAll,
            Self::Overlap { first, second, .. } if first.file != second.file => {
                FindingScope::DenyAll
            }
            Self::ChannelDisabled { .. } | Self::ProjectNotListed { .. } => FindingScope::Route,
            Self::FileRefused { .. }
            | Self::Stale { .. }
            | Self::ProjectUnlisted { .. }
            | Self::RouteRejected { .. }
            | Self::Widening { .. }
            | Self::NoGchatConnection { .. }
            | Self::Overlap { .. } => FindingScope::File,
        }
    }
}

/// The state of one project file after the merge.
///
/// Why: doctor shows each file's outcome; S2b adds `Missing` and `Stale`
/// (Architect Q3), defined here so the public enum does not change then.
/// What: only `Effective` contributes routes.
/// Test: `broken_file_in_one_project_leaves_other_project_effective`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileState {
    /// The file loaded; `routes` of its routes are in effect.
    Effective {
        /// Routes this file contributes.
        routes: usize,
    },
    /// The file is refused; its findings say why.
    Refused,
    /// The file was fine but the whole load was denied.
    Withheld,
    /// S2b: no route file exists for this project.
    Missing,
    /// S2b: a reload of this file failed; its last-good routes stay.
    Stale,
}

impl FileState {
    fn contributes(self) -> bool {
        matches!(self, Self::Effective { .. } | Self::Stale)
    }
}

/// One project file's outcome.
///
/// Why: gchat-mcp needs the connection and spaces that go with its routes
/// (Architect Q2); `ChannelPolicy` carries neither.
/// What: `gchat_connection` is the host's when the host names one, else the
/// file's; `gchat_spaces` maps an effective gchat route name to its space.
/// Both are empty unless the file is `Effective`.
/// Test: `v1_gchat_file_loads_unchanged`, `host_connection_is_authoritative`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileStatus {
    /// The project directory.
    pub project_dir: PathBuf,
    /// The project route file.
    pub file: PathBuf,
    /// The outcome.
    pub state: FileState,
    /// The gchat connection in effect for this file's gchat routes.
    pub gchat_connection: Option<Connection>,
    /// Effective gchat route name to `spaces/{id}`, for space routes.
    pub gchat_spaces: BTreeMap<String, String>,
}

impl FileStatus {
    pub(crate) fn new(project_dir: &Path, file: &Path, state: FileState) -> Self {
        Self {
            project_dir: project_dir.to_path_buf(),
            file: file.to_path_buf(),
            state,
            gchat_connection: None,
            gchat_spaces: BTreeMap::new(),
        }
    }
}
