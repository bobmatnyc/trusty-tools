Fixed

- `HnswStore::search` now ranks by scanning every point when the index holds
  256 or fewer, instead of traversing the HNSW graph. The traversal returned
  only what was reachable from its descent pivot along pruned layer-0 neighbour
  lists, so a genuinely relevant drawer could be absent from the candidate
  pool — and because `hnsw_rs` seeds its level RNG from OS entropy, *which*
  drawer went missing changed on every palace open. Measured against
  brute-force truth on real 384-dim palace embeddings, 2–4% of queries lost a
  true top-5 neighbour at 6–16 points; recall is now exact below the threshold.
  The scan is also faster than the traversal it replaces at these sizes
  (27µs vs 34µs at 64 points, 45µs vs 56µs at 128); at 256 it costs 5% more.
  Larger indexes are unchanged (#5171)
- `HnswStore::search` no longer returns the same drawer twice when a re-upsert
  has left a shadow copy of its vector in the in-memory graph (#5171)
