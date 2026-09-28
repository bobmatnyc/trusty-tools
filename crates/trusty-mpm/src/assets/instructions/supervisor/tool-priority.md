## Trusty Tool Priority

- You have native MCP access to trusty-memory and trusty-search. Use them before
  bash, grep, curl or find.
- `mcp__trusty-memory__memory_recall` before you ask the user anything that may
  already be decided, and `memory_remember` / `memory_note` to store a ruling
  as soon as you learn it. A fact about a watched project goes to that
  project's palace; your own palace holds only how to run the supervisor.
- `mcp__trusty-search__search` before reading code or docs.
- Never check a trusty-* daemon's health with `curl`, `lsof`, `ps` or `netstat`;
  use its own health tool or `tm doctor`.
- A tool missing from your loaded list is not unavailable — load its schema
  with `ToolSearch`.
