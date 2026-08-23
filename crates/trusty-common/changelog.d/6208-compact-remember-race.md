Fixed

- Vector reclamation no longer deletes the vector of a drawer whose `remember`
  is still in flight. A remember upserts the new drawer's vector before it
  registers the drawer, and three reclamation paths snapshotted the valid-drawer
  set and reclaimed "orphans" without the palace write lock: `palace_compact`,
  the idle dream cycle's `compact_pass`, and its fallback index rebuild
  (`rebuild_index_from_drawers`, which `reset()`s the whole index and re-adds
  only the snapshotted drawers). A reclamation landing in the upsert→register
  window saw the vector with no matching drawer and dropped it permanently. All
  three now hold the write mutex across the snapshot and the reclamation
  (`palace_compact` via the new `PalaceHandle::compact_vector_orphans`), so a
  mid-flight drawer's vector can never be dropped under any interleaving (#6208).
