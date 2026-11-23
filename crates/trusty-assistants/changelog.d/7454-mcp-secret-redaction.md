Security

- `GET /api/assistants/{id}/mcp` no longer returns MCP transport secrets (#7454). `McpTransport`'s `Serialize` is deliberately unredacted so the config file round-trips, so serialising a resolved server straight into the response returned every inline `env` value and `headers` bearer token in plaintext over the loopback API. Key names still render; every value is replaced with `<redacted>`. `PUT` refuses a body that sends that marker back, and the Knowledge pane now omits `servers` when it edits only the disable list, so a redaction can never be written over a working credential.
