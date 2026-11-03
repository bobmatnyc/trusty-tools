Added
- `search.index.status` reports `identity: {instance_id, generation}` (#10738). `instance_id` is 128 random bits, kept across restarts, and replaced when the index is deleted and re-created. `generation` starts at 1 and rises after a force reindex, a reindex that resets the hash cache, a migration fault, or a corpus that fails to open. Both persist in `<data_dir>/index-identity/<id>.json`.
- The `search.index.file.put` reply adds `content_sha256` (SHA-256 of the indexed content) and `identity` (#10738).
- New socket method `search.index.file.status` takes `{index_id, body: {path}}` and returns `{path, present, content_sha256, chunk_count, identity}` (#10738). An unknown path answers `present: false` with a null hash; an unknown index answers not-found.
