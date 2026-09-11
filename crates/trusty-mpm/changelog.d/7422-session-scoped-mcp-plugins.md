Changed

- A tm-launched session now loads only the trusty-* framework MCP servers, the project's own `.mcp.json`, and the servers the project's `.trusty-mpm.toml` names under `[session] mcp_servers`; every other server in the shared managed `.claude.json` is scoped out via `--strict-mcp-config` (#7422).
- Claude Code plugins default to off per project: tm writes an `enabledPlugins` map into the project's `.claude/settings.json`, `true` only for the plugins `[session] plugins` names, preserving any key tm did not write.
- `tm mcp add --project` declares a server in the project's own `.mcp.json`, and `tm mcp list` marks each shared server `opted-in` or `scoped-out` for the current project.
- `tm doctor` gains an informational `session_scope` check naming the MCP servers and plugins a project's sessions will not load, and `tm session instructions` prints the same set on stderr.
