//! The owner's technical level for owner-facing text: the port of `technical_level.py` (#12028).
//!
//! Why: the Architect's chat, dashboard items and briefs are written at a
//! level the owner sets in `meta/settings.technical_level`; the hook, the
//! dashboard and `level_lint` all read it through this one table.
//! What: [`LEVELS`] (lowest to highest), [`DEFAULT`] and [`rule`] (the
//! one-line rule per level) are the script's tables; [`normalize`] maps any
//! value outside [`LEVELS`] to [`DEFAULT`]; [`below_engineer`] is true for the
//! levels `level_lint` checks; [`line()`] is the hook text; [`read`] returns the
//! stored level through the dashboard store, [`DEFAULT`] when the document or
//! key is missing or holds anything else.
//! Test: `technical_level_recordings_replay_clean_against_the_port`.

use std::path::Path;

use serde_json::Value;

use crate::dashboard::DashError;
use crate::dashboard::store::{Format, Store};

/// The six levels, lowest to highest (`LEVELS`).
pub const LEVELS: [&str; 6] = [
    "non-technical",
    "vibe-coder",
    "pm",
    "engineer",
    "senior-engineer",
    "cto",
];

/// The level for an unset, missing or unknown value (`DEFAULT`).
pub const DEFAULT: &str = "pm";

const RULES: [&str; 6] = [
    "everyday words, business impact, 2-3 sentences, no paths, SHAs, flags or tool names",
    "product words (app, deploy, repo), what the user sees, 3-4 sentences, no line numbers, SHAs or flags",
    "outcome, cost, risk, reversibility, 4-6 sentences; issue and PR links ok, no file paths, SHAs, flags or job names",
    "full technical terms, module names, root cause and trade-offs, 6-10 sentences with a short evidence list",
    "design trade-offs, failure modes, rollout and rollback, file:line evidence, blast radius",
    "risk to production and customers, cost, reversibility, the contract at stake; line detail stays in the brief",
];

/// Why [`read`] could not answer: the store raised something other than
/// "not found" (the script's `StoreError` propagates).
#[derive(Debug, thiserror::Error)]
pub enum LevelError {
    /// `DASHBOARD_FORMAT` names neither `dir` nor `jsonl`.
    #[error("DASHBOARD_FORMAT={0}: use dir or jsonl")]
    Format(String),
    /// The store refused or could not read `meta/settings`.
    #[error(transparent)]
    Store(#[from] DashError),
}

fn index(level: &str) -> Option<usize> {
    LEVELS.iter().position(|l| *l == level)
}

/// `normalize(value)`: `value` when it is one of [`LEVELS`], else [`DEFAULT`].
pub fn normalize(value: &str) -> &'static str {
    index(value).map_or(DEFAULT, |i| LEVELS[i])
}

/// `below_engineer(level)`: true for `non-technical`, `vibe-coder` and `pm`
/// after [`normalize`].
pub fn below_engineer(level: &str) -> bool {
    index(normalize(level)) < index("engineer")
}

/// The one-line rule of `level` after [`normalize`] (`RULES[level]`).
pub fn rule(level: &str) -> &'static str {
    index(normalize(level)).map_or("", |i| RULES[i])
}

/// `line(level)`: `TECHNICAL LEVEL: <level> (<rule>)` after [`normalize`].
pub fn line(level: &str) -> String {
    format!("TECHNICAL LEVEL: {} ({})", normalize(level), rule(level))
}

/// `dashboard_store.default_format()` for a `DASHBOARD_FORMAT` value.
///
/// # Errors
/// [`LevelError::Format`] for a value other than unset, empty, `dir` or `jsonl`.
pub fn store_format(value: Option<&str>) -> Result<Format, LevelError> {
    match value {
        None | Some("") | Some("dir") => Ok(Format::Dir),
        Some("jsonl") => Ok(Format::Jsonl),
        Some(v) => Err(LevelError::Format(crate::pystr::py_repr(v))),
    }
}

/// `read(root)`: the stored level of the store at `root`.
///
/// Why: the level is a dashboard setting; every reader goes through the
/// store so a refused or other-format store is never read as unset.
/// What: `meta/settings` → `technical_level`, through [`normalize`] when it
/// is a string; a missing document, a missing key or a non-string value is
/// [`DEFAULT`].
/// Test: `technical_level_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// [`LevelError::Store`] for any store error but "not found".
pub fn read(root: &Path, format: Format) -> Result<&'static str, LevelError> {
    match Store::new(root, format).get("meta", "settings") {
        Ok(doc) => Ok(match doc.data.get("technical_level") {
            Some(Value::String(s)) => normalize(s),
            _ => DEFAULT,
        }),
        Err(DashError::NotFound(_)) => Ok(DEFAULT),
        Err(e) => Err(e.into()),
    }
}
