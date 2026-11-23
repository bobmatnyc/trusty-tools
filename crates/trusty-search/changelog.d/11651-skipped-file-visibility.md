Added
- `index_status` (HTTP, MCP, socket) reports `skipped_files` (path, size_bytes, reason `exceeds_size_limit`; first 100) and `skipped_for_size_total` for files the walker refused for size, which were previously dropped with no record (#11651).
