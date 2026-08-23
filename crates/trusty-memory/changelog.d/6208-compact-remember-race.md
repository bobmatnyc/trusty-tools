Fixed

- `palace_compact` now routes through `PalaceHandle::compact_vector_orphans`, which holds the palace write lock across the orphan snapshot and reclaim — closing a race where a concurrent `remember` could have its just-upserted vector reclaimed as a false orphan before its drawer record was pushed (#6208).
