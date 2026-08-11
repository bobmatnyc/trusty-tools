Fixed

- **The BM25 lexical lane addressed the wrong palace, for reads and for writes
  (part of issue #5036).** Two independent keying faults compounded.
  `AppState::bm25_client` was built once as `Bm25Client::for_palace(default_palace)`
  (`src/lib.rs:767`) and holds one fixed socket path, so every search and every
  live index write went to the DEFAULT palace's socket no matter which palace
  was being queried. And the recall handlers passed the palace slug the caller
  REQUESTED while the vector lane used the handle `open_palace` had resolved —
  which differ whenever an alias is involved. `palace_aliases.json` maps
  `bobmatnyc-trusty-tools → trusty-tools`, and the alias directory holds no
  `palace.json`, so `palace_ids_on_disk` (`src/bm25_backfill.rs:667`) never
  enumerates it: the backfill wrote the canonical palace's corpus, reported full
  coverage, and the search read a corpus nobody had written to. On the write
  path the drawer landed in another palace's corpus and the call SUCCEEDED, so
  nothing marked the palace dirty and the repair sweep never ran. Search and
  index now resolve a client bound to the palace's own socket
  (`bm25_client_for_palace`), and every call site keys on `handle.id`.
  `recall_without_embedder` no longer takes a `palace` parameter at all — it
  reads the id off the handle, so the two lanes cannot disagree by construction.
  With `TRUSTY_BM25_DAEMON` unset the lane is off and behaviour is unchanged.
