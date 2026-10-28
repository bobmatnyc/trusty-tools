//! `channels.slack_read_allowlist`: the projects whose sessions may call a
//! read-only Slack tool through the egress guard (#10387).
//!
//! Why: Bob D6c lets one named project (tm-apex) read Slack channels and
//! threads while every other Slack call stays refused by `tm hook
//! --egress-guard` (#8454). The grant lives in the host ceiling, which only a
//! Bob-approved change may edit and which no session may write (the #8878
//! trust-anchor floor covers `config.yaml`).
//! What: [`parse_allowlist`] validates the raw entries strictly: an absolute
//! project path with no `..`, a display `name` safe to print in an audit
//! line, and a non-empty `tools` list drawn only from [`SLACK_READ_TOOLS`],
//! with any `*` or `?` refused. Any fault is a [`HostError::SlackReadAllowlist`],
//! naming the entry by position and never echoing a value.
//! Test: `src/policy/tests/slack_read.rs`.
//!
//! ```yaml
//! channels:
//!   version: 1
//!   slack_read_allowlist:
//!     - project: /abs/canonical/path/to/tm-apex
//!       name: tm-apex
//!       tools: [slack_read_channel, slack_read_thread]
//! ```

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::gchat::routes::expand_home;
use crate::policy::host::HostError;

/// The only Slack tools an allowlist entry may grant (#10387). Reading a
/// canvas or a file is not on it: both return content a channel read does not.
pub const SLACK_READ_TOOLS: &[&str] = &["slack_read_channel", "slack_read_thread"];

/// The longest display name accepted, in bytes.
const MAX_NAME_BYTES: usize = 64;

/// One validated allowlist entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackReadEntry {
    project: PathBuf,
    name: String,
    tools: Vec<&'static str>,
}

impl SlackReadEntry {
    /// The project directory, `~/` expanded; compared by the guard against
    /// the session's canonical `CLAUDE_PROJECT_DIR`.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The display name the audit line prints.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The granted tools, each one of [`SLACK_READ_TOOLS`].
    pub fn tools(&self) -> &[&'static str] {
        &self.tools
    }
}

/// The validated allowlist; empty when the host file names none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackReadAllowlist {
    entries: Vec<SlackReadEntry>,
}

impl SlackReadAllowlist {
    /// The entry for `project`, compared by path components.
    pub fn entry_for(&self, project: &Path) -> Option<&SlackReadEntry> {
        self.entries.iter().find(|e| e.project == project)
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> &[SlackReadEntry] {
        &self.entries
    }
}

/// One `slack_read_allowlist` entry as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSlackReadEntry {
    project: String,
    name: String,
    tools: Vec<String>,
}

/// Validate the raw entries into a [`SlackReadAllowlist`].
///
/// Why (#10387): a grant wider than two read tools, or one the guard cannot
/// match exactly, must refuse the whole host rather than load partly.
/// What: per entry, in order: `project` non-empty, `~/`-expanded against
/// `home`, absolute, no `..`, not listed twice; `name` 1..=64 bytes of ASCII
/// letters, digits, `.`, `_` or `-`; `tools` non-empty, no `*` or `?`, each
/// one of [`SLACK_READ_TOOLS`], none twice.
/// Test: `slack_read_allowlist_parses_both_tools`,
/// `slack_read_allowlist_refuses_each_bad_entry`,
/// `slack_read_allowlist_refuses_a_wildcard`.
pub(super) fn parse_allowlist(
    raw: Vec<RawSlackReadEntry>,
    home: Option<&Path>,
) -> Result<SlackReadAllowlist, HostError> {
    let mut entries: Vec<SlackReadEntry> = Vec::with_capacity(raw.len());
    for (index, entry) in raw.into_iter().enumerate() {
        let fail = |reason: &'static str| HostError::SlackReadAllowlist { index, reason };
        let project = project_path(&entry.project, home).map_err(fail)?;
        if entries.iter().any(|e| e.project == project) {
            return Err(fail("project is listed twice"));
        }
        if !valid_name(&entry.name) {
            return Err(fail(
                "name must be 1 to 64 ASCII letters, digits, '.', '_' or '-'",
            ));
        }
        if entry.tools.is_empty() {
            return Err(fail("tools is empty"));
        }
        let mut tools = Vec::with_capacity(entry.tools.len());
        for tool in &entry.tools {
            // #10387: a pattern is refused by name before the set check.
            if tool.contains(['*', '?']) {
                return Err(fail("tools holds a wildcard; name each tool exactly"));
            }
            let Some(known) = SLACK_READ_TOOLS.iter().copied().find(|t| t == tool) else {
                return Err(fail(
                    "tools holds a tool that is not slack_read_channel or slack_read_thread",
                ));
            };
            if tools.contains(&known) {
                return Err(fail("tools lists a tool twice"));
            }
            tools.push(known);
        }
        entries.push(SlackReadEntry {
            project,
            name: entry.name,
            tools,
        });
    }
    Ok(SlackReadAllowlist { entries })
}

/// The entry's project path, or why it is refused.
fn project_path(entry: &str, home: Option<&Path>) -> Result<PathBuf, &'static str> {
    if entry.trim().is_empty() {
        return Err("project is empty");
    }
    let path = expand_home(entry, home).ok_or("project starts with ~/ but no home is known")?;
    if !path.is_absolute() {
        return Err("project is not an absolute path");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("project holds a .. component");
    }
    Ok(path)
}

/// True for a display name the audit line can print as is.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
