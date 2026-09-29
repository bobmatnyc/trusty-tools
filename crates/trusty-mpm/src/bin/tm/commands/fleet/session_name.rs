//! The Architect's tmux session names for `tm fleet init|status` (#8878 R1).
//!
//! Why: owner ruling 2026-09-29 22:23Z (R1) lets the running supervisor keep
//! its own tmux names. `init --session <name>` must pick them once, and every
//! later `init` and `status` without the flag must find the same names, or a
//! re-run would start a second Architect as `tm-architect`.
//! What: [`SessionNames`] holds a validated Architect session and its poller
//! session, always `<name>-poll`. The chosen name is recorded as `[supervisor]
//! session` in `~/.trusty-mpm/config.toml`, the file that already holds the
//! Architect's allowlist grant and that only the Architect may write
//! (#8878 trust anchors). No key means `tm-architect`, so a run without the
//! flag writes nothing new. [`recorded`] reads the key, failing on a value
//! that is not a valid name; [`record_name`] writes it.
//! Test: `session_tests.rs`.

use std::path::Path;

use anyhow::{Context, bail};
use toml_edit::DocumentMut;
use trusty_mpm::core::architect_session::{poll_session_name, validate_session_name};

use super::config::{Edit, parse_user_config};
use super::{ARCHITECT_SESSION, user_config_path};

/// The `[supervisor]` key holding the chosen Architect session name.
pub(crate) const SESSION_KEY: &str = "session";

/// A validated Architect session name and its poller session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionNames {
    architect: String,
    poll: String,
}

impl SessionNames {
    /// `name` and `<name>-poll`, or the refusal of
    /// [`validate_session_name`].
    ///
    /// Test: `a_bad_session_flag_is_refused_before_any_write`.
    pub(crate) fn new(name: &str) -> anyhow::Result<Self> {
        validate_session_name(name).map_err(|why| anyhow::anyhow!("invalid --session: {why}"))?;
        Ok(Self {
            architect: name.to_owned(),
            poll: poll_session_name(name),
        })
    }

    /// `tm-architect` and `tm-architect-poll`.
    pub(crate) fn default_names() -> Self {
        Self {
            architect: ARCHITECT_SESSION.to_owned(),
            poll: poll_session_name(ARCHITECT_SESSION),
        }
    }

    /// The Architect's tmux session.
    pub(crate) fn architect(&self) -> &str {
        &self.architect
    }

    /// The poller's tmux session.
    pub(crate) fn poll(&self) -> &str {
        &self.poll
    }
}

/// The session name `[supervisor] session` records in the parsed user config.
///
/// What: `None` when the table or key is absent. A key that is not a string,
/// or not a valid name, is an error naming `path`, so a caller never falls
/// back to `tm-architect` over a value it could not read.
/// Test: `a_malformed_recorded_name_refuses_init_and_fails_status`.
pub(crate) fn recorded(doc: &DocumentMut, path: &Path) -> anyhow::Result<Option<String>> {
    let Some(table) = doc.get("supervisor").and_then(|t| t.as_table_like()) else {
        return Ok(None);
    };
    let Some(item) = table.get(SESSION_KEY) else {
        return Ok(None);
    };
    let name = item.as_str().with_context(|| {
        format!(
            "`supervisor.{SESSION_KEY}` in {} is not a string",
            path.display()
        )
    })?;
    validate_session_name(name).map_err(|why| {
        anyhow::anyhow!("`supervisor.{SESSION_KEY}` in {}: {why}", path.display())
    })?;
    Ok(Some(name.to_owned()))
}

/// The names the user config at `home` records, or the default.
///
/// Why: `tm fleet status` without `--session` reads the name `init` chose.
/// What: a missing file or key is [`SessionNames::default_names`]; a file
/// that cannot be read or parsed, or an invalid recorded name, is `Err` with
/// the cause, never the default.
/// Test: `status_without_the_flag_reads_the_recorded_name`,
/// `a_malformed_recorded_name_refuses_init_and_fails_status`.
pub(crate) fn recorded_names(home: &Path) -> Result<SessionNames, String> {
    let path = user_config_path(home);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SessionNames::default_names());
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let names = parse_user_config(&raw, &path)
        .and_then(|(doc, _)| recorded(&doc, &path))
        .and_then(|name| {
            name.map_or_else(
                || Ok(SessionNames::default_names()),
                |n| SessionNames::new(&n),
            )
        });
    names.map_err(|e| format!("cannot read the recorded session name: {e:#}"))
}

/// Record `name` as `[supervisor] session` in the user config text `raw`.
///
/// What: [`Edit::Unchanged`] when the file already resolves to `name` — also
/// when it records nothing and `name` is `tm-architect`, so a default run
/// writes no key. Otherwise sets the key, keeping every other key and
/// comment. Postcondition on [`Edit::Changed`]: the text reads back as `name`.
/// Test: `init_with_a_session_records_it_and_status_reads_it_back`,
/// `the_default_session_writes_no_key`.
pub(crate) fn record_name(raw: &str, name: &SessionNames, path: &Path) -> anyhow::Result<Edit> {
    let (mut doc, _) = parse_user_config(raw, path)?;
    let current = recorded(&doc, path)?;
    let effective = current.as_deref().unwrap_or(ARCHITECT_SESSION);
    if effective == name.architect() {
        return Ok(Edit::Unchanged);
    }
    let table = doc
        .entry("supervisor")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .with_context(|| format!("`supervisor` in {} is not a table", path.display()))?;
    table.insert(SESSION_KEY, toml_edit::value(name.architect()));
    let text = doc.to_string();
    let (check, _) = parse_user_config(&text, path)?;
    if recorded(&check, path)?.as_deref() != Some(name.architect()) {
        bail!(
            "internal error: the edited {} does not record session {}",
            path.display(),
            name.architect()
        );
    }
    Ok(Edit::Changed(text))
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
