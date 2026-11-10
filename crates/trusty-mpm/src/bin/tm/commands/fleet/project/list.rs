//! `tm fleet project list` rows (#10895).
//!
//! Why: the Architect reads the watch set from this list on every pass (plan
//! O2), so each row carries what a pass checks first: is the path there, is
//! the project still registered, do its sessions run under the name the key
//! suggests, and does its record exist.
//! What: [`rows`] joins each entry with the [`Registry`] and the live tmux
//! session names; [`render`] prints the human table. The JSON form is the
//! rows serialized, eight fields each.
//! Test: `list_rows_carry_the_eight_fields`,
//! `list_reports_a_missing_path_and_an_alias_mismatch`,
//! `list_counts_sessions_by_the_alias_leaf`.

use std::path::Path;

use serde::Serialize;
use trusty_common::session_naming::leaf_slug_from_hint;

use super::record;
use super::registry::Registry;
use super::watch::WatchFile;

/// One watched project, as `list --json` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Row {
    /// The registry key on the list.
    pub(crate) key: String,
    /// The project's directory: its alias entry's path, else its checkout.
    pub(crate) path: Option<String>,
    /// Whether `path` exists on this host.
    pub(crate) path_exists: bool,
    /// Whether the registry still holds `key`.
    pub(crate) registered: bool,
    /// The `project-paths.json` alias the join found.
    pub(crate) session_alias: Option<String>,
    /// An alias was found and differs from `key`.
    pub(crate) alias_mismatch: bool,
    /// Live tmux sessions named `tm-<leaf>` or `tm-<leaf>-NN`; `null` when
    /// tmux could not be read.
    pub(crate) live_sessions: Option<usize>,
    /// Whether `local/projects/<key>.md` exists.
    pub(crate) record_present: bool,
}

/// One row per entry of `file`, in list order.
///
/// Why: see the module doc.
/// What: an unregistered key still gets a row, with no path and no alias.
/// `sessions` is `None` when tmux could not be enumerated.
/// Test: `list_rows_carry_the_eight_fields`.
pub(crate) fn rows(
    dir: &Path,
    file: &WatchFile,
    registry: &Registry,
    sessions: Option<&[String]>,
) -> Vec<Row> {
    file.projects
        .iter()
        .map(|entry| {
            let key = entry.key.as_str();
            let project = registry.get(key);
            let alias = project.and_then(|p| registry.alias_for(p));
            let path = project.and_then(|p| registry.path_for(p));
            let stem = alias.map_or(key, |a| a.alias.as_str());
            Row {
                key: key.to_owned(),
                path_exists: path.as_deref().is_some_and(Path::exists),
                path: path.map(|p| p.display().to_string()),
                registered: project.is_some(),
                session_alias: alias.map(|a| a.alias.clone()),
                alias_mismatch: alias.is_some_and(|a| a.alias != key),
                live_sessions: sessions.map(|names| count_sessions(names, stem)),
                record_present: record::path(dir, key).is_ok_and(|p| p.exists()),
            }
        })
        .collect()
}

/// The tmux session stem of a project alias or key: `tm-<leaf>`.
pub(crate) fn session_stem(alias: &str) -> String {
    format!("tm-{}", leaf_slug_from_hint(alias))
}

/// How many of `names` are `tm-<leaf>` or `tm-<leaf>-NN` for `alias`.
pub(crate) fn count_sessions(names: &[String], alias: &str) -> usize {
    let stem = session_stem(alias);
    names
        .iter()
        .filter(|name| match name.strip_prefix(stem.as_str()) {
            Some("") => true,
            Some(rest) => rest
                .strip_prefix('-')
                .is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit())),
            None => false,
        })
        .count()
}

/// The human table: one line per row, flags for anything a pass must act on.
pub(crate) fn render(rows: &[Row], list: &Path) -> String {
    if rows.is_empty() {
        return format!(
            "No watched projects ({}). Add one with `tm fleet project add <key|dir>`.\n",
            list.display()
        );
    }
    let width = rows.iter().map(|r| r.key.len()).max().unwrap_or(0);
    let mut out = format!("Watched projects ({}, {}):\n", rows.len(), list.display());
    for row in rows {
        let live = row
            .live_sessions
            .map_or_else(|| "? live".to_owned(), |n| format!("{n} live"));
        let mut flags = Vec::new();
        if !row.registered {
            flags.push("not registered".to_owned());
        }
        if row.path.is_some() && !row.path_exists {
            flags.push("path missing".to_owned());
        }
        if row.alias_mismatch
            && let Some(alias) = &row.session_alias
        {
            flags.push(format!("sessions named {}", session_stem(alias)));
        }
        if !row.record_present {
            flags.push("no record".to_owned());
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", flags.join("] ["))
        };
        out.push_str(&format!(
            "  {:width$}  {live:>7}  {}{flags}\n",
            row.key,
            row.path.as_deref().unwrap_or("-"),
        ));
    }
    out
}
