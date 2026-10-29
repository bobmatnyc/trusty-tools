Fixed
- `StdioMcpClient` keeps the partial bytes of a stdout line when a call times out mid-line, so the next call resumes that line instead of failing with "parsing JSON-RPC frame" on its tail. A skipped frame that carries the awaited id now logs at `warn` with its id and method (#10360).
- `stdio_mcp_client::is_response_to` is now public, so other stdio JSON-RPC clients apply the same response rule (#10360).
- `StdioMcpClient` caps a buffered stdout line at 16 MiB. A longer line fails the call with an error naming the cap, and its remaining bytes are discarded up to the next newline. Logged frame ids, methods and unparsable lines are cut to 64 characters (#10360).
