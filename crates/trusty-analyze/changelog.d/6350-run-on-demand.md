Changed
- trusty-analyze runs on demand instead of as a resident daemon. `serve` now
  exits after ten minutes with no traffic (`TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS`
  overrides it; `0` disables the exit), unlinking its socket on the way out, and
  `trusty-analyze deep` starts the server itself rather than failing when
  nothing is listening (#6350).
  - `serve --mcp` is the exception and serves until it is signalled. The stdio
    loop that process runs dials the socket once per tool call and never
    respawns it, so an idle exit would strand a live MCP session with a
    transport error for the rest of its life (#6355).
- `trusty-analyze service install`, `service status` and `service logs` are
  removed — no LaunchAgent is installed any more. `service uninstall` remains as
  the migration: it unloads `com.trusty.analyze` and its legacy alias and deletes
  their plists. `setup daemon` runs the same eviction before doing anything else,
  so an upgrade moves off the resident unit without an explicit command (#6350).
