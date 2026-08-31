Fixed

- Tier C: a `fact_key` slot's incumbent is now looked up in redb when the
  in-memory drawer table does not hold it, so the newcomer retires it instead of
  joining it. Before, an incumbent missing from that table — left by a commit
  whose mirror never ran (#6366) or a degraded open that skipped rows (#6201) —
  read as an empty slot, and two drawer rows ended up carrying one live
  `fact_key` with nothing able to retire the stranded one (#6438).
- `KgStoreRedb::load_drawer` / `KnowledgeGraph::load_drawer` point-read one
  drawer row by id. `Ok(None)` means no such row; a row that exists but cannot be
  decoded returns `Err` rather than reading as absent, so an unreadable incumbent
  fails the write instead of admitting a second live claimant.
