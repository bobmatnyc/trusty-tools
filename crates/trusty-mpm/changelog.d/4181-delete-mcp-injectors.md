Removed

- tm no longer writes MCP server declarations into a session's workspace
  `.mcp.json`, and no longer pre-approves any MCP server name via
  `enabledMcpjsonServers`. The five injectors, their two call sites, the
  `.mcp.json` git-exclusion guard they needed, and the whole trust derivation
  behind them are deleted. MCP servers are declared once in
  `<CLAUDE_CONFIG_DIR>/.claude.json`'s user-scope `mcpServers` map — where
  `tm mcp add` already writes and where a relocated spawn reads them with no
  approval prompt. An approval left by an earlier version is stripped from the
  project entry on the next launch: an approved name is what lets a repo's own
  `.mcp.json` entry override the operator's declaration, so leaving stale ones
  in place would keep that displacement alive (#4181, ADR-0042).
