Changed

- The four framework MCP servers (`trusty-memory`, `trusty-mpm`,
  `trusty-review`, `trusty-search`) are now declared once in the user-scope
  `mcpServers` map of the tm-owned `.claude.json`, seeded on every launch by
  `mcp_config::seed_builtin_servers`. Seeding is insert-if-absent: an entry the
  operator registered under one of those names via `tm mcp add` is never
  overwritten, reordered, or removed. This replaces the write to
  `<CLAUDE_CONFIG_DIR>/.mcp.json`, which no session ever read — Claude Code
  discovers a `.mcp.json` by walking up from the session's cwd, and cwd is
  always the repo (#4181, ADR-0042).
- Seeding never quarantines. A malformed `.claude.json`, or one whose
  `mcpServers` is not an object, makes it warn and return without renaming or
  writing anything; a failure of any kind is absorbed so the agent and skill
  deploy that follows still runs. The per-launch injectors and the
  `enabledMcpjsonServers` approval are untouched here and are deleted together
  in the next PR of the ADR-0042 sequence.
