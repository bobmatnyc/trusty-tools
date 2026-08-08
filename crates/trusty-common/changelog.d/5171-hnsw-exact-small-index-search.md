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
  Palaces above 256 drawers keep the graph path and its recall loss, which is
  larger there, not smaller — 8.4% of queries lost a true top-5 neighbour at
  1024 points, and 1.8% lost the nearest one. That residual is unchanged by
  this fix (#5171)
- `HnswStore::search` now scores a re-embedded drawer against the vector
  `VECTORS` currently holds for it. Because `hnsw_rs` cannot remove a point, a
  re-upsert leaves the old embedding in the graph until the next palace open,
  and the drawer was ranked by whichever copy was closer — so a query matching
  text the drawer no longer holds came back at distance 0.0, which
  `VectorStore::search` reports as similarity 1.0. `palace_reembed` creates
  that state in bulk. The drawer also no longer occupies two result slots
  (#5171)
- `HnswStore::search` selects the exact path on the live drawer count rather
  than the graph's point count, so deletes and re-embeds accumulating within a
  session can no longer push a small palace back onto the approximate path
  (#5171)
- Below the threshold, `HnswStore::search` no longer trims its candidate list
  before re-scoring re-embedded drawers. A drawer that has been re-embedded is
  ranked provisionally by whichever of its two embeddings is nearer, and
  trimming on that optimistic score let a drawer whose SUPERSEDED vector sat
  near the query push a genuinely nearer drawer out of the results entirely —
  the same lost-neighbour symptom this fix exists to remove, reached through
  re-embedding rather than through the graph. `palace_reembed` puts every
  drawer in that state (#5171)
