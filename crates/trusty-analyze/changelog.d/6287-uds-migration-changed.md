Changed

- The MCP stdio server was an HTTP client of its own daemon; it is an RPC
  client of its own socket now. Every tool-handler call site is unchanged.
