Added

- `tm sessions catchup --json [--sessions-offset <n>]` prints the paged JSON
  payload of the `session_context_catchup` MCP tool, so a session that lost the
  MCP server can still page its catch-up rather than read one unbounded
  markdown digest. The `tm-session-resume` skill names the new fallback.
