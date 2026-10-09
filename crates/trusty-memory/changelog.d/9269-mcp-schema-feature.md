Added
- New `mcp-schema` feature: compiles only the MCP tool schema (`tools::tool_definitions*`, `openrpc`) and `MemoryMcpService`, for a host that merges the schema into its own `rpc.discover` without linking the daemon. `server` implies it. The default build and its generated README tool table are unchanged (#9269).
