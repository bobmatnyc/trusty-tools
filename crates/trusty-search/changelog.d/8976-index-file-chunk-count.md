Added
- The `index-file` response carries `chunks`, the number of chunks the write committed; a tombstone write also carries `removed: true`. The `index_file_failed` error body now carries `indexed: false`. Existing fields are unchanged (#8976).
- `CodeIndexer::index_file_outcome` returns an `IndexFileOutcome` (`Indexed`, `Empty`, `TooLarge`, `NoChunks`, `Removed`); `index_file` keeps its `Result<()>` signature (#8976).
- A `.json` file above 50 windows (10,000 lines) is not chunked; `index-file` answers `indexed: false` with `reason: too_large` (#8976).
- The `index_file` MCP tool description names `indexed: false`, its `reason` values, and the tombstone reply (#8976).
