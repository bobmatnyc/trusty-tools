Added

- `redb_open`, behind the new light `redb-open` feature, is the workspace's
  single redb corruption / obsolete-format classifier (#5063). Five crates each
  carried a byte-identical copy of the four-arm `match` that decides whether an
  unopenable redb file may be quarantined, so a safety change to that decision
  had to land five times. The module also carries the quarantine-path helper
  (`INCOMPATIBLE_SUFFIX`, `incompatible_backup_path`,
  `backup_incompatible_file`) that two of them duplicated verbatim, numbered
  anti-clobber rule included. `redb-open` gates only `dep:redb` — the classifier
  used to sit under `memory-core`, which pulls in usearch, git2 and a bundled
  ORT embedder, which is why no consumer reused it. `memory-core` enables the
  new feature, and `memory_core::store::redb_open` re-exports every item
  unchanged, so no existing path moves. Each store's recovery POLICY stays in
  its own crate: they diverge for recorded reasons (#4227, #5064) and are
  deliberately not collapsed.
