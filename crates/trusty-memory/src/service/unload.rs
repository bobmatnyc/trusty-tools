//! `MemoryService::unload_palace` — close one palace and keep its data (#10989).
//!
//! Why: tm decommissions a project and must shut its palace down without
//! deleting it. A palace is lazily reopened on its next use, so "shut down"
//! means releasing what the daemon holds open for it: the registry handle
//! (drawer table, vector index, KG and their redb files), the chat-session
//! store handle and the resident BM25 index.
//! What: [`MemoryService::unload_palace`] takes the palace's write lock (so no
//! write is mid-flight), flushes and drops the BM25 index, then drops the
//! session store and the registry handle — the same releases `delete_palace`
//! and `rename_palace` run before they touch the directory, without touching
//! it. Folded as `memory.palace_unload`.
//! Test: `service/unload_tests.rs`.

use std::time::Duration;

use serde::Serialize;
use trusty_common::palace_alias::canonical_palace_id;

use super::core::MemoryService;
use super::types::{ServiceError, ServiceResult};
use crate::engine::palace::PalaceId;

/// How long an unload waits for an in-flight write to finish.
///
/// Why: an unload is a shutdown step tm runs inside one request; a write that
/// holds the lock longer than this is reported busy rather than waited out.
pub const UNLOAD_LOCK_WAIT: Duration = Duration::from_secs(10);

/// What an unload did (#10989).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnloadOutcome {
    /// The id the caller sent.
    pub palace_id: String,
    /// The id the palace's data lives under (an alias resolves to its target).
    pub canonical_id: String,
    /// Whether the palace exists on disk. `false` means nothing was loaded.
    pub exists: bool,
    /// Whether a registry handle was resident before the unload.
    pub was_resident: bool,
    /// Always `true` on success: no handle for the palace is resident now.
    pub unloaded: bool,
    /// Always `true`: an unload never removes palace data.
    pub data_kept: bool,
}

impl MemoryService {
    /// Release palace `palace_id`'s open handles and caches; keep its data.
    ///
    /// Why: see the module doc. A palace that is not on disk is not loaded,
    /// so it answers success with `exists: false` rather than an error.
    /// What: refuses an invalid id (`BadRequest`). Waits at most
    /// [`UNLOAD_LOCK_WAIT`] for the palace's write lock; a timeout is
    /// `Conflict` and releases nothing. Under the lock: evicts the BM25 index
    /// (flushed first; unflushed text is queued for repair), drops the
    /// session-store handle and the registry handle under the raw and
    /// canonical ids. Then checks the postcondition that no handle is
    /// resident; a handle that reappeared is `Conflict`. Last, waits (again
    /// at most the lock wait) until the palace's redb files are closed, so
    /// the next use reopens it writable; a file still open is `Conflict`.
    /// Test: `unload_releases_the_handle_and_keeps_the_palace_on_disk`,
    /// `unload_of_an_absent_palace_reports_nothing_loaded`,
    /// `unload_while_a_write_holds_the_lock_is_busy_and_releases_nothing`,
    /// `unload_refuses_an_invalid_id`,
    /// `an_unloaded_palace_reopens_on_its_next_tool_call`.
    pub async fn unload_palace(&self, palace_id: &str) -> ServiceResult<UnloadOutcome> {
        self.unload_palace_within(palace_id, UNLOAD_LOCK_WAIT).await
    }

    /// [`Self::unload_palace`] with an explicit lock wait (tests shorten it).
    pub(crate) async fn unload_palace_within(
        &self,
        palace_id: &str,
        lock_wait: Duration,
    ) -> ServiceResult<UnloadOutcome> {
        let raw = palace_id.trim();
        if !trusty_common::palace_id_is_valid(raw) {
            return Err(ServiceError::bad_request(format!(
                "palace_unload: {raw:?} is not a valid palace id"
            )));
        }
        let state = &self.state;
        let canonical = canonical_palace_id(&state.data_root, raw);
        let exists = state
            .data_root
            .join(&canonical)
            .join("palace.json")
            .exists();
        let lock = state.palace_write_lock(raw);
        let _guard = tokio::time::timeout(lock_wait, lock.lock_owned())
            .await
            .map_err(|_| {
                ServiceError::conflict(format!(
                    "palace {raw:?} is busy (a write held its lock for {lock_wait:?}); \
                     nothing was unloaded, retry when idle"
                ))
            })?;
        let ids = [PalaceId::new(raw), PalaceId::new(&canonical)];
        let was_resident = ids.iter().any(|id| state.registry.peek(id).is_some());
        if let Some(lane) = state.bm25_lane() {
            for id in [raw, canonical.as_str()] {
                // #10989: flush-then-drop; unflushed text goes to repair, as on rename.
                if lane.evict_palace(id).await {
                    crate::bm25_repair::mark_dirty(state, &canonical);
                }
            }
        }
        for id in &ids {
            state.session_stores.remove(id.as_str());
            state.registry.remove(id);
        }
        // #10989 postcondition: nothing resident, or the unload did not happen.
        if ids.iter().any(|id| state.registry.peek(id).is_some()) {
            return Err(ServiceError::conflict(format!(
                "palace {raw:?} was reopened while it was being unloaded; retry"
            )));
        }
        // #10989 (as #9945 for rename): a dropped handle's writer task closes
        // its redb files later; a reopen before then is a read-only snapshot.
        let (root, keys) = (
            state.data_root.clone(),
            vec![raw.to_string(), canonical.clone()],
        );
        tokio::task::spawn_blocking(move || {
            crate::engine::registry::await_files_closed(&root, &keys, lock_wait)
        })
        .await
        .map_err(|e| ServiceError::internal(format!("palace_unload: close wait failed: {e}")))?
        .map_err(|e| {
            ServiceError::conflict(format!(
                "palace_unload: handle released, files still open: {e}"
            ))
        })?;
        Ok(UnloadOutcome {
            palace_id: raw.to_string(),
            canonical_id: canonical,
            exists,
            was_resident,
            unloaded: true,
            data_kept: true,
        })
    }
}
