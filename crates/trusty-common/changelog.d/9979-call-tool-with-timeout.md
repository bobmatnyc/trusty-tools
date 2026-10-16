Added
- `StdioMcpClient::call_tool_with_timeout` calls one MCP tool under a caller-chosen timeout, so a tool that legitimately runs past the 30 s default (a long chat turn) is not cut off. `call_tool` and every other request keep the 30 s default (#9979).
