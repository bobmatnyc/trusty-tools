Removed
- `TrustyAddrs::memory`, `TRUSTY_MEMORY_DEFAULT_ADDR`, and the `~/.trusty-memory/http_addr` read in `daemon::discover`. trusty-memory has no port to discover since ADR-0032; nothing read the field, and the file it read is stale on every machine that ran the old daemon
