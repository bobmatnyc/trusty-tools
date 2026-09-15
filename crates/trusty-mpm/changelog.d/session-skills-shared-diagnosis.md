Documentation

- `tm-session-pause` and `tm-session-resume` now share one MCP-tool
  failure-diagnosis procedure, stated once in `tm-session-management`
  ("MCP Session-Tool Failure Diagnosis"), instead of carrying near-verbatim
  copies
  - each skill keeps only its own mandatory `ToolSearch` load line and its
    own fallback specifics (pause's hand-written snapshot format, resume's
    CLI `tm session catchup` fallback)
