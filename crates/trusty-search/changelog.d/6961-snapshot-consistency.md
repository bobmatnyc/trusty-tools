Fixed

- HNSW snapshots now capture the graph and key maps at one completed mutation boundary, preventing concurrent indexing from producing a binary that warm boot rejects against its sidecar. Key rewrites stay dirty until persisted, and snapshot publication stages both files, restores the previous sidecar on binary rename failure, and preserves deletion accounting until publication succeeds ([#6961](https://github.com/bobmatnyc/trusty-tools/issues/6961)).
