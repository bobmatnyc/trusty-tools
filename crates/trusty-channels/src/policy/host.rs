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
        /// The parser's message.
        reason: String,
    },
    /// The file has no top-level `channels` key.
    #[error("config.yaml has no channels section; every channel is denied")]
    NoChannelsSection,
    /// The `channels` section has an unknown key, a wrong type or a missing
    /// field.
    #[error("channels section is invalid: {reason}")]
    Invalid {
        /// The parser's message.
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
    /// directory that is not known.
    #[error("channels.{channel}.projects entry {entry:?}: {reason}")]
    Project {
        /// The channel.
        channel: Channel,
        /// The entry as written.
        entry: String,
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

/// Red-commit stub: accepts any text as an empty ceiling.
pub fn parse_host(_text: &str, _home: Option<&Path>) -> Result<HostCeiling, HostError> {
    Ok(HostCeiling {
        rate_limit: RateLimit::DEFAULT,
        channels: BTreeMap::new(),
    })
}
