Fixed

- `tm hook --pm-guard` now denies a `git worktree add` under
  `/private/var/folders`, the spelling macOS's `/var` symlink resolves to
  (refs [#5924](https://github.com/bobmatnyc/trusty-tools/issues/5924)).
  The denylist carried `/var/folders` only, and `current_dir()` inside
  `$TMPDIR` returns the `/private/var/...` form — so a worktree target given
  relative to a `$TMPDIR` working directory matched nothing and the guard let
  it through. `/tmp` and `/private/tmp` were already paired this way; `/var`
  now is too.
- `tm project list` reads the persistent project registry
  (`GET /api/v1/projects`) instead of the daemon's in-memory, session-derived
  project map (refs
  [#5994](https://github.com/bobmatnyc/trusty-tools/issues/5994)). That map is
  rebuilt on every daemon start and seeded only from `config.yaml` and session
  history, so the command reported "no projects registered" on a host whose
  `projects.json`, `project_list` MCP tool, and `/api/v1/projects` route all
  listed five. Rows now render like `tm projects list` (name, repo URL,
  default branch) and keep the `[mcp-trusted]` marker, resolved through the
  local project-path alias store.
