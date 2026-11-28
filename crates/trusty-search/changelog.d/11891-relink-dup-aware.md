Fixed

- The usearch re-link sweep after an HNSW rebuild no longer reports exact-duplicate vectors as stranded: a key whose self-search returns a key holding the identical stored vector counts as reachable, so more than 10 duplicates no longer re-insert on every pass and warn "still stranded". A distinct stranded vector is still re-linked. Each sweep that sees a strand or a tie now logs one `re-link sweep` line with `path`, the per-pass strand counts (`passes`), `duplicate_ties`, `graph_stranded` and `left` (Refs #11891).
