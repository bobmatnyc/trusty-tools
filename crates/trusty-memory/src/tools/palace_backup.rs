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

use crate::engine::store::kg_redb::{check_backup_name, PalaceSnapshot};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

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
/// (absent, or a name under the backup root, checked by `check_backup_name`
/// before anything opens), opens the handle FIRST so a missing palace errors
/// before the session store could create its directory, then snapshots; the
/// snapshot verifies the root and the name's parents before it writes.
/// Every failure is an `Err`.
/// Test: `palace_backup_manifest_counts_equal_the_palace_counts`,
/// `palace_backup_lends_the_cached_chat_store`,
/// `palace_backup_error_arms_fail_and_write_nothing`,
/// `palace_backup_refuses_a_dest_in_the_registry_dir`,
/// `palace_backup_refuses_a_symlinked_dest_component`,
/// `palace_backup_long_s_and_nested_names_land_under_the_backup_root`.
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

/// `dest` as a backup name, or `None` for the default `<palace>/<UTC>`.
///
/// Why (#9282, option C): `dest` names a directory under the palace's backup
/// root, never a path the daemon resolves; an absolute path, `..`, `.`, an
/// empty segment or a NUL byte is refused here, before anything opens.
/// Test: `palace_backup_error_arms_fail_and_write_nothing`,
/// `palace_backup_refuses_dotdot_after_a_missing_component`.
fn parse_dest(args: &Value) -> Result<Option<PathBuf>> {
    match args.get("dest") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let name = PathBuf::from(s);
            check_backup_name(&name).context("palace_backup: 'dest'")?;
            Ok(Some(name))
        }
        Some(other) => Err(anyhow!(
            "palace_backup: 'dest' must be a string, got {other}"
        )),
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
