Breaking
- `trusty-search serve` speaks MCP over stdio only. `--with-http`, `--http <addr>` and `--port` exit 1 naming #9542 and bind nothing; the MCP HTTP/SSE listener and its `~/.trusty-search/mcp_http_addr` file are gone (ADR-0032). There is no MCP over HTTP: trusty-console has no MCP endpoint. Use stdio (`trusty-search serve`) or the daemon's Unix socket.
