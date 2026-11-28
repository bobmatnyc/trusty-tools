Fixed

- The usearch re-link warnings after an HNSW rebuild ("re-linked N stranded vectors", "N vectors still stranded") now carry a `path` field naming the index's snapshot file, so a warning can be traced to its index (Refs #11891).
