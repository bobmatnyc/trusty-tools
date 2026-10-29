Fixed
- `StdioMcpClient` keeps the partial bytes of a stdout line when a call times out mid-line, so the next call resumes that line instead of failing with "parsing JSON-RPC frame" on its tail. A skipped frame that carries the awaited id now logs at `warn` with its id and method (#10360).
- `stdio_mcp_client::is_response_to` is now public, so other stdio JSON-RPC clients apply the same response rule (#10360).
