Changed
- `MEMORY_SET` no longer lists `trusty-bm25-daemon`. #5329 removed that binary from trusty-memory's install surface, so `tctl sign trusty-memory` signs `trusty-memory` and the deprecated `trusty-memory-mcp-bridge` shim only.
