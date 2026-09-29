//! The two TOML edits `tm fleet init` makes, as pure text-to-text functions (#8436).
//!
//! Why: the user-level `~/.trusty-mpm/config.toml` holds the operator's other
//! settings and comments, and a lenient load turns a bad file into defaults.
//! A rewrite through `MpmConfig` would drop comments and unknown keys, and a
//! write over a file tm could not parse would reset it. `toml_edit` changes
//! one array and leaves every other byte alone.
//! What: [`add_allowlist_entry`] and [`request_supervisor_profile`] each take
//! a file's text and return [`Edit::Unchanged`] or the new text. Both refuse
//! text that does not parse, both as TOML and as the typed config tm reads,
//! so a caller never writes over a file it could not read.
//! Test: `commands::fleet::tests`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use toml_edit::{Array, DocumentMut, Item, Value};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::project_config::ProjectLevelConfig;
use trusty_mpm::core::session_profile::{self, SUPERVISOR_PROFILE_ID};

/// The result of one edit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    /// The file already says what the edit would make it say.
    Unchanged,
    /// The file's new text.
    Changed(String),
}

/// Parse the user config text strictly: TOML syntax and the `MpmConfig` shape.
///
/// Why: `MpmConfig::load` falls back to defaults on a bad file, and a
/// defaulted config has an empty allowlist — writing it back would erase the
/// operator's settings.
/// What: `Ok` with both parses; an error naming `path` otherwise.
/// Test: `a_malformed_config_fails_and_is_left_byte_identical`.
pub(crate) fn parse_user_config(
    raw: &str,
    path: &Path,
) -> anyhow::Result<(DocumentMut, MpmConfig)> {
    let doc = raw
        .parse::<DocumentMut>()
        .with_context(|| malformed(path))?;
    let typed = toml::from_str::<MpmConfig>(raw).with_context(|| malformed(path))?;
    Ok((doc, typed))
}

/// The refusal text for a file `tm fleet init` will not edit.
fn malformed(path: &Path) -> String {
    format!(
        "{} is malformed; fix it by hand and re-run — `tm fleet init` never rewrites a file it \
         cannot parse",
        path.display()
    )
}

/// Add `dir` to `[supervisor] projects` in the user config text `raw`.
///
/// Why: condition (a) of the supervisor gate (#8453). #3981 kept this grant
/// outside every project's write boundary; the #8436 owner ruling (Q6) lets
/// `tm fleet init` write it, including when a PM runs the command.
/// What: `dir` must be absolute and UTF-8. [`Edit::Unchanged`] when an entry
/// already canonicalizes to `dir` ([`session_profile::path_is_listed`]).
/// Otherwise appends `dir` to the array, creating the `[supervisor]` table or
/// the `projects` key when absent. Refuses unparseable text and a
/// `supervisor` or `projects` of the wrong type.
/// Postcondition on [`Edit::Changed`]: the new text parses as `MpmConfig` and
/// lists `dir`; every other key and comment is untouched.
/// Test: `an_unrelated_key_and_comment_survive_the_allowlist_write`,
/// `a_second_add_is_unchanged`, `an_existing_supervisor_table_gains_the_entry`.
pub(crate) fn add_allowlist_entry(raw: &str, dir: &Path, path: &Path) -> anyhow::Result<Edit> {
    let (mut doc, typed) = parse_user_config(raw, path)?;
    if session_profile::is_allow_listed(dir, &typed.supervisor) {
        return Ok(Edit::Unchanged);
    }
    let entry = absolute_utf8(dir)?;
    let table = doc
        .entry("supervisor")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .with_context(|| format!("`supervisor` in {} is not a table", path.display()))?;
    match table.get_mut("projects") {
        None => {
            table.insert(
                "projects",
                Item::Value(Value::Array(Array::from_iter([entry]))),
            );
        }
        Some(item) => item
            .as_array_mut()
            .with_context(|| {
                format!(
                    "`supervisor.projects` in {} is not an array",
                    path.display()
                )
            })?
            .push(entry),
    }
    let text = doc.to_string();
    // Postcondition: the file tm will read back grants `dir`.
    let (_, check) = parse_user_config(&text, path)?;
    if !session_profile::is_allow_listed(dir, &check.supervisor) {
        bail!(
            "internal error: the edited {} does not list {}",
            path.display(),
            dir.display()
        );
    }
    Ok(Edit::Changed(text))
}

/// Set `profile = "supervisor"` in a project's `.trusty-mpm.toml` text `raw`.
///
/// Why: condition (b) of the supervisor gate (#8453).
/// What: [`Edit::Unchanged`] when the file already resolves to the supervisor
/// profile ([`session_profile::profile_from_config`]); otherwise sets the key,
/// keeping every other key and comment. Refuses text that fails the
/// project-config schema (`deny_unknown_fields`).
/// Test: `a_second_run_changes_nothing`,
/// `a_malformed_project_config_fails_and_is_left_byte_identical`.
pub(crate) fn request_supervisor_profile(raw: &str, path: &Path) -> anyhow::Result<Edit> {
    let typed = ProjectLevelConfig::from_toml(raw, path).with_context(|| malformed(path))?;
    if session_profile::profile_from_config(Some(&typed)).is_supervisor() {
        return Ok(Edit::Unchanged);
    }
    let mut doc = raw
        .parse::<DocumentMut>()
        .with_context(|| malformed(path))?;
    doc["profile"] = toml_edit::value(SUPERVISOR_PROFILE_ID);
    Ok(Edit::Changed(doc.to_string()))
}

/// Other allow-listed projects that already run as a supervisor.
///
/// Why: one Architect per user (#8436 ruling); a second `init` elsewhere
/// must not make a second one.
/// What: each absolute `[supervisor] projects` entry that does not
/// canonicalize to `dir` and whose own `.trusty-mpm.toml` requests the
/// supervisor profile.
/// Test: `a_second_architect_elsewhere_is_refused`.
pub(crate) fn other_architects(config: &MpmConfig, dir: &Path) -> Vec<PathBuf> {
    config
        .supervisor
        .projects
        .iter()
        .filter(|entry| !session_profile::path_is_listed(dir, std::slice::from_ref(entry)))
        .filter(|entry| entry.is_absolute() && session_profile::requested(entry).is_supervisor())
        .cloned()
        .collect()
}

/// `dir` as the string an allowlist entry holds.
fn absolute_utf8(dir: &Path) -> anyhow::Result<String> {
    if !dir.is_absolute() {
        bail!("allowlist entry {} is not an absolute path", dir.display());
    }
    dir.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not valid UTF-8", dir.display()))
}
