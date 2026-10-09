//! A project's `.trusty-channels/routes.toml`, schema v1 or v2.
//!
//! Why: each project commits its own routes, reviewed in its own repo
//! (#8454 plan §3.2). v1 is today's gchat file and must load unchanged; v2
//! adds Slack and Telegram routes and per-route rate limits. A pre-S2
//! gchat-mcp reads `version = 2` as a version error, so a version skew
//! fails closed.
//! What: [`parse_project_file`] parses text into a [`ProjectFile`] with
//! unknown keys denied at every level. Route names, recipients and kinds are
//! validated later by [`ChannelPolicy::build`](crate::policy::ChannelPolicy::build)
//! inside the merge; this parser checks the version, the gchat connection
//! and each `space`. No file I/O here (S2b).
//! Test: `src/policy/tests/project_file.rs`, `v1_gchat_file_loads_unchanged`.
//!
//! ```toml
//! version = 2
//!
//! [[slack.routes]]
//! name = "bob-dm"
//! recipient = "U0ABCDEF1"
//! kinds = ["question"]
//! rate_limit = { limit = 10, window_secs = 60 }
//! ```

use std::path::Path;

use serde::Deserialize;

use crate::gchat::api::client::is_space_name;
use crate::gchat::routes::{validate_connection, Connection, RawConnection};
use crate::policy::types::{Channel, MessageKind, RateLimitSpec, RouteSpec};

/// Why a project file was refused. Only that project's routes are lost.
///
/// Why: Q1 ruling: a broken project file denies only its own routes.
/// What: parse (including an unknown key), version, or one entry broke a
/// file rule. S2b adds the I/O and load-gate refusals.
/// Test: `project_file_faults_are_refused`, `v2_slack_route_requires_v2`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectFileError {
    /// Not valid TOML for its schema, including an unknown key.
    #[error("invalid routes file: {reason}")]
    Parse {
        /// The TOML parser's message.
        reason: String,
    },
    /// `version` is neither 1 nor 2.
    #[error("routes file version {found} is not supported; expected 1 or 2")]
    Version {
        /// The version found.
        found: i64,
    },
    /// One entry breaks a file rule.
    #[error("{entry}: {reason}")]
    Invalid {
        /// The entry, e.g. `gchat.connection` or `gchat.routes[1] "bob"`.
        entry: String,
        /// What is wrong.
        reason: String,
    },
}

/// One route as written in a project file.
///
/// Why: the merge maps a [`PolicyError`](crate::policy::PolicyError) back to
/// this entry, and gchat space routes keep their space (#9448).
/// What: the S1 spec, the gchat `space` if any, and the entry label.
/// Test: `v1_gchat_file_loads_unchanged`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRoute {
    /// The route as S1 input.
    pub spec: RouteSpec,
    /// gchat only: `spaces/{id}` for a space route.
    pub space: Option<String>,
    /// `<channel>.routes[<i>] "<name>"`, indexed per channel table.
    pub entry: String,
}

/// A parsed project route file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectFile {
    /// 1 or 2.
    pub version: i64,
    /// `[gchat.connection]`, validated, `~/` expanded.
    pub gchat_connection: Option<Connection>,
    /// Routes: gchat, then Slack, then Telegram, each in file order.
    pub routes: Vec<ProjectRoute>,
}

/// Red-commit stub: accepts any text as an empty file.
pub fn parse_project_file(
    _text: &str,
    _home: Option<&Path>,
) -> Result<ProjectFile, ProjectFileError> {
    Ok(ProjectFile::default())
}
