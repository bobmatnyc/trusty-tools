Fixed

- A forced reindex no longer leaves chunks for files that were deleted or that
  stopped matching the walker's include set. Force stages an empty corpus and
  skips the prune pass, so the promoted redb rows were already correct — but
  the warm chunk map, the BM25 index and the vector store kept the obsolete
  chunks, deferred embedding re-created their vectors, and the next graceful
  shutdown flush wrote them back into the promoted corpus. A confirmed force
  promotion now reconciles the warm state to the promoted corpus's chunk ids,
  re-confirming each candidate against the corpus immediately before dropping
  it so a concurrent `index-file` cannot lose its vector. Incremental
  reindexes are unchanged. (#7004)
