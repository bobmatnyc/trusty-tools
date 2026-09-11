Changed

- A tm-launched session now loads only the trusty-* framework MCP servers plus, in a project the operator has trusted with `tm project trust`, that project's own `.mcp.json` and the servers its `.trusty-mpm.toml` names under `[session] mcp_servers`; every other server in the shared managed `.claude.json` is scoped out via `--strict-mcp-config` (#7422).
- Both in-repo surfaces are gated on the project-trust store because each ships with a clone: an untrusted repository's `.mcp.json` can no longer run a command in a pane launched with `--dangerously-skip-permissions`, and its `[session] mcp_servers` list can no longer decide which of the operator's credentialed shared servers load.
- The composed config is written to `~/.trusty-tools/trusty-mpm/session-mcp/<workspace hash>.json` at mode `0600` instead of into the project working tree, because it copies each server's `env` and `headers` verbatim.
- Claude Code plugins default to off per project: tm writes an `enabledPlugins` map into the project's `.claude/settings.json`, `true` only for the plugins `[session] plugins` names, preserving any key tm did not write.
- `tm mcp add --project` declares a server in the project's own `.mcp.json` and now requires the project to be trusted first; `tm mcp list` marks each shared server `opted-in` or `scoped-out` for the current project.
- `tm doctor` gains an informational `session_scope` check naming the MCP servers and plugins a project's sessions will not load, and `tm session instructions` prints the same set on stderr.
