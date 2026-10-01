Added
- The `index-file` response carries `chunks`, the number of chunks the write committed; a tombstone write also carries `removed: true`. The `index_file_failed` error body now carries `indexed: false`. Existing fields are unchanged (#8976).
- `CodeIndexer::index_file_outcome` returns an `IndexFileOutcome` (`Indexed`, `Empty`, `NoChunks`, `Removed`); `index_file` keeps its `Result<()>` signature (#8976).
