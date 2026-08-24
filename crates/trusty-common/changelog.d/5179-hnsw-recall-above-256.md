Fixed

- Memory-palace vector search no longer loses true nearest neighbours as a
  palace grows past 256 drawers. `HnswStore::search` now answers exactly by
  scanning every point up to 1024 live drawers (was 256), and above that scales
  its HNSW `ef_search` with the live-drawer count instead of holding it at 64.
  Measured on this repo's own palace embeddings, recall@10 misses fell from
  17.4% of queries to 0% at 512 drawers, from 24.5% to 0% at 1024, and from
  23.3% to 17.7% at 2048. Recall above 1024 drawers is improved, not closed —
  and the wider search costs more per query there, roughly 0.5ms to 2.2ms at
  2900 drawers. (#5179)
- A palace above the exhaustive threshold that has deleted drawers within a
  session can no longer return fewer results than asked for. The graph arm cut
  its candidate list to `2k` before filtering tombstoned ids, so deleted drawers
  spent the caller's result slots and a palace whose nearest neighbours had been
  deleted could return nothing at all while live neighbours sat in the index.
  Tombstones are now filtered before the candidate pool is judged deep enough,
  and the pool widens until `k` live drawers survive. (#5179)
