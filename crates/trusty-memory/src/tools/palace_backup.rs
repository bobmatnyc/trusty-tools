//! `palace_backup` — a consistent backup of a palace the daemon serves (#9282).
//!
//! Why: the trusty-memory 1.0 gate needs a backup taken from a live daemon
//! that reopens cleanly. Only the daemon can take it: it holds the redb lock
//! on `kg.redb`, and its session-store cache holds `chat_sessions.redb`.
//! What: [`handle_palace_backup`] opens the palace handle, borrows the
//! daemon's cached chat store, and runs `PalaceHandle::snapshot` on a
//! blocking thread. [`snapshot_json`] is the one shape both the tool and the
//! offline CLI path print.
//! Test: `palace_backup_manifest_counts_equal_the_palace_counts`,
//! `palace_backup_lends_the_cached_chat_store`,
//! `palace_backup_error_arms_fail_and_write_nothing`.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::store::kg_redb::PalaceSnapshot;

use super::helpers::{open_palace_handle, resolve_palace};
use crate::AppState;

/// `palace_backup {palace?, dest?}` (#9282).
///
/// Why: see the module doc. The chat store is ALWAYS the daemon's cached one
/// (PR 1 review, requirement 1): redb allows one handle per file per process,
/// so a backup that opened the file itself would fail while the cache held
/// it, and while its handle was held every chat call that opened the store
/// would fail.
/// What: resolves `palace` as the other palace tools do, parses `dest`
/// (absent, or an absolute path that must not exist), opens the handle
/// FIRST so a missing palace errors before the session store could create
/// its directory, then snapshots. Every failure is an `Err`.
/// Test: `palace_backup_manifest_counts_equal_the_palace_counts`,
/// `palace_backup_lends_the_cached_chat_store`,
/// `palace_backup_error_arms_fail_and_write_nothing`.
pub(crate) async fn handle_palace_backup(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "palace_backup")?;
    let dest = parse_dest(&args)?;
    let handle = open_palace_handle(state, &palace)?;
    // #9282: lend the cached store, keyed by the handle's canonical id.
    let chat = state.session_store(handle.id.as_str())?;
    let snapshot =
        tokio::task::spawn_blocking(move || handle.snapshot(Some(chat.as_ref()), dest.as_deref()))
            .await
            .context("join palace_backup")?
            .with_context(|| format!("palace_backup: back up palace {palace}"))?;
    Ok(snapshot_json(&snapshot))
}

/// `dest` as an absolute path, or `None` for the default directory.
///
/// Why: a relative path would resolve against the daemon's working
/// directory, which the caller cannot see.
fn parse_dest(args: &Value) -> Result<Option<PathBuf>> {
    match args.get("dest") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let path = PathBuf::from(s);
            if path.is_absolute() {
                Ok(Some(path))
            } else {
                Err(anyhow!(
                    "palace_backup: 'dest' must be an absolute path, got {s:?}"
                ))
            }
        }
        Some(other) => Err(anyhow!("palace_backup: 'dest' must be a string, got {other}")),
    }
}

/// The JSON a backup reports: directory, manifest counts and file list.
///
/// Why: the MCP tool and the CLI's direct path print the same record, so a
/// script reads one shape whichever path ran.
/// Test: `palace_backup_manifest_counts_equal_the_palace_counts`.
pub fn snapshot_json(snapshot: &PalaceSnapshot) -> Value {
    let m = &snapshot.manifest;
    json!({
        "palace": m.palace,
        "dir": snapshot.dir,
        "taken_at": m.taken_at,
        "palace_format": m.palace_format,
        "drawer_count": m.drawer_count,
        "room_count": m.room_count,
        "kg_tables": m.kg_tables,
        "chat_tables": m.chat_tables,
        "files": m.files,
        // ADR-0067 D2: derived, rebuilt by a re-embed after restore.
        "vector_index_included": false,
    })
}
