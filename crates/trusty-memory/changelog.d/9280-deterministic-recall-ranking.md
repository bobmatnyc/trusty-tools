Fixed
- Recall ranks tied drawers by score, then layer, then drawer id, in both the BM25 fusion sort and the stale-snapshot demotion sort. Before, tied drawers kept the vector lane's order, which a reopened palace could change (#9280).
