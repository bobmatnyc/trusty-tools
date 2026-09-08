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
//! changed since the cached entry was written);
//! [`SessionManager::residency_cache_evict`] drops a session's entry when the
//! session reaches a terminal state, so the map is bounded by the live record
//! count rather than by every session the daemon has ever seen (#7087).
//! Test: `residency_generation_starts_at_zero_and_bumps`,
//! `residency_cache_hits_on_matching_root_and_misses_on_change`,
//! `residency_cache_evict_drops_the_entry` below; generation bumps at the
//! production call sites are covered by
//! `generation_increments_across_create_and_stop`,
//! `generation_increments_across_resume`,
//! `generation_increments_across_mark_reactivated`,
//! `generation_increments_across_mark_runtime_exited_stopped`,
//! `generation_increments_across_forced_delete` and
//! `forced_delete_evicts_the_residency_cache_entry` in
//! `daemon::managed_routes::residency`'s route tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::manager::SessionManager;
use super::record::ManagedSessionId;

impl SessionManager {
    /// Bump the residency generation and return the NEW value.
    ///
    /// The rule: call this wherever a persisted record enters or leaves the
    /// set `mpm.residency.active` serves — the route keeps exactly the records
    /// whose state is `Active` or `Provisioning`, so every transition across
    /// that boundary in either direction is a set change a polling consumer
    /// must be able to notice. Entering: `create`, `adopt_existing`, `resume`
    /// (`Stopped`/`Errored -> Active`) and `mark_reactivated`
    /// (`Stopped`/`Decommissioned -> Active`). Leaving: `stop`/
    /// `stop_with_cause`, `mark_runtime_exited_stopped`, both decommission
    /// paths (record-only and full teardown), `delete_record` (`--force`
    /// reaches it straight from `Active`/`Provisioning`), and
    /// `reconcile_on_boot`'s leaked-test-adoption sweep (#6116).
    ///
    /// The generation is advisory-monotonic, never a set hash: bumping when
    /// the set happens to be unchanged only costs a consumer one redundant
    /// diff, while a missed bump serves a stale set forever.
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

    /// Drop a session's cached derivation.
    ///
    /// Why: the cache is keyed by [`ManagedSessionId`] and nothing else would
    /// ever remove a key, so without this it grows with the number of sessions
    /// the daemon has ever served — a leak measured in historical sessions,
    /// not live ones. Same shape as [`super::slots::SlotRegistry::release`]:
    /// an in-memory-only map whose one removal point is the moment the record
    /// behind the key stops being something the map can be asked about.
    /// What: removes `id`'s entry. An id the cache never held is a no-op —
    /// most sessions never reach a residency poll, so a miss is the norm.
    /// Called from the terminal transitions (`delete_record`, both
    /// decommission paths); a `Stopped` session keeps its entry because a
    /// resume re-enters the set at the same root.
    /// Test: `residency_cache_evict_drops_the_entry` below;
    /// `forced_delete_evicts_the_residency_cache_entry` in
    /// `daemon::managed_routes::residency`'s route tests covers the
    /// production call site.
    pub async fn residency_cache_evict(&self, id: &ManagedSessionId) {
        self.residency_cache.write().await.remove(id);
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

    #[tokio::test]
    async fn residency_cache_evict_drops_the_entry() {
        let dir = crate::test_support::hermetic_temp_dir();
        let (mgr, _fake) = make_manager(&dir).await;
        let id = ManagedSessionId::new();
        let root = PathBuf::from("/repo/evict-me");

        mgr.residency_cache_store(id, root.clone(), Some("palace-1".to_string()), Vec::new())
            .await;
        assert!(mgr.residency_cache_lookup(&id, &root).await.is_some());

        mgr.residency_cache_evict(&id).await;
        assert_eq!(mgr.residency_cache_lookup(&id, &root).await, None);

        // Evicting an id the cache never held is a no-op, not a panic.
        mgr.residency_cache_evict(&ManagedSessionId::new()).await;
    }
}
