Added

- `tm memory recall-deep`, `recall-all`, `list`, `prompt-context`, `task add|list|complete` and `kg assert|query|gaps` call the matching trusty-memory tools over the daemon socket, so they work without an MCP connection. Each takes `--json` and `--memory-socket`; the palace-scoped verbs also take `--palace` (#11625).
