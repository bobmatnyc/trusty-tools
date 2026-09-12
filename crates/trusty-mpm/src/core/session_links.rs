//! Which Claude session ids belong to the same managed session (#7617).
//!
//! Why: Claude Code mints a NEW `session_id` on every restart — a relaunch, a
//! `/login` account switch, a crash — and the `💸` statusline segment folds the
//! savings ledger by exactly the id Claude Code hands it on stdin. On
//! 2026-09-12 one managed session carried three ids in a single evening
//! (3544c9e5 → 63589e53 → f3def033), and after each restart the segment
//! vanished until the new id had earned rows of its own. The daemon already
//! knows the correlation — `SessionStart: linked Claude session to managed
//! session (#1744)` — but it keeps only the CURRENT id on the record, so the
//! prior ids' rows become unreachable the moment they are superseded.
//!
//! What: an append-only sidecar under the framework root, one file per managed
//! session listing every Claude session id ever linked to it.
//! [`record_link`] is written by the daemon's `SessionStart` correlation;
//! [`linked_claude_ids`] is read by the statusline, which needs no daemon to be
//! running and no session store to parse.
//!
//! FAIL-OPEN BY CONSTRUCTION: every read failure — an absent directory, an
//! unreadable file, a partial write — yields NO siblings. The caller then shows
//! its explicit empty state. Nothing here ever invents a link, so a fold can
//! never be attributed to a session that did not earn it.
//! Test: the `tests` module below.

use std::path::{Path, PathBuf};

/// The sidecar directory, under the framework root's `usage/`.
///
/// Why: it sits beside `savings.jsonl` and `no-fold-warned/` because
/// `tm repair savings-ledger` sweeps that directory as a unit, and a link store
/// that survived a ledger reset would point at rows that no longer exist.
const SESSION_LINKS_DIR: &str = "session-links";

/// Where `managed_id`'s link list lives.
///
/// What: `<root>/usage/session-links/<managed_id>`. `None` for an id that is
/// not a single safe path segment — the id is used verbatim as a filename, so
/// anything with a separator, or a relative-path spelling, is refused rather
/// than resolved.
/// Test: `a_traversing_managed_id_is_refused`.
fn link_file(root: &Path, managed_id: &str) -> Option<PathBuf> {
    let id = managed_id.trim();
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.starts_with('.')
    {
        return None;
    }
    Some(root.join("usage").join(SESSION_LINKS_DIR).join(id))
}

/// Record that `claude_id` belongs to `managed_id`.
///
/// Why: the daemon's correlation overwrites the record's single
/// `claude_session_id` field, so without this the previous id — and every
/// savings row keyed by it — is orphaned. Appending rather than replacing is
/// the whole point.
/// What: appends `claude_id` as its own line, once. Already-present ids are a
/// no-op, so the repeated `SessionStart` a resume or compaction raises costs one
/// read. Best-effort: a failure to create the directory or write the file is
/// returned as `false` and never propagated — a missing link degrades the
/// segment, and must not fail a session start.
/// Test: `a_link_is_recorded_once`, `a_second_id_joins_the_same_managed_session`,
/// `a_traversing_managed_id_is_refused`.
pub fn record_link(root: &Path, managed_id: &str, claude_id: &str) -> bool {
    let claude_id = claude_id.trim();
    if claude_id.is_empty() {
        return false;
    }
    let Some(path) = link_file(root, managed_id) else {
        return false;
    };
    if read_ids(&path).iter().any(|id| id == claude_id) {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    use std::io::Write;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return false;
    };
    writeln!(file, "{claude_id}").is_ok()
}

/// Every Claude session id that shares a managed session with `claude_id`.
///
/// Why (#7617): this is what lets the segment fold across a restart instead of
/// reading as a disappearance. The lookup is by CONTENT rather than through a
/// reverse index because the statusline runs in a short-lived process with no
/// daemon to ask, and one small directory scan is cheaper than keeping a second
/// file consistent with the first.
/// What: the ids from the one link file that lists `claude_id`, INCLUDING
/// `claude_id` itself, so a caller can fold the whole set in one pass. Empty
/// when nothing links it — an unmanaged session, a session that started before
/// this store existed, or any read failure at all.
/// Test: `siblings_include_the_queried_id`, `an_unlinked_id_has_no_siblings`,
/// `an_absent_store_has_no_siblings`.
pub fn linked_claude_ids(root: &Path, claude_id: &str) -> Vec<String> {
    let claude_id = claude_id.trim();
    if claude_id.is_empty() {
        return Vec::new();
    }
    let dir = root.join("usage").join(SESSION_LINKS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let ids = read_ids(&entry.path());
        if ids.iter().any(|id| id == claude_id) {
            return ids;
        }
    }
    Vec::new()
}

/// The non-empty lines of `path`, trimmed.
///
/// What: an unreadable or absent file reads as no ids, which is what makes
/// every failure path above degrade to "no siblings" rather than to a guess.
fn read_ids(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[path = "session_links_tests.rs"]
mod tests;
