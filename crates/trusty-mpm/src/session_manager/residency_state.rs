//! Residency-generation counter and per-session derivation cache for
//! [`SessionManager`] (#7087 slice 1b).
//!
//! Why: `mpm.residency.active` publishes
//! [`trusty_common::residency::ActiveProjectSet::generation`] so a future
//! consumer can skip redundant pin/evict work on a set it has already applied,
//! and must not re-run `resolve_palace_slug`/`derive_project_index_id` — both
//! filesystem- (the second, `git`-) touching — on every poll of an
//! otherwise-unchanged session. Both concerns are the same shape as
//! `slots.rs`'s in-memory-only registry: state that lives for the daemon
//! process, is never persisted, and is owned by [`SessionManager`] so every
//! route reads the same instance instead of each deriving its own. Extracted
//! to its own file (rather than added to `manager.rs`) to keep that file under
//! its 500-SLOC production cap.
//! What: [`SessionManager::bump_residency_generation`] /
//! [`SessionManager::residency_generation`] wrap the atomic counter;
//! [`SessionManager::residency_cache_lookup`] /
//! [`SessionManager::residency_cache_store`] wrap the per-session
//! `(root, palace_id, index_ids)` cache, keyed by [`ManagedSessionId`] and
//! invalidated by a root mismatch (the session's `workspace_path`/`cwd`
//! changed since the cached entry was written).
//! Test: `residency_generation_starts_at_zero_and_bumps`,
//! `residency_cache_hits_on_matching_root_and_misses_on_change` below;
//! generation bumps at the four call sites are covered by
//! `residency_generation_bumps_across_create_and_stop` and
//! `daemon::managed_routes::residency`'s route tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::manager::SessionManager;
use super::record::ManagedSessionId;

impl SessionManager {
    /// Bump the residency generation and return the NEW value.
    ///
    /// Called at every site that changes which sessions are active: `create`,
    /// `stop`/`stop_with_cause`, `adopt_existing`, and both decommission paths
    /// (record-only and full teardown). Never called by `resume`/
    /// `mark_reactivated` — neither changes the active SET, only whether an
    /// already-active-or-not session's runtime is running.
    pub fn bump_residency_generation(&self) -> u64 {
        self.residency_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Read the current residency generation without bumping it.
    pub fn residency_generation(&self) -> u64 {
        self.residency_generation.load(Ordering::SeqCst)
    }

    /// Look up a session's cached `(palace_id, index_ids)` derivation.
    ///
    /// `None` on a cache miss OR when the cached entry's root no longer
    /// matches `root` (the session's `workspace_path`/`cwd` changed since the
    /// entry was written) — both read as "derive it fresh."
    pub async fn residency_cache_lookup(
        &self,
        id: &ManagedSessionId,
        root: &Path,
    ) -> Option<(Option<String>, Vec<String>)> {
        let cache = self.residency_cache.read().await;
        cache
            .get(id)
            .filter(|(cached_root, _, _)| cached_root == root)
            .map(|(_, palace_id, index_ids)| (palace_id.clone(), index_ids.clone()))
    }

    /// Store a session's derivation, keyed by its id.
    pub async fn residency_cache_store(
        &self,
        id: ManagedSessionId,
        root: PathBuf,
        palace_id: Option<String>,
        index_ids: Vec<String>,
    ) {
        self.residency_cache
            .write()
            .await
            .insert(id, (root, palace_id, index_ids));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::tests::make_manager;
    use super::ManagedSessionId;

    #[tokio::test]
    async fn residency_generation_starts_at_zero_and_bumps() {
        let dir = crate::test_support::hermetic_temp_dir();
        let (mgr, _fake) = make_manager(&dir).await;

        assert_eq!(mgr.residency_generation(), 0);
        assert_eq!(mgr.bump_residency_generation(), 1);
        assert_eq!(mgr.bump_residency_generation(), 2);
        assert_eq!(mgr.residency_generation(), 2);
    }

    #[tokio::test]
    async fn residency_cache_hits_on_matching_root_and_misses_on_change() {
        let dir = crate::test_support::hermetic_temp_dir();
        let (mgr, _fake) = make_manager(&dir).await;
        let id = ManagedSessionId::new();
        let root = PathBuf::from("/repo/checkout");

        assert_eq!(mgr.residency_cache_lookup(&id, &root).await, None);

        mgr.residency_cache_store(
            id,
            root.clone(),
            Some("palace-1".to_string()),
            vec!["idx-1".to_string()],
        )
        .await;

        assert_eq!(
            mgr.residency_cache_lookup(&id, &root).await,
            Some((Some("palace-1".to_string()), vec!["idx-1".to_string()]))
        );

        // The root changed (e.g. the session's workspace_path was reassigned)
        // — the stale entry must not be served.
        let other_root = PathBuf::from("/repo/other-checkout");
        assert_eq!(mgr.residency_cache_lookup(&id, &other_root).await, None);
    }
}
