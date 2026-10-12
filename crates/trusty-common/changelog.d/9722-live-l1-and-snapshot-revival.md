Fixed
- memory_core: L1 now follows the drawer table on a live palace handle; a drawer remembered, forgotten or imported after open reaches `retrieve_l0_l1` without a reopen (#9719). `PalaceHandle::l1_drawers` is now `Arc<RwLock<Vec<Drawer>>>` and `refresh_l1` takes `&self`.
- memory_core: a stale second palace handle can no longer revive a forgotten drawer at the next open. The first maintainer open persists any snapshot-only drawers and stamps a `drawers_authoritative` marker in `kg_schema`; a stamped palace no longer gap-fills its drawer table from `l1_cache.json` (#9722).
