Fixed
- `index_file` (HTTP, socket, MCP and boot reconcile) now replaces a file's earlier chunks. Before, an edit that shifted lines or renamed a symbol left the old chunks searchable beside the new ones in the corpus, BM25, HNSW and the symbol graph (#8959).
- `index_file` and `remove_file` no longer rebuild the whole symbol graph on every call. They mark the graph stale, and the daemon rebuilds it once per burst of writes: after 2 s without a write, or after 60 s of continuous writes. On a 315K-chunk index one `remove_file` took over 60 s and allocated about 1.2 GB for that rebuild (#8959, #9179).
- A restart, park or crash before a deferred symbol-graph rebuild ran no longer boots the old persisted graph as current. Each write stores a stale mark in the index's corpus before it changes anything, and the next load schedules the rebuild (#8959).
- `index_file` now fails when the file's replaced chunks cannot be deleted from the durable corpus. Before, it answered success and a restart brought the old chunks back; a retry now removes them (#8959).
- Concurrent `index_file` writes to the same path now end with only the last write's chunks on that path (#8959).
- `GET /indexes/:id/graph`, `call_chain` and `graph/neighbors` hold the index teardown guard while they flush pending graph writes, so a concurrent `DELETE` cannot remove the data directory during the rebuild (#8959).
