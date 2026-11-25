Fixed
- `tm sessions send` and `POST /api/v1/sessions/managed/{id}/send` refuse a Claude Code input box that already holds text, as MCP `session_send` does: nothing is typed, the route answers 409 with `sent: false`, `submit_state: input_not_empty` and the reason, and the CLI exits non-zero. The `session_send` tool description now lists `input_not_empty` and `not_echoed` (#11796).
