//! Where a store's import lands, and the checks made before touching it (#277).
//!
//! Why: palace resolution, the write open, the dry-run snapshot and the
//! live-daemon refusal each decide whether a palace is touched at all, so they
//! sit together, away from the planner.
//! What: [`target_palace`] names the palace and the rule that chose it;
//! [`open_palace_for_write`] opens or creates it, refusing anything but a
//! genuinely absent palace and any degraded drawer load;
//! [`open_snapshot_view`] reads a copy for `--dry-run`; [`DaemonProbe`]
//! detects a running daemon before any write.
//! Test: `corrupt_palace_json_fails_the_store_and_is_left_untouched`,
//! `degraded_drawer_load_refuses_the_write_and_the_dry_run`,
//! `dry_run_through_import_one_leaves_the_palace_byte_identical`,
//! `live_daemon_is_refused_before_any_store_is_read`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::store::{KnowledgeGraph, OpenIntent};
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};
use trusty_common::palace_resolve::PalaceSource;

use super::apply::SnapshotView;
use super::discovery::{DiscoveredStore, STORE_DB_NAME};
use super::KuzuImportError;

/// A resolved target palace and the rule that chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub palace: String,
    /// Printed next to the palace on every store line (#277 H2).
    pub source: &'static str,
}

/// The palace a store imports into; see the module doc of `kuzu_import`.
///
/// Test: `walk_refuses_an_env_palace_and_from_reports_its_source`.
pub fn target_palace(
    store: &DiscoveredStore,
    explicit: Option<&str>,
) -> Result<Target, KuzuImportError> {
    if let Some(p) = explicit {
        if trusty_common::palace_id::palace_id_is_valid(p) {
            return Ok(Target {
                palace: p.to_string(),
                source: "--palace",
            });
        }
        return Err(KuzuImportError::PalaceResolve(format!(
            "invalid palace id {p:?}"
        )));
    }
    let r = trusty_common::palace_resolve::resolve_palace(store.project_dir())
        .map_err(|e| KuzuImportError::PalaceResolve(e.to_string()))?;
    let source = match r.source {
        PalaceSource::EnvOverride => "TRUSTY_MEMORY_PALACE",
        PalaceSource::PinFile => "pin file",
        PalaceSource::GitOwnerRepo => "git owner/repo",
        PalaceSource::ParentDir => "parent/dir",
        _ => "derived",
    };
    Ok(Target {
        palace: r.id,
        source,
    })
}

/// Refuse a walk while `TRUSTY_MEMORY_PALACE` is set.
///
/// Why (#277 H2): `resolve_palace` honours the variable first, and every tm
/// session exports it, so a `--discover` run from a session would send every
/// store into the session's palace. Refusing is chosen over resolving past
/// the variable: the rest of the precedence chain lives in trusty-common, and
/// a local copy of it would drift. A walk is refused even when it finds one
/// store, so a store's palace never depends on how many others the walk
/// found. `--from` still honours the variable, like every other entry point.
/// Test: `walk_refuses_an_env_palace_and_from_reports_its_source`.
pub fn check_env_override(walk: bool, env_palace: Option<&str>) -> Result<(), KuzuImportError> {
    match env_palace {
        Some(v) if walk => Err(KuzuImportError::EnvPalaceWithWalk(v.to_string())),
        _ => Ok(()),
    }
}

/// Whether the store at `dir` (a `.kuzu-memory` path) still exists.
pub fn store_is_live(dir: &str) -> bool {
    Path::new(dir).join(STORE_DB_NAME).exists()
}

/// Open (creating only if genuinely absent) `palace` for writing.
///
/// Why (#277 H1, H3): only a missing `palace.json` means "create". Every
/// other open failure — undecodable metadata, EIO, an incompatible KG, an
/// open-queue timeout — fails the store and leaves the palace untouched
/// (ADR-0045, #5549). A handle whose drawer table loaded degraded is refused
/// too: the drawers are the import ledger, and a partial ledger would
/// re-import every memory it lost.
/// Test: `corrupt_palace_json_fails_the_store_and_is_left_untouched`,
/// `degraded_drawer_load_refuses_the_write_and_the_dry_run`.
pub fn open_palace_for_write(
    data_root: &Path,
    palace: &str,
) -> Result<Arc<PalaceHandle>, KuzuImportError> {
    let registry = PalaceRegistry::new();
    let id = PalaceId::new(palace);
    let handle = match registry.open_palace(data_root, &id) {
        Ok(h) => h,
        Err(e) if PalaceRegistry::open_error_is_absent(&e) => registry
            .create_palace(
                data_root,
                Palace {
                    id: id.clone(),
                    name: palace.to_string(),
                    description: Some("Imported from kuzu-memory".to_string()),
                    created_at: chrono::Utc::now(),
                    data_dir: data_root.join(palace),
                },
            )
            .map_err(|e| KuzuImportError::Palace(format!("{e:#}")))?,
        Err(e) => return Err(KuzuImportError::Palace(format!("{e:#}"))),
    };
    if handle.is_read_only() {
        return Err(KuzuImportError::PalaceLocked(palace.to_string()));
    }
    if handle.drawer_load_degraded {
        return Err(KuzuImportError::DrawersUnreadable(palace.to_string()));
    }
    Ok(handle)
}

/// Read `palace`'s drawers and KG from a copy; empty when absent.
///
/// Why (#277 M1): see [`SnapshotView`] — the live `kg.redb` is only ever read
/// (by the copy), so a dry run writes nothing to the palace.
/// What: copies `kg.redb` into a fresh temp directory and opens the copy. A
/// skipped (unreadable) drawer row fails the store with
/// [`KuzuImportError::DrawersUnreadable`], the same answer the write path
/// gives.
/// Test: `snapshot_view_of_a_missing_palace_is_empty_and_creates_nothing`,
/// `dry_run_through_import_one_leaves_the_palace_byte_identical`,
/// `degraded_drawer_load_refuses_the_write_and_the_dry_run`.
pub fn open_snapshot_view(data_root: &Path, palace: &str) -> Result<SnapshotView, KuzuImportError> {
    let live = data_root.join(palace).join("kg.redb");
    if !live.exists() {
        return Ok(SnapshotView {
            drawers: Vec::new(),
            kg: None,
            snapshot_dir: None,
        });
    }
    let tmp = tempfile::TempDir::with_prefix("trusty-kuzu-dry-run-")
        .map_err(|e| KuzuImportError::Io(format!("create snapshot dir: {e}")))?;
    std::fs::copy(&live, tmp.path().join("kg.redb"))
        .map_err(|e| KuzuImportError::Io(format!("copy kg.redb for a dry run: {e}")))?;
    let err = |e: anyhow::Error| KuzuImportError::Palace(format!("{e:#}"));
    let kg =
        KnowledgeGraph::open_with_intent(&tmp.path().join("kg.db"), OpenIntent::ReadOnlyClient)
            .map_err(err)?;
    let (drawers, skipped) = kg.load_drawers_with_skipped().map_err(err)?;
    if skipped > 0 {
        return Err(KuzuImportError::DrawersUnreadable(palace.to_string()));
    }
    Ok(SnapshotView {
        drawers,
        kg: Some(kg),
        snapshot_dir: Some(tmp),
    })
}

/// Detects a running trusty-memory daemon.
///
/// Why: the import writes palaces in this process; a live daemon holds their
/// write locks. Refusing up front with one clear message beats a per-palace
/// [`KuzuImportError::PalaceLocked`] after the exports already ran.
/// Test: `live_daemon_is_refused_before_any_store_is_read`.
#[async_trait]
pub trait DaemonProbe: Send + Sync {
    /// A description of the live daemon, or `None` when none is running.
    async fn live_daemon(&self) -> Option<String>;
}

/// [`DaemonProbe`] over the daemon's socket and the process table.
///
/// What: the socket probe `trusty-memory start` uses, then the `serve`
/// process scan `trusty-memory stop` uses, which also sees a daemon still
/// hydrating palaces before it binds.
pub struct SystemDaemonProbe;

#[async_trait]
impl DaemonProbe for SystemDaemonProbe {
    async fn live_daemon(&self) -> Option<String> {
        if let Ok(socket) = crate::socket_path() {
            if crate::commands::daemon_guard::probe(&socket).await {
                return Some(format!("serving {}", socket.display()));
            }
        }
        let pids = crate::commands::stop::find_daemon_pids();
        (!pids.is_empty()).then(|| format!("pid {pids:?}"))
    }
}
