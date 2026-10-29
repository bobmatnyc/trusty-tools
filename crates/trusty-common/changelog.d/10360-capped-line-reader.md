Added
- `stdio_mcp_client::capped_line::read_capped_line`, the cancel-safe 16 MiB-capped line reader, is public, with `MAX_LINE_BYTES` and `log_field`, so other stdio JSON-RPC clients share the same framing rules (#10360).
