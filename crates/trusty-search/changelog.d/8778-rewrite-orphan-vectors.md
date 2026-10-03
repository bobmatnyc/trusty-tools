Fixed
- A schema migration that renamed two chunk ids onto one (M003 relativization, M005 re-chunk) no longer leaves the replaced id's vector in the HNSW graph. That orphan made the saved binary hold more vectors than its `hnsw.keys.json` sidecar, so every boot discarded the pair as torn and the index served BM25 only until it was re-embedded (#8778).
