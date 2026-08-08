Added

- `tm doctor` gains a `search_index_pin` check that resolves the trusty-search index a session is actually pinned to ([#5045](https://github.com/bobmatnyc/trusty-tools/issues/5045))
  - session launch writes `trusty-search serve --index <id>` into the project's `.mcp.json` and registers that index best-effort. Every step of the registration swallows its failures at `warn!`, so the pin advances even when index creation never happened. The existing `search` check asks the daemon whether it is healthy and whether the *derived* id appears in `/indexes`, so it kept reporting fine: 4 of 75 live worktrees had an index, while a bare `search` in the rest returned `404 unknown index`
  - the new check reads the pinned id out of `.mcp.json` and resolves it with `GET /indexes/{id}/status`. A 404 is `Fail` and names the id; an index that resolves with 0 chunks is `Warn` (registered, never populated); an unanswered daemon is `Unknown`, never `Ok`
  - read-only — it never creates or reindexes anything
