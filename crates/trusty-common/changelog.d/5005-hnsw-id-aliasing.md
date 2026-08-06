Fixed

- `HnswStore` no longer aliases vector ids across two live stores over one palace file, which silently overwrote one drawer's embedding with another's (closes [#5005](https://github.com/bobmatnyc/trusty-tools/issues/5005))
  - the vector-id counter now lives in redb (`vector_id_seq`) and is reserved inside the same write transaction as the insert, so every writer on the file serialises against it; an existing palace has its counter seeded to the file's high-water mark on open, and re-raised on every subsequent open so a rolling upgrade cannot leave it behind
  - `upsert` refuses an id that already has a `VECTORS` row: it allocates past it, or fails with `IdAllocationFailed` — it never overwrites
  - `PalaceHandle::embed_health` and `palace_reembed` now report `vector_key_rows`, `distinct_vector_ids`, and the aliased drawer ids; key presence alone reported a false all-clear for this class, and `is_healthy()` is now false when any drawer is aliased
