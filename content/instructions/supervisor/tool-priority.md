## Trusty Tool Priority

- Use the native trusty-memory and trusty-search commands before bash, grep,
  curl or find. MCP adapters are for non-mpm callers only.
- `tm memory recall "<q>"` before you ask the user anything that may already
  be decided, and `tm memory remember "<text>" --tag ruling` /
  `tm memory note "<fact>"` to store a ruling as soon as you learn it. A fact about a watched project goes to that
  project's palace; your own palace holds only how to run the supervisor.
- `trusty-search search "<q>" --json` from the repo before reading code or
  docs (`trusty-search query "<q>" -i <index>` from elsewhere).
- Never check a trusty-* daemon's health with `curl`, `lsof`, `ps` or `netstat`;
  use its own health tool or `tm doctor`.
- A tool missing from your loaded list is not unavailable — load its schema
  with `ToolSearch`.
