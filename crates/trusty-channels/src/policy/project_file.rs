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

/// Parse and check a project route file's text.
///
/// Why: the strict half of the loader; S2b reads the bytes and runs the
/// load gate, then calls this.
/// What: reads `version` first, then parses the whole text as v1 or v2 with
/// unknown keys denied. v1 is exactly today's gchat schema (a `[gchat]`
/// table needs `[gchat.connection]`); v2 also takes `[[slack.routes]]`,
/// `[[telegram.routes]]`, a per-route `rate_limit`, and an optional
/// `[gchat.connection]` (Architect Q2). Pure: `home` is a parameter.
/// Test: `project_file_faults_are_refused`, `v2_slack_route_requires_v2`,
/// `v1_gchat_file_loads_unchanged`.
pub fn parse_project_file(
    text: &str,
    home: Option<&Path>,
) -> Result<ProjectFile, ProjectFileError> {
    let probe: VersionProbe = toml_parse(text)?;
    match probe.version {
        1 => {
            let raw: RawV1 = toml_parse(text)?;
            let Some(g) = raw.gchat else {
                return Ok(ProjectFile {
                    version: 1,
                    ..ProjectFile::default()
                });
            };
            let connection = connection(g.connection, home)?;
            let routes = g
                .routes
                .into_iter()
                .map(|r| (r.name, r.recipient, r.kinds, r.space, None))
                .collect();
            Ok(ProjectFile {
                version: 1,
                gchat_connection: Some(connection),
                routes: routes_of(Channel::Gchat, routes)?,
            })
        }
        2 => {
            let raw: RawV2 = toml_parse(text)?;
            let mut file = ProjectFile {
                version: 2,
                ..ProjectFile::default()
            };
            if let Some(g) = raw.gchat {
                file.gchat_connection = g.connection.map(|c| connection(c, home)).transpose()?;
                let routes = g
                    .routes
                    .into_iter()
                    .map(|r| (r.name, r.recipient, r.kinds, r.space, r.rate_limit))
                    .collect();
                file.routes = routes_of(Channel::Gchat, routes)?;
            }
            for (c, table) in [
                (Channel::Slack, raw.slack),
                (Channel::Telegram, raw.telegram),
            ] {
                let Some(t) = table else { continue };
                let routes = t
                    .routes
                    .into_iter()
                    .map(|r| (r.name, r.recipient, r.kinds, None, r.rate_limit))
                    .collect();
                file.routes.extend(routes_of(c, routes)?);
            }
            Ok(file)
        }
        found => Err(ProjectFileError::Version { found }),
    }
}

/// (name, recipient, kinds, space, rate_limit) as parsed.
type RawFields = (
    String,
    String,
    Vec<MessageKind>,
    Option<String>,
    Option<RateLimitSpec>,
);

fn routes_of(channel: Channel, raw: Vec<RawFields>) -> Result<Vec<ProjectRoute>, ProjectFileError> {
    raw.into_iter()
        .enumerate()
        .map(|(i, (name, recipient, kinds, space, rate_limit))| {
            let entry = format!("{channel}.routes[{i}] {name:?}");
            // #9448: a configured space passes the same check as a send target.
            if let Some(s) = &space {
                if !is_space_name(s) {
                    return Err(ProjectFileError::Invalid {
                        entry,
                        reason: format!("space {s:?} must be spaces/{{space}}"),
                    });
                }
            }
            Ok(ProjectRoute {
                spec: RouteSpec {
                    channel,
                    name,
                    recipient,
                    kinds,
                    rate_limit,
                },
                space,
                entry,
            })
        })
        .collect()
}

fn connection(raw: RawConnection, home: Option<&Path>) -> Result<Connection, ProjectFileError> {
    validate_connection(raw, home).map_err(|e| ProjectFileError::Invalid {
        entry: "gchat.connection".into(),
        reason: e.to_string(),
    })
}

fn toml_parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, ProjectFileError> {
    toml::from_str(text).map_err(|e| ProjectFileError::Parse {
        reason: e.message().to_string(),
    })
}

/// Reads `version` alone; every other key is checked by the versioned parse.
#[derive(Deserialize)]
struct VersionProbe {
    version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawV1 {
    // Checked by VersionProbe; listed so deny_unknown_fields accepts it.
    #[serde(rename = "version")]
    _version: i64,
    #[serde(default)]
    gchat: Option<RawGchatV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGchatV1 {
    connection: RawConnection,
    #[serde(default)]
    routes: Vec<RawRouteV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRouteV1 {
    name: String,
    recipient: String,
    kinds: Vec<MessageKind>,
    #[serde(default)]
    space: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawV2 {
    // Checked by VersionProbe; listed so deny_unknown_fields accepts it.
    #[serde(rename = "version")]
    _version: i64,
    #[serde(default)]
    gchat: Option<RawGchatV2>,
    #[serde(default)]
    slack: Option<RawTableV2>,
    #[serde(default)]
    telegram: Option<RawTableV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGchatV2 {
    #[serde(default)]
    connection: Option<RawConnection>,
    #[serde(default)]
    routes: Vec<RawGchatRouteV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGchatRouteV2 {
    name: String,
    recipient: String,
    kinds: Vec<MessageKind>,
    #[serde(default)]
    space: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimitSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTableV2 {
    #[serde(default)]
    routes: Vec<RawBotRouteV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBotRouteV2 {
    name: String,
    recipient: String,
    kinds: Vec<MessageKind>,
    #[serde(default)]
    rate_limit: Option<RateLimitSpec>,
}
