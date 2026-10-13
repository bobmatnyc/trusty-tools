//! Whether a palace holds anything a rename-over or a delete would lose (#9544).
//!
//! Why: a palace rename may replace an existing target only when that target is
//! empty, and `palace_delete` asks the same question before it removes a
//! directory. Two copies of "empty" drift — the delete check already ignored
//! `chat_sessions.redb` — so the rule lives here once and both callers read it.
//! What: [`check_palace_empty`] answers `Ok(())` or the first reason the palace
//! is not empty, in a fixed order: unconfirmed (degraded drawer load, unreadable
//! drawer ids, unreadable chat store), legacy data, drawers, chat sessions.
//! Knowledge-graph triples never count: a new palace is seeded with bootstrap
//! triples, and the delete check has never counted triples either. Legacy
//! `kg.db` decoding needs SQLite, which this crate does not link by default, so
//! the caller passes a [`LegacyProbe`]; [`conservative_legacy_probe`] is the
//! fail-closed default for a caller without one.
//! Test: `rename_palace_refuses_a_nonempty_target`,
//! `rename_palace_refuses_a_target_with_chat_sessions`,
//! `chat_sessions_make_a_palace_non_empty`,
//! `conservative_probe_flags_kg_db_and_quarantined_stores`.

use std::collections::HashSet;
use std::path::Path;

use redb::{ReadableDatabase, ReadableTableMetadata, TableError};
use uuid::Uuid;

use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::kg_store::SESSIONS;
use crate::redb_open::INCOMPATIBLE_SUFFIX;

/// Reports legacy data the live store has not absorbed, given the palace
/// directory and the drawer ids the live store holds. `Some(reason)` makes the
/// palace non-empty.
pub type LegacyProbe = dyn Fn(&Path, &HashSet<Uuid>) -> Option<String> + Send + Sync;

/// Why a palace is not empty.
///
/// Why: a delete may offer `force` for drawers but must not for legacy data,
/// which `force` would destroy unimported, so callers need the arm, not a
/// string.
/// What: one variant per reason, in the order [`check_palace_empty`] tests them.
/// Test: `chat_sessions_make_a_palace_non_empty`,
/// `rename_palace_refuses_a_nonempty_target`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PalaceNotEmpty {
    /// Something that would decide emptiness could not be read.
    #[error("the palace could not be confirmed empty: {0}")]
    Unconfirmed(String),
    /// Legacy data the live store has not absorbed.
    #[error("the palace holds legacy data: {0}")]
    LegacyData(String),
    /// Live drawers.
    #[error("the palace has {0} drawer(s)")]
    Drawers(usize),
    /// Saved chat sessions.
    #[error("the palace has {0} chat session(s)")]
    ChatSessions(u64),
}

/// Whether the opened palace `handle`, stored at `data_dir`, is empty.
///
/// Why: see the module docs; this is the single statement of "empty".
/// What: returns the first failing check as a [`PalaceNotEmpty`]: a degraded
/// drawer load or unreadable drawer ids are `Unconfirmed`; `legacy` reporting
/// a reason is `LegacyData`; live drawers are `Drawers`; then
/// [`count_chat_sessions`] (an unreadable chat store is `Unconfirmed`). Reads
/// only — it never creates or rewrites a file. Blocking.
/// Test: `rename_palace_refuses_a_nonempty_target`,
/// `chat_sessions_make_a_palace_non_empty`,
/// `rename_palace_replace_empty_trashes_the_target`.
pub fn check_palace_empty(
    handle: &PalaceHandle,
    data_dir: &Path,
    legacy: &LegacyProbe,
) -> Result<(), PalaceNotEmpty> {
    if handle.drawer_load_degraded {
        return Err(PalaceNotEmpty::Unconfirmed(
            "the drawer table loaded degraded".to_string(),
        ));
    }
    let live = handle
        .kg
        .load_drawer_ids()
        .map_err(|e| PalaceNotEmpty::Unconfirmed(format!("drawer ids could not be read: {e:#}")))?;
    if let Some(reason) = legacy(data_dir, &live) {
        return Err(PalaceNotEmpty::LegacyData(reason));
    }
    let drawers = handle.drawers.read().len().max(live.len());
    if drawers > 0 {
        return Err(PalaceNotEmpty::Drawers(drawers));
    }
    match count_chat_sessions(data_dir) {
        Ok(0) => Ok(()),
        Ok(n) => Err(PalaceNotEmpty::ChatSessions(n)),
        Err(reason) => Err(PalaceNotEmpty::Unconfirmed(reason)),
    }
}

/// Fail-closed [`LegacyProbe`] for callers that cannot decode `kg.db`.
///
/// Why: without SQLite this crate cannot tell an imported `kg.db` from one
/// holding the only copy of some drawers, so it must assume the latter.
/// What: `Some(reason)` when `<data_dir>/kg.db` exists or cannot be probed, or
/// when any `.v2-incompatible` file sits directly in `data_dir` or the
/// directory cannot be listed; `None` otherwise.
/// Test: `conservative_probe_flags_kg_db_and_quarantined_stores`.
pub fn conservative_legacy_probe(data_dir: &Path, _live: &HashSet<Uuid>) -> Option<String> {
    let mut reasons = Vec::new();
    match data_dir.join("kg.db").try_exists() {
        Ok(false) => {}
        Ok(true) => reasons.push("a legacy kg.db is present".to_string()),
        Err(e) => reasons.push(format!("kg.db could not be checked: {e}")),
    }
    match std::fs::read_dir(data_dir) {
        Ok(entries) => {
            let quarantined = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .contains(INCOMPATIBLE_SUFFIX)
                })
                .count();
            if quarantined > 0 {
                reasons.push(format!("{quarantined} {INCOMPATIBLE_SUFFIX} file(s)"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => reasons.push(format!("{} could not be listed: {e}", data_dir.display())),
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// Number of saved chat sessions in `<data_dir>/chat_sessions.redb`.
///
/// Why (#9544): the delete check ignored this store, so a palace holding only
/// chat history read as empty.
/// What: `Ok(0)` when the file is absent or has no sessions table. Otherwise
/// opens it read-only (no create, no quarantine) and counts the rows. Any
/// failure — including a store another process holds open — is `Err` with a
/// reason, so the caller fails closed.
/// Test: `chat_sessions_make_a_palace_non_empty`.
pub fn count_chat_sessions(data_dir: &Path) -> Result<u64, String> {
    let path = data_dir.join("chat_sessions.redb");
    match path.try_exists() {
        Ok(false) => return Ok(0),
        Ok(true) => {}
        Err(e) => return Err(format!("{} could not be checked: {e}", path.display())),
    }
    let fail = |e: &dyn std::fmt::Display| format!("{} could not be read: {e}", path.display());
    let db = crate::redb_cache::open_palace_db_read_only(&path).map_err(|e| fail(&e))?;
    let rtx = db.begin_read().map_err(|e| fail(&e))?;
    match rtx.open_table(SESSIONS) {
        Ok(table) => table.len().map_err(|e| fail(&e)),
        Err(TableError::TableDoesNotExist(_)) => Ok(0),
        Err(e) => Err(fail(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::store::chat_sessions::ChatSessionStore;

    /// Why (#9544): chat history alone must make a palace non-empty.
    /// Test: itself.
    #[test]
    fn chat_sessions_make_a_palace_non_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(count_chat_sessions(tmp.path()), Ok(0), "absent store");
        let path = tmp.path().join("chat_sessions.redb");
        drop(ChatSessionStore::open(&path).unwrap());
        assert_eq!(
            count_chat_sessions(tmp.path()),
            Ok(0),
            "store with no sessions"
        );
        let store = ChatSessionStore::open(&path).unwrap();
        store.create_session(Some("t".to_string())).unwrap();
        drop(store);
        assert_eq!(count_chat_sessions(tmp.path()), Ok(1));
    }

    /// Why (#9544): without SQLite, any `kg.db` or quarantined store must read
    /// as legacy data rather than be assumed imported.
    /// Test: itself.
    #[test]
    fn conservative_probe_flags_kg_db_and_quarantined_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let live = HashSet::new();
        assert_eq!(conservative_legacy_probe(tmp.path(), &live), None);
        std::fs::write(tmp.path().join("kg.db"), b"x").unwrap();
        assert!(conservative_legacy_probe(tmp.path(), &live).is_some());
        std::fs::remove_file(tmp.path().join("kg.db")).unwrap();
        std::fs::write(
            tmp.path().join(format!("kg.redb{INCOMPATIBLE_SUFFIX}")),
            b"x",
        )
        .unwrap();
        assert!(conservative_legacy_probe(tmp.path(), &live).is_some());
    }
}
