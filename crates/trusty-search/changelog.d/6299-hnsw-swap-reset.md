Fixed
- A reindex could leave an index unable to accept any further vector write for
  the life of the daemon. Each reindex checkpoint saved to a staging file and
  recorded it as the store's snapshot source; an idle or memory-pressure
  demotion then re-viewed the store from that file; resolving the swap renamed
  or deleted it without telling the store, so every later write failed
  `usearch failed to promote view → mutable load: No such file or directory`,
  and a failed promote never clears `is_view`, so the store could not heal
  itself. Committing the swap now re-points the store at the live path, and
  aborting it restores the store from the live snapshot. As a backstop for a
  recorded snapshot that vanishes for any other reason, promoting a view whose
  file can no longer be read rebuilds the graph from the mapping still held in
  memory instead of failing forever (#6299).
