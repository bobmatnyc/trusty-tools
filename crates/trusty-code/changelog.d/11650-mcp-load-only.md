Added
- `McpToolSet::load_only` and `McpToolSet::load_at_only` take a server-name filter and never spawn a server the filter rejects; skipped servers are left out of the diagnostics and named in one debug line (#11650).
