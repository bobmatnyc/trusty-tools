Fixed

- `PalaceHandle::open_with_intent` no longer stamps `last_accessed` to the open time — it starts at "never accessed" instead, so a restart's hydration burst (`load_palaces_from_disk` opening every persisted palace) no longer reads as a burst of fresh recalls to `evict_idle`. Every genuine access still calls `PalaceHandle::touch`, which continues to make a handle recent. `PalaceHandle::new` (in-memory / test handles) is unchanged. Slice 0 of #7087.
