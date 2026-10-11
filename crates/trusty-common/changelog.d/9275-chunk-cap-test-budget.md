Fixed
- The drawer chunk-cap test runs its accept/refuse boundary at a cap of 32 through a new `HnswStore::replace_group_capped` seam, so it no longer outruns the 60 s HNSW op budget on CI; the production cap stays 4096 (#9275).
