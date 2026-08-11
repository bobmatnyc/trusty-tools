Added

- Sessions now carry their two per-project MCP pins as environment variables the
  spawn exports: `TRUSTY_MEMORY_PALACE` (the project's palace slug) and
  `TRUSTY_INDEX` (the confirmed trusty-search index id). A single shared
  user-scope declaration cannot carry a per-project argument, so these replace
  the `env` block and the `serve --index <id>` argument the deleted injectors
  wrote. Both stay gated on the same `[mcp] trusty_memory` / `[mcp] trusty_search`
  manifest toggles, and each is omitted rather than exported empty when the
  toggle is off or the value cannot be derived — so a session keeps the right
  palace (#1605) and the right index (#1373) without a workspace `.mcp.json`
  (#4181, ADR-0042).
