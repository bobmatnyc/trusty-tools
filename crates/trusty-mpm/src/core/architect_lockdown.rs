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
//! [`capitalised`] variant and `disableClaudeAiConnectors: true`, merged into
//! the owner carrier file when one resolved — Claude Code keeps only the LAST
//! `--settings` flag (Step 0, 2026-10-21), so two flags would drop one payload.
//! A non-Architect profile, or an empty deny list, gets the carrier argv
//! unchanged. [`Lockdown::env_unset`] names the variables an Architect child
//! loses. Every launch seam goes through [`Lockdown::apply_to_spec`],
//! [`Lockdown::apply_to_command`] or [`Lockdown::settings_file_argv`].
//! Fail-closed: every [`LockdownError`] refuses the Architect launch.
//! Test: `crates/trusty-mpm/src/core/architect_lockdown_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use thiserror::Error;

use trusty_common::credential_registry::REGISTRY;

use crate::core::launch_policy::carrier::{SETTINGS_FLAG, settings_argv};
use crate::core::session_profile::SessionProfile;
use crate::runtime::launch_spec::LaunchSpec;

/// The settings key that turns off the claude.ai connector tools (#10127 §1a).
pub const DISABLE_CLAUDE_AI_CONNECTORS: &str = "disableClaudeAiConnectors";

/// The tool-name globs an Architect launch denies (#10127 D24 = B: the plan's
/// full list, `s5-p3-plan.md` §1a).
///
/// Why: #8454 boxes 43 and 44 — the Architect holds no Slack, Gmail, Notion or
/// Google Chat tool, and no Google Workspace mail, chat, message or draft tool.
/// What: one `permissions.deny` rule per entry, each with no `(` (Claude Code
/// skips such an `mcp__` rule); [`Lockdown::deny_rules`] adds the
/// [`capitalised`] variant. The gworkspace braces are four plain globs.
/// Test: `the_production_deny_list_is_the_plans_full_list`,
/// `the_family_deny_never_matches_ask_owner_or_the_trusty_mpm_tools`,
/// `the_architect_deny_covers_every_egress_family`.
pub const ARCHITECT_DENY_GLOBS: &[&str] = &[
    "mcp__*slack*",
    "mcp__*gmail*",
    "mcp__*googlemail*",
    "mcp__*notion*",
    "mcp__*googlechat*",
    "mcp__*google-chat*",
    "mcp__*gchat*",
    "mcp__*gworkspace*__*mail*",
    "mcp__*gworkspace*__*chat*",
    "mcp__*gworkspace*__*message*",
    "mcp__*gworkspace*__*draft*",
];

/// The provider-key families whose credentials are chat tokens (#10127 Q3).
///
/// What: a `REGISTRY` provider key is a chat channel when it equals a family
/// or starts with `<family>-` (`slack-user`, `slack-app`).
const CHAT_CHANNEL_FAMILIES: &[&str] = &["slack", "telegram"];

/// How many `REGISTRY` entries are chat-channel credentials.
const CHAT_TOKEN_COUNT: usize = count_chat_tokens(REGISTRY);

/// The chat-channel credential variables, in `REGISTRY` order.
const CHAT_TOKENS: [&str; CHAT_TOKEN_COUNT] = chat_tokens(REGISTRY);

/// The chat-token variables an Architect child does not inherit (#10127 Q3,
/// #8454 box 14).
///
/// Why: derived from the credential `REGISTRY`, never hand-copied, so a new
/// Slack or Telegram credential cannot be forgotten.
/// What: `SLACK_BOT_TOKEN`, `SLACK_USER_TOKEN`, `SLACK_APP_TOKEN`,
/// `TELEGRAM_BOT_TOKEN` at this base ([`chat_tokens`]).
/// Test: `architect_env_unset_is_every_chat_token_in_the_registry`,
/// `chat_tokens_follow_a_new_registry_entry`.
pub const ARCHITECT_ENV_UNSET: &[&str] = &CHAT_TOKENS;

/// Whether `key` names a [`CHAT_CHANNEL_FAMILIES`] provider.
const fn is_chat_provider(key: &str) -> bool {
    let mut i = 0;
    while i < CHAT_CHANNEL_FAMILIES.len() {
        let family = CHAT_CHANNEL_FAMILIES[i].as_bytes();
        let key = key.as_bytes();
        if key.len() >= family.len()
            && starts_with(key, family)
            && (key.len() == family.len() || key[family.len()] == b'-')
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether `bytes` begins with `prefix` (a `const` `starts_with`).
const fn starts_with(bytes: &[u8], prefix: &[u8]) -> bool {
    let mut i = 0;
    while i < prefix.len() {
        if bytes[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// How many entries of `registry` are chat-channel credentials.
pub(crate) const fn count_chat_tokens(registry: &[(&str, &str)]) -> usize {
    let (mut i, mut n) = (0, 0);
    while i < registry.len() {
        if is_chat_provider(registry[i].0) {
            n += 1;
        }
        i += 1;
    }
    n
}

/// The variable of every chat-channel entry of `registry`, in order; `N` is
/// [`count_chat_tokens`]`(registry)`.
pub(crate) const fn chat_tokens<const N: usize>(
    registry: &[(&'static str, &'static str)],
) -> [&'static str; N] {
    let mut out = [""; N];
    let (mut i, mut j) = (0, 0);
    while i < registry.len() && j < N {
        if is_chat_provider(registry[i].0) {
            out[j] = registry[i].1;
            j += 1;
        }
        i += 1;
    }
    out
}

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
    /// The payload file a typed launch line names could not be written.
    #[error(
        "Architect lockdown (#10127): cannot write the --settings file {path}: {source}; \
         refusing the launch"
    )]
    WriteSettings {
        /// The payload file.
        path: PathBuf,
        /// The write error.
        source: std::io::Error,
    },
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
    /// [`Self::deny_rules`] entry appended to `permissions.deny` and
    /// [`DISABLE_CLAUDE_AI_CONNECTORS`] set `true`.
    /// Test: `architect_launch_carries_every_glob_and_its_capitalised_variant`,
    /// `architect_lockdown_merges_into_the_owner_carrier_file`,
    /// `a_carrier_file_that_cannot_be_read_refuses_the_architect_launch`,
    /// `an_empty_deny_list_leaves_the_architect_argv_unchanged`.
    pub fn settings_argv(
        &self,
        profile: SessionProfile,
        carrier: Option<&Path>,
    ) -> Result<Vec<String>, LockdownError> {
        match self.payload(profile, carrier)? {
            Some(json) => Ok(vec![SETTINGS_FLAG.to_owned(), json]),
            None => Ok(settings_argv(carrier)),
        }
    }

    /// The merged `--settings` JSON for an Architect under a non-empty deny
    /// list; `None` when the carrier argv applies unchanged.
    fn payload(
        &self,
        profile: SessionProfile,
        carrier: Option<&Path>,
    ) -> Result<Option<String>, LockdownError> {
        if !profile.is_architect() || self.deny_globs.is_empty() {
            return Ok(None);
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
        // #10127 §1a: the claude.ai connector tools go too.
        body.insert(DISABLE_CLAUDE_AI_CONNECTORS.to_owned(), Value::Bool(true));
        Ok(Some(serde_json::to_string(&Value::Object(body))?))
    }

    /// [`Self::settings_argv`] for a line a shell re-splits (#10127 Q2).
    ///
    /// Why: the typed GUI spawn line cannot carry the JSON inline under the
    /// pane's byte limit, so the payload travels as a file it names.
    /// What: when the lockdown applies, writes the payload to `out` (mode
    /// 0600) and returns `["--settings", out]`; otherwise
    /// [`settings_argv`]`(carrier)`. A write failure is
    /// [`LockdownError::WriteSettings`].
    /// Test: `gui_spawn_line_carries_the_architect_lockdown`,
    /// `an_unwritable_lockdown_file_refuses_the_gui_architect_line`.
    pub fn settings_file_argv(
        &self,
        profile: SessionProfile,
        carrier: Option<&Path>,
        out: &Path,
    ) -> Result<Vec<String>, LockdownError> {
        let Some(json) = self.payload(profile, carrier)? else {
            return Ok(settings_argv(carrier));
        };
        write_private(out, &json).map_err(|source| LockdownError::WriteSettings {
            path: out.to_path_buf(),
            source,
        })?;
        Ok(settings_argv(Some(out)))
    }

    /// Append the lockdown's `--settings` argv to `spec` and drop its env
    /// variables (#10127).
    ///
    /// Why: one call per [`LaunchSpec`] launch seam, so no seam can carry the
    /// deny and forget the env scrub. `carrier` is the owner settings file this
    /// launch provisioned; the caller adds no `--settings` of its own.
    /// What: [`Self::settings_argv`] appended to `spec.args`, then
    /// [`Self::scrub_spec_env`].
    /// Test: `every_architect_spec_seam_carries_the_lockdown`.
    pub fn apply_to_spec(
        &self,
        profile: SessionProfile,
        carrier: Option<&Path>,
        spec: &mut LaunchSpec,
    ) -> Result<(), LockdownError> {
        spec.args.extend(self.settings_argv(profile, carrier)?);
        self.scrub_spec_env(profile, spec);
        Ok(())
    }

    /// Unset [`Self::env_unset`] in `spec`, and drop any assignment of it.
    ///
    /// Test: `architect_launch_unsets_the_env_list_and_a_pm_launch_does_not`.
    pub fn scrub_spec_env(&self, profile: SessionProfile, spec: &mut LaunchSpec) {
        let unset = self.env_unset(profile);
        spec.env_set.retain(|(name, _)| !unset.contains(name));
        spec.env_unset.extend(unset);
    }

    /// [`Self::apply_to_spec`] for a [`std::process::Command`]: the argv
    /// appended, each [`Self::env_unset`] variable removed.
    ///
    /// Test: `run_launch_command_carries_the_architect_lockdown`.
    pub fn apply_to_command(
        &self,
        profile: SessionProfile,
        carrier: Option<&Path>,
        cmd: &mut std::process::Command,
    ) -> Result<(), LockdownError> {
        cmd.args(self.settings_argv(profile, carrier)?);
        for name in self.env_unset(profile) {
            cmd.env_remove(name);
        }
        Ok(())
    }
}

/// Write `text` to `path`, readable by the owner only.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(text.as_bytes())
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

/// Assert `args` carries the production deny as ONE inline `--settings`
/// (#10127 seam tests).
#[cfg(test)]
pub(crate) fn assert_production_deny(args: &[String]) {
    let values: Vec<&String> = args
        .windows(2)
        .filter(|w| w[0] == SETTINGS_FLAG)
        .map(|w| &w[1])
        .collect();
    assert_eq!(values.len(), 1, "one --settings: {args:?}");
    let body: Value = serde_json::from_str(values[0]).expect("an inline JSON payload");
    let deny = body["permissions"]["deny"]
        .as_array()
        .expect("permissions.deny");
    for glob in ARCHITECT_DENY_GLOBS {
        for rule in [(*glob).to_owned(), capitalised(glob)] {
            assert!(
                deny.contains(&Value::String(rule.clone())),
                "{rule} in {deny:?}"
            );
        }
    }
    assert_eq!(body[DISABLE_CLAUDE_AI_CONNECTORS], true, "{body}");
}

/// Assert `env_unset` names every production chat token (#10127 seam tests).
#[cfg(test)]
pub(crate) fn assert_chat_tokens_unset(env_unset: &[String]) {
    for name in ARCHITECT_ENV_UNSET {
        assert!(
            env_unset.iter().any(|n| n == name),
            "{name} unset: {env_unset:?}"
        );
    }
}

#[cfg(test)]
#[path = "architect_lockdown_tests.rs"]
mod tests;
