Changed

- `tm launch` and `tm connect` now point the session they spawn at the tm-owned
  `CLAUDE_CONFIG_DIR` (`~/.trusty-tools/trusty-mpm/claude-config`) and carry
  `--setting-sources user,project,local`, matching the daemon-managed path.
  Under ADR-0042 the MCP declaration lives once in the `user` tier of that
  directory's `.claude.json`, and a session that neither relocates nor loads
  `user` reads no MCP servers at all. #1269's isolation guarantee is unchanged:
  the `user` tier now resolves to the tm-owned config home rather than the
  operator's `~/.claude`, so the operator's global settings and hooks stay out
  by relocation instead of by exclusion. When the home cannot be resolved,
  both commands fall back to the previous `--setting-sources project,local`
  with no relocation (#4181).
- Both commands seed workspace trust into `<CLAUDE_CONFIG_DIR>/.claude.json`
  instead of `~/.claude.json` — the file a relocated session actually reads.
  The four framework MCP builtins enter `enabledMcpjsonServers` only when that
  run's injector reported an actual write, carried through from the same run's
  `PrepReport` (#3950's contract, unchanged by the move) (#4181).
