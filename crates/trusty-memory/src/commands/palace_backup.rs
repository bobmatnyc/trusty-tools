//! `trusty-memory palace backup` and `trusty-memory palace restore` (#9282).
//!
//! Why: an operator needs one command that backs a palace up whether or not
//! the daemon is running, and one that restores it without ever racing a
//! live daemon or writing over a palace.
//! What: [`handle_backup`] calls the daemon's `palace_backup` tool when a
//! daemon serves this data root, and otherwise opens the palace directly
//! under the exclusive redb lock ([`backup_offline`]). A failed daemon call
//! is an error, never a fallback to the direct open. `--dest` is a name
//! under the palace's backup root; both paths refuse a bad name
//! (`check_backup_name`) before anything opens (#9282, option C).
//! [`handle_restore`] probes the daemon socket
//! and then runs [`restore_into`], which refuses a foreign or malformed
//! palace id and a live or unknown daemon before PR 1's
//! `restore_palace_snapshot` verifies and writes. Every refusal is a typed
//! [`RestoreError`] and nothing is written.
//! Test: `palace_backup_9282.rs` (the binary end to end) and
//! `palace_backup_tests.rs` (the typed refusals).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::engine::palace::Palace;
use crate::engine::retrieval::PalaceHandle;
use crate::engine::store::OpenIntent;
use crate::engine::store::kg_redb::snapshot::SNAPSHOT_MANIFEST;
use crate::engine::store::kg_redb::{SnapshotManifest, check_backup_name, restore_palace_snapshot};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use trusty_common::uds::{SocketVerdict, probe_socket_verdict};

use crate::tools::palace_backup::snapshot_json;

/// Budget for the daemon's `palace_backup` call: it copies every row of
/// `kg.redb`, so a large palace takes far longer than an ordinary call.
const BACKUP_CALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long the restore's daemon probe may take before it is inconclusive.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Why `palace restore` refused. Nothing is written in any case.
///
/// Why: the PR 1 review requires these refusals to be typed, so a caller
/// (and a test) can tell them apart without matching message text.
/// Test: `restore_into_refuses_a_palace_mismatch_before_writing`,
/// `restore_into_refuses_an_invalid_manifest_palace_id`,
/// `restore_into_refuses_a_serving_or_unknown_daemon`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestoreError {
    /// `--palace` names another palace than the backup holds.
    #[error("{snapshot} is a backup of palace {found}, not {requested}; nothing was written")]
    PalaceMismatch {
        snapshot: PathBuf,
        requested: String,
        found: String,
    },
    /// The manifest's palace id would not be a safe directory name.
    #[error(
        "{snapshot}: the manifest names palace {found:?}, which is not a valid palace id; nothing was written"
    )]
    InvalidPalaceId { snapshot: PathBuf, found: String },
    /// A daemon serves this data root; it could load a half-written palace.
    #[error(
        "a trusty-memory daemon is serving {socket}; stop it (`trusty-memory stop`) before restoring. Nothing was written"
    )]
    DaemonServing { socket: PathBuf },
    /// The probe could not prove the socket unserved; fail closed.
    #[error(
        "could not tell whether a trusty-memory daemon is serving {socket}; refusing to restore. Nothing was written"
    )]
    DaemonUnknown { socket: PathBuf },
}

/// What a restore wrote, for the report.
#[derive(Debug)]
pub struct RestoreReport {
    /// The palace directory the restore created.
    pub target: PathBuf,
    /// The backup it came from.
    pub snapshot: PathBuf,
    /// The verified manifest.
    pub manifest: SnapshotManifest,
}

/// `trusty-memory palace backup <name> [--dest NAME]`.
///
/// Why/What: see the module doc. `dest` is a name under the backup root,
/// sent as given and never resolved against the current directory; a bad
/// name is refused (exit 1) before the daemon is called or the palace opens.
/// Test: `cli_backup_then_restore_into_a_fresh_root_keeps_counts`,
/// `cli_backup_with_a_failing_daemon_errors_without_falling_back`,
/// `cli_backup_long_s_and_cwd_names_land_under_the_backup_root`,
/// `cli_backup_refuses_a_dest_in_the_registry_dir_and_writes_nothing`.
pub async fn handle_backup(name: String, dest: Option<PathBuf>) -> Result<()> {
    // #9282: refuse a bad name on both paths before any call or open.
    if let Some(d) = &dest {
        check_backup_name(d).with_context(|| format!("back up palace {name}: --dest"))?;
    }
    let report = if crate::client::is_running().await {
        let mut params = json!({ "palace": name });
        if let Some(d) = &dest {
            let d = d
                .to_str()
                .with_context(|| format!("--dest {} is not UTF-8", d.display()))?;
            params["dest"] = json!(d);
        }
        // #9282: a daemon is up, so its answer is final; no direct-open fallback.
        crate::client::call_with_timeout("palace_backup", params, BACKUP_CALL_TIMEOUT).await?
    } else {
        let (_, palace) = super::palace::resolve(&name)?;
        tokio::task::spawn_blocking(move || backup_offline(&palace, dest.as_deref()))
            .await
            .context("join palace backup")??
    };
    print!("{}", render_backup(&report));
    Ok(())
}

/// Back up `palace` with no daemon running.
///
/// Why: the read-only intent never migrates or purges, but it is not a
/// read-only file open: it takes the exclusive redb lock on `kg.redb` for
/// the length of the backup. If a daemon started in the meantime and holds
/// the store, the open falls back to a copied file and
/// `PalaceHandle::snapshot` refuses it (`SnapshotError::ReadOnlyHandle`).
/// What: refuses a `dest` that `check_backup_name` rejects before the open,
/// opens the palace with the read-only intent, snapshots it with no lent chat store
/// (the read-only open of `chat_sessions.redb` works with no other holder),
/// and returns [`snapshot_json`].
/// Test: `cli_backup_then_restore_into_a_fresh_root_keeps_counts`,
/// `cli_backup_refuses_a_dest_in_the_registry_dir_and_writes_nothing`.
pub fn backup_offline(palace: &Palace, dest: Option<&Path>) -> Result<Value> {
    // #9282: a name under the backup root only; refused before the open.
    if let Some(d) = dest {
        check_backup_name(d).with_context(|| format!("back up palace {}", palace.id))?;
    }
    let handle = PalaceHandle::open_with_intent_purging(palace, OpenIntent::ReadOnlyClient, false)
        .with_context(|| format!("open palace {}", palace.id))?;
    let snapshot = handle
        .snapshot(None, dest)
        .with_context(|| format!("back up palace {}", palace.id))?;
    Ok(snapshot_json(&snapshot))
}

/// `trusty-memory palace restore <snapshot> [--palace ID]`.
///
/// Why/What: see the module doc. The target root is derived from the data
/// root (`TRUSTY_DATA_DIR_OVERRIDE` honoured) and the palace id.
/// Test: `cli_backup_then_restore_into_a_fresh_root_keeps_counts`,
/// `cli_restore_refuses_while_a_daemon_serves_the_root`.
pub async fn handle_restore(snapshot: PathBuf, palace: Option<String>) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let socket = crate::transport::uds::socket_path()?;
    let verdict = probe_socket_verdict(&socket, PROBE_TIMEOUT).await;
    let report = tokio::task::spawn_blocking(move || {
        restore_into(&data_dir, &snapshot, palace.as_deref(), &socket, verdict)
    })
    .await
    .context("join palace restore")??;
    print!("{}", render_restore(&report));
    Ok(())
}

/// Check, then restore `snapshot` under `data_dir`.
///
/// Why: requirement 2 and 3 of the PR 1 review — no write under a foreign
/// palace id, and no write while a daemon may serve the root.
/// What: in order, all before any write: read the manifest's palace id;
/// refuse an invalid id ([`RestoreError::InvalidPalaceId`]) or one other
/// than `palace` ([`RestoreError::PalaceMismatch`]); refuse unless `verdict`
/// proves the socket unserved ([`RestoreError::DaemonServing`],
/// [`RestoreError::DaemonUnknown`]). Then `restore_palace_snapshot` into
/// `<registry dir>/<id>`, which verifies the backup and stages then renames
/// it into an empty target. A daemon that starts after the probe sees either
/// no palace or the whole one, never a half-written one.
/// Test: `restore_into_refuses_a_palace_mismatch_before_writing`,
/// `restore_into_refuses_an_invalid_manifest_palace_id`,
/// `restore_into_refuses_a_serving_or_unknown_daemon`.
pub fn restore_into(
    data_dir: &Path,
    snapshot: &Path,
    palace: Option<&str>,
    socket: &Path,
    verdict: SocketVerdict,
) -> Result<RestoreReport> {
    let found = manifest_palace(snapshot)?;
    if !trusty_common::palace_id::palace_id_is_valid(&found) {
        return Err(RestoreError::InvalidPalaceId {
            snapshot: snapshot.to_path_buf(),
            found,
        }
        .into());
    }
    match palace {
        Some(requested) if requested != found => {
            return Err(RestoreError::PalaceMismatch {
                snapshot: snapshot.to_path_buf(),
                requested: requested.to_string(),
                found,
            }
            .into());
        }
        _ => {}
    }
    match verdict {
        SocketVerdict::NotServing => {}
        SocketVerdict::Serving => {
            let socket = socket.to_path_buf();
            return Err(RestoreError::DaemonServing { socket }.into());
        }
        _ => {
            let socket = socket.to_path_buf();
            return Err(RestoreError::DaemonUnknown { socket }.into());
        }
    }
    let root = trusty_common::palace_alias::palace_registry_dir_from(data_dir.to_path_buf());
    let target = root.join(&found);
    let manifest = restore_palace_snapshot(snapshot, &target)
        .with_context(|| format!("restore {} into {}", snapshot.display(), target.display()))?;
    Ok(RestoreReport {
        target,
        snapshot: snapshot.to_path_buf(),
        manifest,
    })
}

/// The palace id `snapshot`'s manifest names.
fn manifest_palace(snapshot: &Path) -> Result<String> {
    let path = snapshot.join(SNAPSHOT_MANIFEST);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest: SnapshotManifest =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(manifest.palace)
}

/// The text `palace backup` prints, from the [`snapshot_json`] record.
fn render_backup(v: &Value) -> String {
    let mut out = format!(
        "backup of palace {} written to {}\n  drawers {}  rooms {}  palace_format {}\n  files:\n",
        v["palace"].as_str().unwrap_or("?"),
        v["dir"].as_str().unwrap_or("?"),
        v["drawer_count"],
        v["room_count"],
        v["palace_format"],
    );
    for f in v["files"].as_array().map(Vec::as_slice).unwrap_or_default() {
        out.push_str(&format!(
            "    {}  {} bytes  sha256 {}\n",
            f["name"].as_str().unwrap_or("?"),
            f["len"],
            f["sha256"].as_str().unwrap_or("?"),
        ));
    }
    out.push_str("  the vector index is not in a backup; a restored palace is re-embedded\n");
    out
}

/// The text `palace restore` prints, ending with the re-embed step.
fn render_restore(r: &RestoreReport) -> String {
    let m = &r.manifest;
    let format = m
        .palace_format
        .map_or_else(|| "none".to_string(), |f| f.to_string());
    format!(
        "restored palace {id} into {target}\n  drawers {d}  rooms {rooms}  palace_format {format}\n  \
         from {snap} (taken {taken})\n\
         next: the vector index is not in a backup, so recall finds no restored drawer yet.\n  \
         1. start the daemon: trusty-memory start\n  \
         2. call the MCP tool palace_reembed {{\"palace\": \"{id}\", \"dry_run\": false}}\n     \
         and repeat until it reports missing: 0\n",
        id = m.palace,
        target = r.target.display(),
        d = m.drawer_count,
        rooms = m.room_count,
        snap = r.snapshot.display(),
        taken = m.taken_at.to_rfc3339(),
    )
}

#[cfg(test)]
#[path = "palace_backup_tests.rs"]
mod tests;
