Added
- `crate::mcp` loads the MCP servers configured in the shared
  `~/.trusty-tools/mcp/servers.toml` (the file trusty-agents already reads) plus
  a `<project>/.trusty-code/mcp.toml` override tier, spawns each enabled stdio
  server in isolation, and registers its tools as `mcp__<server>__<tool>` in the
  engineer's registry. A project entry is honoured only when it is
  content-equivalent to a global one, or only disables/re-enables it; anything
  that introduces or alters a command is refused with a reported reason. A
  broken config file yields zero servers AND a visible finding, never a silent
  empty catalog. Remote (`http`/`sse`) transports are reported as unsupported
  rather than dropped (#5428).
