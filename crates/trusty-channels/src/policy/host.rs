//! The host ceiling: the `channels:` section of trusty-mpm's `config.yaml`.
//!
//! Why: the host file is the root of trust for every channel (#8454 plan
//! §3.1, Bob Daa). It names which channels are on, which projects may define
//! routes for each, the default rate limit, the kind ceiling and the
//! connection refs. A project file can narrow it, never widen it.
//! What: [`parse_host`] takes the file text, reads only the `channels` key,
//! and deserializes it with unknown keys denied at every level. Any fault is
//! a [`HostError`], which denies every route. No file I/O here (S2b).
//! Test: `src/policy/tests/host.rs`.
//!
//! ```yaml
//! channels:
//!   version: 1
//!   rate_limit: { limit: 100, window_secs: 60 }
//!   gchat:
//!     enabled: true
//!     connection: { project_id: p, subscription: s, key_file: ~/sa.json }
//!     kinds: [question, review_notice]
//!     projects: [/Users/me/proj]
//!   slack: { enabled: true, connection: { credential_ref: slack }, projects: [/abs] }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::gchat::routes::{expand_home, validate_connection, Connection, RawConnection};
use crate::policy::types::{Channel, MessageKind, RateLimit, RateLimitSpec};

/// The only `channels.version` this parser reads.
pub const HOST_SCHEMA_VERSION: i64 = 1;

/// Why the host ceiling was refused. Any of these denies every route.
///
/// Why: the host file is the master switch; a fault there must not leave
/// any route in effect (#8454 Q1).
/// What: one variant per rule. No variant carries a credential value.
/// Test: `host_faults_deny_all`, `host_unknown_key_denies_all`,
/// `no_channels_section_denies_all`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostError {
    /// The text is not YAML, or its top level is not a mapping.
    #[error("config.yaml is malformed: {reason}")]
    Malformed {
        /// The parser's message, any quoted value withheld.
        reason: String,
    },
    /// The file has no top-level `channels` key.
    #[error("config.yaml has no channels section; every channel is denied")]
    NoChannelsSection,
    /// The `channels` section has an unknown key, a wrong type or a missing
    /// field.
    #[error("channels section is invalid: {reason}")]
    Invalid {
        /// The parser's message, any quoted value withheld.
        reason: String,
    },
    /// `channels.version` is not [`HOST_SCHEMA_VERSION`].
    #[error("channels.version {found} is not supported; expected 1")]
    Version {
        /// The version found.
        found: i64,
    },
    /// `channels.rate_limit` is out of range.
    #[error("channels.rate_limit: {reason}")]
    RateLimit {
        /// What is wrong.
        reason: String,
    },
    /// A channel's `kinds` ceiling is empty or lists a kind it cannot carry.
    #[error("channels.{channel}.kinds: {reason}")]
    Kinds {
        /// The channel.
        channel: Channel,
        /// What is wrong.
        reason: String,
    },
    /// A `projects` entry is empty, relative, holds `..`, or needs a home
    /// directory that is not known. The entry is named by position only
    /// (#8454): a token typed into the list must not reach a log.
    #[error("channels.{channel}.projects[{index}]: {reason}")]
    Project {
        /// The channel.
        channel: Channel,
        /// The entry's zero-based position in the list.
        index: usize,
        /// What is wrong.
        reason: String,
    },
    /// The gchat connection is malformed.
    #[error("channels.gchat.connection: {reason}")]
    Connection {
        /// What is wrong.
        reason: String,
    },
    /// A `credential_ref` names something other than the channel's bot or
    /// app credential. The value is not echoed: a pasted token must not
    /// reach a log.
    #[error(
        "channels.{channel}.connection.credential_ref is not allowed; expected one of {allowed:?}"
    )]
    CredentialRef {
        /// The channel.
        channel: Channel,
        /// The names allowed for this channel.
        allowed: &'static [&'static str],
    },
}

/// The validated host ceiling.
///
/// Why: fields are private so a ceiling exists only as the output of
/// [`parse_host`].
/// What: the default rate limit and the channels present in the file.
/// Test: `host_ceiling_parses_every_field`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCeiling {
    rate_limit: RateLimit,
    channels: BTreeMap<Channel, HostChannel>,
}

impl HostCeiling {
    /// The default rate limit; a route may lower it, never raise it.
    pub fn rate_limit(&self) -> RateLimit {
        self.rate_limit
    }

    /// The channel's ceiling; `None` when the file does not name it.
    pub fn channel(&self, channel: Channel) -> Option<&HostChannel> {
        self.channels.get(&channel)
    }
}

/// One channel's ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostChannel {
    enabled: bool,
    kinds: BTreeSet<MessageKind>,
    projects: Vec<PathBuf>,
    gchat_connection: Option<Connection>,
    credential_ref: Option<String>,
}

impl HostChannel {
    /// False means every route on this channel is dropped.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The kinds a route may list; the channel's own kinds when unset.
    pub fn kinds(&self) -> &BTreeSet<MessageKind> {
        &self.kinds
    }

    /// The project directories that may define routes, `~/` expanded.
    pub fn projects(&self) -> &[PathBuf] {
        &self.projects
    }

    /// True when `project` is listed, compared by path components.
    pub fn lists(&self, project: &Path) -> bool {
        self.projects.iter().any(|p| p == project)
    }

    /// gchat only: the authoritative connection, when the host names one.
    pub fn gchat_connection(&self) -> Option<&Connection> {
        self.gchat_connection.as_ref()
    }

    /// Slack and Telegram: the credential name, never its value.
    pub fn credential_ref(&self) -> Option<&str> {
        self.credential_ref.as_deref()
    }
}

/// The `credential_ref` names each channel accepts: a bot or app-level
/// token only, never a user token (#8454 ruling 2026-09-23). Names come from
/// `trusty_common::credential_registry::REGISTRY`.
fn allowed_refs(channel: Channel) -> &'static [&'static str] {
    match channel {
        Channel::Slack => &["slack", "slack-app"],
        Channel::Telegram => &["telegram"],
        Channel::Gchat => &[],
    }
}

/// Parse and validate the host ceiling from `config.yaml` text.
///
/// Why: a separate strict parse of the same file the lenient
/// `TrustyToolsConfig` loader reads (#8454 plan §4, Architect Q8): that
/// loader returns a default on error, which must never apply to routes.
/// What: YAML to a value, take only `channels`, deserialize it with unknown
/// keys denied at every level, then check `version`, the rate limit, each
/// channel's kinds, projects (absolute after `~/` expansion against `home`,
/// no `..`), gchat connection and credential ref. Other top-level keys are
/// ignored. Pure: `home` is a parameter.
/// Test: `host_faults_deny_all`, `host_unknown_key_denies_all`,
/// `no_channels_section_denies_all`, `host_ceiling_parses_every_field`.
pub fn parse_host(text: &str, home: Option<&Path>) -> Result<HostCeiling, HostError> {
    // #8454: both serde_yaml messages pass through withhold_values.
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| HostError::Malformed {
            reason: withhold_values(&e),
        })?;
    let section = match value {
        serde_yaml::Value::Null => return Err(HostError::NoChannelsSection),
        serde_yaml::Value::Mapping(mut map) => {
            map.remove("channels").ok_or(HostError::NoChannelsSection)?
        }
        _ => {
            return Err(HostError::Malformed {
                reason: "the top level is not a mapping".into(),
            })
        }
    };
    let raw: RawHost = serde_yaml::from_value(section).map_err(|e| HostError::Invalid {
        reason: withhold_values(&e),
    })?;
    if raw.version != HOST_SCHEMA_VERSION {
        return Err(HostError::Version { found: raw.version });
    }
    let rate_limit = match raw.rate_limit {
        None => RateLimit::DEFAULT,
        Some(spec) => {
            RateLimit::from_spec(spec).map_err(|reason| HostError::RateLimit { reason })?
        }
    };
    let mut channels = BTreeMap::new();
    if let Some(g) = raw.gchat {
        let connection = g
            .connection
            .map(|c| validate_connection(c, home))
            .transpose()
            .map_err(|e| HostError::Connection {
                reason: e.to_string(),
            })?;
        let common = RawCommon {
            enabled: g.enabled,
            kinds: g.kinds,
            projects: g.projects,
        };
        let ch = channel(Channel::Gchat, common, home)?;
        channels.insert(
            Channel::Gchat,
            HostChannel {
                gchat_connection: connection,
                ..ch
            },
        );
    }
    for (c, raw_ch) in [
        (Channel::Slack, raw.slack),
        (Channel::Telegram, raw.telegram),
    ] {
        let Some(b) = raw_ch else { continue };
        let credential_ref = match b.connection {
            None => None,
            Some(conn) => {
                let allowed = allowed_refs(c);
                if !allowed.contains(&conn.credential_ref.as_str()) {
                    return Err(HostError::CredentialRef {
                        channel: c,
                        allowed,
                    });
                }
                Some(conn.credential_ref)
            }
        };
        let common = RawCommon {
            enabled: b.enabled,
            kinds: b.kinds,
            projects: b.projects,
        };
        let ch = channel(c, common, home)?;
        channels.insert(
            c,
            HostChannel {
                credential_ref,
                ..ch
            },
        );
    }
    Ok(HostCeiling {
        rate_limit,
        channels,
    })
}

/// serde messages whose `, expected …` tail is written by this code's types.
const EXPECTING: [&str; 5] = [
    "invalid type: ",
    "invalid value: ",
    "invalid length ",
    "unknown variant ",
    "unknown field ",
];

/// A serde_yaml message from host input, with every value it quotes withheld.
///
/// Why: serde quotes the value it could not read (`invalid type: string
/// "xoxb-…"`), so a token typed into the wrong host key would reach a
/// `HostError` and every finding built from it (#8454).
/// What: `missing field` and `duplicate field` name a field of this code's
/// types and stay. Every duplicate-key message becomes a fixed text with its
/// position, quoted or not. A message with a code-written `, expected …`
/// tail keeps the tail and replaces the span from its first to its last
/// quote mark (`"` or `` ` ``) before it. Any other message that quotes
/// something (a key path) becomes a fixed text with its position. A message
/// that quotes nothing, such as a libyaml syntax error, stays.
/// Test: `host_faults_deny_all`, `host_unknown_key_denies_all`.
fn withhold_values(e: &serde_yaml::Error) -> String {
    let msg = e.to_string();
    if msg.starts_with("missing field `") || msg.starts_with("duplicate field `") {
        return msg;
    }
    let withheld = |what: &str| {
        let at = e
            .location()
            .map(|l| format!(" at line {} column {}", l.line(), l.column()))
            .unwrap_or_default();
        format!("{what}{at} (value withheld)")
    };
    // #8454: a number, null or collection key is printed unquoted, and the
    // key-path prefix can carry input text, so no duplicate keeps its text.
    if msg.contains("duplicate entry ") {
        return withheld("a mapping repeats a key");
    }
    let (head, tail) = match msg.rfind(", expected ") {
        Some(i) if EXPECTING.iter().any(|p| msg.starts_with(p)) => msg.split_at(i),
        _ => (msg.as_str(), ""),
    };
    let quote = |c: char| c == '"' || c == '`';
    let (Some(first), Some(last)) = (head.find(quote), head.rfind(quote)) else {
        return msg;
    };
    if tail.is_empty() {
        return withheld("a quoted value is invalid");
    }
    // Quote marks are ASCII, so `last + 1` is a char boundary.
    format!(
        "{}<value withheld>{}{tail}",
        &head[..first],
        &head[last + 1..]
    )
}

fn channel(c: Channel, raw: RawCommon, home: Option<&Path>) -> Result<HostChannel, HostError> {
    let kinds = match raw.kinds {
        None => MessageKind::ALL
            .into_iter()
            .filter(|k| c.allows_kind(*k))
            .collect(),
        Some(list) => {
            let kind_err = |reason: String| HostError::Kinds { channel: c, reason };
            if list.is_empty() {
                return Err(kind_err(
                    "empty; use enabled: false to turn the channel off".into(),
                ));
            }
            if let Some(k) = list.iter().find(|k| !c.allows_kind(**k)) {
                return Err(kind_err(format!("{k} is not carried on {c}")));
            }
            list.into_iter().collect()
        }
    };
    let projects = raw
        .projects
        .into_iter()
        .enumerate()
        .map(|(index, entry)| project_entry(c, index, &entry, home))
        .collect::<Result<_, _>>()?;
    Ok(HostChannel {
        enabled: raw.enabled,
        kinds,
        projects,
        gchat_connection: None,
        credential_ref: None,
    })
}

/// #8454 plan §4: a bad `projects` entry is a ceiling fault, never skipped.
fn project_entry(
    c: Channel,
    index: usize,
    entry: &str,
    home: Option<&Path>,
) -> Result<PathBuf, HostError> {
    // #8454: the error names the entry's position; no reason repeats it.
    let fail = |reason: &str| HostError::Project {
        channel: c,
        index,
        reason: reason.into(),
    };
    if entry.trim().is_empty() {
        return Err(fail("is empty"));
    }
    let Some(path) = expand_home(entry, home) else {
        return Err(fail("starts with ~/ but no home directory is known"));
    };
    if !path.is_absolute() {
        return Err(fail("is not an absolute path"));
    }
    if path.components().any(|p| matches!(p, Component::ParentDir)) {
        return Err(fail("holds a .. component"));
    }
    Ok(path)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHost {
    version: i64,
    #[serde(default)]
    rate_limit: Option<RateLimitSpec>,
    #[serde(default)]
    gchat: Option<RawGchatChannel>,
    #[serde(default)]
    slack: Option<RawBotChannel>,
    #[serde(default)]
    telegram: Option<RawBotChannel>,
}

/// The keys every channel shares, moved out of its raw struct.
struct RawCommon {
    enabled: bool,
    kinds: Option<Vec<MessageKind>>,
    projects: Vec<String>,
}

// `deny_unknown_fields` does not combine with `flatten`, so each channel
// struct lists the common keys itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGchatChannel {
    enabled: bool,
    #[serde(default)]
    kinds: Option<Vec<MessageKind>>,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    connection: Option<RawConnection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBotChannel {
    enabled: bool,
    #[serde(default)]
    kinds: Option<Vec<MessageKind>>,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    connection: Option<RawBotConnection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBotConnection {
    credential_ref: String,
}
