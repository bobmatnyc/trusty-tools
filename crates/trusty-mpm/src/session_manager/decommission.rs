//! Decommission and workspace-ownership methods for the session manager (#1511).
//!
//! Why: `decommission` and the companion `set_workspace_owned` are extracted
//! from `manager.rs` to keep that file under the 500-SLOC production cap,
//! mirroring the pattern used by `adopt.rs` and `prune.rs`. The decommission
//! logic is also a natural home for the ownership-tracking primitive.
//! What: two inherent `impl SessionManager` methods —
//! [`SessionManager::decommission`] (full teardown with the #1511 dual guard)
//! and [`SessionManager::set_workspace_owned`] (marks a workspace as
//! SM-provisioned after a git clone).
//! Test: `manager_decommission_removes_workspace`,
//! `manager_decommission_unowned_skips_deletion`,
//! `workspace_owned_flag_round_trips_via_set` in `super::tests`.

use tracing::{info, warn};

use crate::core::trusty_tools_config::{TrustyToolsConfig, workspace_root};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::workspace_guard::is_safe_to_remove;

impl SessionManager {
    /// Decommission a session: stop the runtime, remove the workspace from disk
    /// (ONLY if the SM provisioned it), and mark the record `Decommissioned`.
    ///
    /// Why: the only full teardown operation. Unlike `stop`, this removes the
    /// workspace directory from disk so no future `resume` is possible. A
    /// tombstone record is kept in the store so `ls` can show history.
    ///
    /// Safety (#1511): `remove_dir_all` is executed ONLY when BOTH conditions hold:
    /// (a) `record.workspace_owned == true` — the SM provisioned (cloned) the
    ///     directory and is the rightful owner; local-path spawn (#1502) and
    ///     `adopt_existing` (#1433) leave `workspace_owned = false` so they are
    ///     NEVER deleted by this path.
    /// (b) `is_safe_to_remove(workspace_path, managed_root)` — the canonicalized
    ///     path is strictly INSIDE the SM's managed workspace root, rejecting any
    ///     path outside it (including `$HOME`, volume roots, and paths with too
    ///     few components). This belt-and-suspenders guard catches stale/incorrect
    ///     `workspace_owned` flags before disk mutation occurs.
    ///
    /// When deletion is skipped (unowned or unsafe path), decommission still
    /// transitions the record to `Decommissioned` and returns successfully — the
    /// session becomes unreachable without deleting the user's directory.
    ///
    /// What: kills the tmux session (best-effort), evaluates the two-condition
    /// deletion gate, removes the workspace directory when it is safe to do so,
    /// clears `workspace_path` on the record, marks it `Decommissioned`, persists.
    /// Test: `manager_decommission_removes_workspace` — asserts the workspace dir
    /// is gone from disk and the record state is `Decommissioned`.
    /// `manager_decommission_unowned_skips_deletion` — asserts that decommissioning
    /// a local-path/adopt record does NOT delete the directory.
    pub async fn decommission(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;

        // Kill the runtime (best-effort).
        if self.tmux.session_exists(&record.tmux_name)
            && let Err(e) = self.tmux.kill_session(&record.tmux_name)
        {
            warn!(name = %record.tmux_name, "decommission: kill_session failed: {e}");
        }

        // Guard: only remove the workspace directory if the SM provisioned it.
        if let Some(ref ws) = record.workspace_path {
            if !record.workspace_owned {
                // Unowned workspace (local-path spawn or adopt): never delete.
                warn!(
                    id = %id,
                    workspace = %ws.display(),
                    "decommission: skipping workspace removal — not SM-owned \
                     (local-path or adopted session); the directory was NOT \
                     created by the session manager"
                );
            } else {
                // Owned: apply the belt-and-suspenders path-containment guard.
                let config = TrustyToolsConfig::load();
                let managed_root = workspace_root(&config);

                if !is_safe_to_remove(ws, &managed_root) {
                    warn!(
                        id = %id,
                        workspace = %ws.display(),
                        root = %managed_root.display(),
                        "decommission: skipping workspace removal — path fails \
                         containment guard (outside managed root or unsafe path)"
                    );
                } else if ws.exists() {
                    std::fs::remove_dir_all(ws).map_err(|e| {
                        ManagedError::Io(std::io::Error::new(
                            e.kind(),
                            format!("remove workspace {:?}: {e}", ws),
                        ))
                    })?;
                    info!(
                        id = %id,
                        workspace = %ws.display(),
                        "decommission: owned workspace removed from disk"
                    );
                } else {
                    warn!(
                        id = %id,
                        workspace = %ws.display(),
                        "decommission: workspace path absent (already removed?)"
                    );
                }
            }
        }

        // Tombstone: clear workspace_path, mark Decommissioned, persist.
        record.workspace_path = None;
        record.workspace_owned = false;
        record.state = ManagedSessionState::Decommissioned;
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session decommissioned");
        Ok(record)
    }

    /// Mark a session's workspace as SM-owned (provisioned by clone) or unowned.
    ///
    /// Why (#1511): the decommission path must know whether the SM provisioned the
    /// `workspace_path` (and therefore may `remove_dir_all` it) or whether the path
    /// is a real, pre-existing user directory (local-path spawn, adopt) that must
    /// NEVER be deleted. Setting `workspace_owned = true` is the explicit assertion
    /// that this SM cloned the workspace; `false` (the serde default) means "do not
    /// touch this directory on decommission." Callers that use the local-path spawn
    /// or `adopt_existing` path MUST NOT call this method (or call it with `false`).
    /// What: looks up the record, sets `workspace_owned`, and persists.
    /// Test: `workspace_owned_flag_round_trips_via_set` + the decommission guard
    /// tests in `session_manager/tests.rs`.
    pub async fn set_workspace_owned(
        &self,
        id: &ManagedSessionId,
        owned: bool,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.workspace_owned = owned;
        self.store.write().await.upsert(record).await?;
        Ok(())
    }
}
