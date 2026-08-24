Fixed

- Memory-palace vector search is now exact for every palace this fleet holds.
  `HnswStore::search` scans every point up to 4096 live drawers (was 1024), so
  the sizes that still went through the HNSW graph are answered exactly.
  Measured on this repo's own palace embeddings, queries losing a true top-10
  neighbour fell from 28.1% to 0% at 1025 live vectors, 14.9% to 0% at 1354,
  14.9% to 0% at 2048 and 20.5% to 0% at 2879 — the largest palace on this
  machine holds about 2900. The scan is linear, so a query over 4096 points
  costs about 1.4ms against 0.43ms over 1024; above 4096 the graph arm is still
  approximate. Nothing migrates: no graph is persisted, and every palace open
  rebuilds the index from its stored vectors. (#5179)
