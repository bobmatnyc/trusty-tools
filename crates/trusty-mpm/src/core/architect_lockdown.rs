//! The Architect's own `--settings` lockdown and its env scrub (#10127 S5 P3,
//! #8454).
//!
//! Why: the Architect must not hold the chat-family tools or the chat tokens
//! (#8454 boxes 43, 44, 14). The owner-policy carrier
//! ([`crate::core::launch_policy::carrier`]) cannot carry that deny: an
//! Architect dir normally resolves no owner, so that carrier writes no file.
//! What: [`Lockdown`] turns one deny list and one env list into the
//! Architect's argv and env. [`Lockdown::settings_argv`] returns ONE
//! `--settings <json>` pair with `permissions.deny` holding every glob and its
//! [`capitalised`] variant, merged into the owner carrier file when one
//! resolved — Claude Code keeps only the LAST `--settings` flag (Step 0,
//! 2026-10-21), so two flags would drop one payload. A non-Architect profile,
//! or an empty deny list, gets the carrier argv unchanged.
//! [`Lockdown::env_unset`] names the variables an Architect child loses.
//! Fail-closed: every [`LockdownError`] refuses the Architect launch.
//! Test: `crates/trusty-mpm/src/core/architect_lockdown_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use thiserror::Error;

use crate::core::launch_policy::carrier::{SETTINGS_FLAG, settings_argv};
use crate::core::session_profile::SessionProfile;

/// The tool-name globs an Architect launch denies (#10127 Q4).
///
/// Why: owner ruling pending (Architect ruling 2026-10-21, "payload on hold"):
/// the mechanism ships, the list ships EMPTY. The owner's later ruling changes
/// only this line.
/// Test: `the_production_lists_ship_empty`.
pub const ARCHITECT_DENY_GLOBS: &[&str] = &[];

/// The chat-token variables an Architect child does not inherit (#10127 Q3).
///
/// Why: as [`ARCHITECT_DENY_GLOBS`] — Architect-only, shipped EMPTY until the
/// owner rules.
/// Test: `the_production_lists_ship_empty`.
pub const ARCHITECT_ENV_UNSET: &[&str] = &[];

/// Why an Architect launch was refused.
///
/// Test: `a_carrier_file_that_cannot_be_read_refuses_the_architect_launch`,
/// `a_deny_entry_that_is_not_a_tool_glob_refuses_the_launch`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockdownError {
    /// A deny entry is empty, holds whitespace, or holds `(` — Claude Code
    /// skips an `mcp__` rule with `(`, so the entry would deny nothing.
    #[error(
        "Architect lockdown (#10127): deny entry {0:?} is not a tool-name glob; refusing the launch"
    )]
    BadGlob(String),
    /// The owner settings file could not be read to merge the deny into.
    #[error(
        "Architect lockdown (#10127): cannot read the owner settings file {path}: {source}; \
         refusing the launch"
    )]
    ReadCarrier {
        /// The carrier file.
        path: PathBuf,
        /// The read error.
        source: std::io::Error,
    },
    /// The owner settings file is not a JSON object with mergeable keys.
    #[error(
        "Architect lockdown (#10127): owner settings file {path} cannot take the deny: {reason}; \
         refusing the launch"
    )]
    BadCarrier {
        /// The carrier file.
        path: PathBuf,
        /// What was wrong with it.
        reason: String,
    },
    /// The merged payload could not be serialised.
    #[error("Architect lockdown (#10127): cannot serialise the --settings payload: {0}")]
    Serialise(#[from] serde_json::Error),
}

/// One deny list and one env list, applied to an Architect launch only.
///
/// Why: the launch seams take the lists as a value, so a test injects its own
/// list and production uses [`Lockdown::PRODUCTION`] — one source each.
/// Test: `architect_launch_carries_every_glob_and_its_capitalised_variant`.
#[derive(Debug, Clone, Copy)]
pub struct Lockdown<'a> {
    deny_globs: &'a [&'a str],
    env_unset: &'a [&'a str],
}

impl Lockdown<'static> {
    /// The shipped lists: [`ARCHITECT_DENY_GLOBS`] and [`ARCHITECT_ENV_UNSET`].
    pub const PRODUCTION: Self = Self::new(ARCHITECT_DENY_GLOBS, ARCHITECT_ENV_UNSET);
}

impl<'a> Lockdown<'a> {
    /// A lockdown over `deny_globs` and `env_unset`.
    pub const fn new(deny_globs: &'a [&'a str], env_unset: &'a [&'a str]) -> Self {
        Self {
            deny_globs,
            env_unset,
        }
    }

    /// Every deny glob plus its [`capitalised`] variant, sorted, deduplicated.
    ///
    /// What: refuses an entry that is empty, holds whitespace or holds `(`.
    /// Test: `a_deny_entry_that_is_not_a_tool_glob_refuses_the_launch`,
    /// `capitalised_upper_cases_each_word_after_the_mcp_prefix`.
    pub fn deny_rules(&self) -> Result<Vec<String>, LockdownError> {
        let mut rules = std::collections::BTreeSet::new();
        for glob in self.deny_globs {
            if glob.is_empty() || glob.contains('(') || glob.contains(char::is_whitespace) {
                return Err(LockdownError::BadGlob((*glob).to_owned()));
            }
            rules.insert((*glob).to_owned());
            rules.insert(capitalised(glob));
        }
        Ok(rules.into_iter().collect())
    }

    /// The variables a launch of `profile` must not inherit: the env list for
    /// an Architect, nothing otherwise.
    ///
    /// Test: `architect_launch_unsets_the_env_list_and_a_pm_launch_does_not`.
    pub fn env_unset(&self, profile: SessionProfile) -> Vec<String> {
        match profile {
            SessionProfile::Architect => self.env_unset.iter().map(|n| (*n).to_owned()).collect(),
            SessionProfile::Pm => Vec::new(),
        }
    }

    /// The `--settings` argv a launch of `profile` carries, given the owner
    /// carrier file `carrier`.
    ///
    /// Why: Claude Code keeps only the last `--settings` flag, so the deny and
    /// the owner policy must travel as one payload (module doc).
    /// What: a PM, or an empty deny list, gets [`settings_argv`]`(carrier)`
    /// unchanged — no new flag. An Architect with a non-empty list gets
    /// `["--settings", <json>]`: the carrier file's object (or `{}`) with every
    /// [`Self::deny_rules`] entry appended to `permissions.deny`.
    /// Test: `architect_launch_carries_every_glob_and_its_capitalised_variant`,
    /// `architect_lockdown_merges_into_the_owner_carrier_file`,
    /// `a_carrier_file_that_cannot_be_read_refuses_the_architect_launch`,
    /// `an_empty_deny_list_leaves_the_architect_argv_unchanged`.
    pub fn settings_argv(
        &self,
        profile: SessionProfile,
        carrier: Option<&Path>,
    ) -> Result<Vec<String>, LockdownError> {
        if !profile.is_architect() || self.deny_globs.is_empty() {
            return Ok(settings_argv(carrier));
        }
        let rules = self.deny_rules()?;
        let mut body = match carrier {
            Some(path) => read_object(path)?,
            None => Map::new(),
        };
        merge_deny(&mut body, rules).map_err(|reason| LockdownError::BadCarrier {
            path: carrier.map(Path::to_path_buf).unwrap_or_default(),
            reason,
        })?;
        let json = serde_json::to_string(&Value::Object(body))?;
        Ok(vec![SETTINGS_FLAG.to_owned(), json])
    }
}

/// `glob` with the first letter of each word upper-cased, the `mcp__` prefix
/// kept (#10127 Q5: deny-glob case sensitivity is unverified).
///
/// What: a word is a run of ASCII letters; `mcp__*slack*` gives
/// `mcp__*Slack*`.
/// Test: `capitalised_upper_cases_each_word_after_the_mcp_prefix`.
pub fn capitalised(glob: &str) -> String {
    let (prefix, rest) = match glob.strip_prefix("mcp__") {
        Some(rest) => ("mcp__", rest),
        None => ("", glob),
    };
    let mut out = String::with_capacity(glob.len());
    out.push_str(prefix);
    let mut in_word = false;
    for c in rest.chars() {
        let letter = c.is_ascii_alphabetic();
        out.push(if letter && !in_word {
            c.to_ascii_uppercase()
        } else {
            c
        });
        in_word = letter;
    }
    out
}

/// The JSON object in `path`.
fn read_object(path: &Path) -> Result<Map<String, Value>, LockdownError> {
    let text = std::fs::read_to_string(path).map_err(|source| LockdownError::ReadCarrier {
        path: path.to_path_buf(),
        source,
    })?;
    let bad = |reason: String| LockdownError::BadCarrier {
        path: path.to_path_buf(),
        reason,
    };
    match serde_json::from_str(&text).map_err(|e| bad(e.to_string()))? {
        Value::Object(map) => Ok(map),
        _ => Err(bad("not a JSON object".to_owned())),
    }
}

/// Append each of `rules` missing from `body.permissions.deny`.
fn merge_deny(body: &mut Map<String, Value>, rules: Vec<String>) -> Result<(), String> {
    let permissions = body
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("`permissions` is not an object")?;
    let deny = permissions
        .entry("deny")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("`permissions.deny` is not an array")?;
    for rule in rules {
        let rule = Value::String(rule);
        if !deny.contains(&rule) {
            deny.push(rule);
        }
    }
    Ok(())
}

/// Why the typed GUI spawn line refuses an Architect (#10127 Q2).
///
/// Why: that line is re-split by the pane shell under a 1024-byte limit and
/// cannot carry the lockdown payload; refusing is an error, never a silent
/// launch without the lockdown.
/// Test: `gui_spawn_command_refuses_an_architect`.
pub const TYPED_LINE_REFUSAL: &str = "refusing to launch the Architect through the typed GUI \
     spawn line: it cannot carry the Architect lockdown --settings payload (#10127); launch the \
     Architect with `tm fleet init`";

#[cfg(test)]
#[path = "architect_lockdown_tests.rs"]
mod tests;
