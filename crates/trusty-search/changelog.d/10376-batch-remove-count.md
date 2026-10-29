Fixed
- `remove_files` reports `removed_files` and `removed_chunks` for the chunk rows it deleted, not the rows it planned, so the counts match what `list_chunks` no longer returns (#10376).
