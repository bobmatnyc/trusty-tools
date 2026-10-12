//! Retiring a managed session's channel taint when the session ends (#8433 P0).
//!
//! Why (owner ruling Dfc): a channel taint lasts until the managed session
//! ends — a deliberate stop or a decommission — and nothing else removes it.
//! The manager owns those two transitions, so it retires the taint there.
//! What: the daemon installs its taint directory once
//! ([`SessionManager::install_taint_dir`]); [`SessionManager::retire_channel_taint`]
//! runs `channels_taint::retire` for one record. With no directory installed
//! (a test manager, `tm supervisor`) nothing is retired, which leaves the
//! session tainted: the fail-closed direction. A failed retire is logged at
//! ERROR, leaves the files in place, and the `channel_taint` doctor row
//! reports them.
//! Test: `stop_and_decommission_retire_taint` in `channel_taint_tests.rs`.

use std::path::PathBuf;

use super::manager::SessionManager;
use super::record::ManagedSessionId;
use crate::core::channels_taint;

impl SessionManager {
    /// Install the directory taint files live in; `false` if one already is.
    ///
    /// Why: the manager has no home of its own; the daemon resolves it.
    pub fn install_taint_dir(&self, dir: PathBuf) -> bool {
        self.taint_dir.set(dir).is_ok()
    }

    /// The bookkeeping after a full decommission teardown is persisted.
    ///
    /// What: the #7087 residency bump and cache evict, then
    /// [`Self::retire_channel_taint`]. The record-only tombstone does not call
    /// this: its contract is a store write and nothing else, so a taint it
    /// leaves is reported by the doctor row.
    pub(crate) async fn after_full_teardown(&self, id: &ManagedSessionId) {
        self.bump_residency_generation();
        self.residency_cache_evict(id).await;
        self.retire_channel_taint(id);
    }

    /// Retire `id`'s channel taint, because the managed session ended (Dfc).
    ///
    /// What: no-op without an installed directory. A retire error is logged
    /// at ERROR and the files stay, so the session stays tainted.
    pub(crate) fn retire_channel_taint(&self, id: &ManagedSessionId) {
        let Some(dir) = self.taint_dir.get() else {
            return;
        };
        match channels_taint::retire(dir, &id.to_string()) {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(id = %id, removed = n, "managed session ended: channel taint retired (#8433)")
            }
            Err(e) => tracing::error!(
                id = %id,
                "managed session ended but its channel taint could not be retired; it stays \
                 tainted and `tm doctor` lists it (#8433): {e}"
            ),
        }
    }
}

#[cfg(test)]
#[path = "channel_taint_tests.rs"]
mod tests;
