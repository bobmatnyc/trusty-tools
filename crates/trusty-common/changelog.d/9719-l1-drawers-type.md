Breaking
- memory_core: `PalaceHandle::l1_drawers` changed type from `Vec<Drawer>` to `Arc<RwLock<Vec<Drawer>>>`, and `PalaceHandle::refresh_l1` now takes `&self` (#9719). Read it with `handle.l1_drawers.read()`.
