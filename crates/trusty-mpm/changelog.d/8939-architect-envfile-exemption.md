Changed

- The pm-guard secret-file rule lets the Architect's main thread run
  `tm env keys` and `tm env set` on a `.env`, `.env.local` or `.env.*` file
  inside its project directory (`CLAUDE_PROJECT_DIR`) or a root listed in
  `[supervisor] projects` of `~/.trusty-mpm/config.toml`, with and without a
  bypass. `~/.trusty-mpm/project-paths.json` grants no scope, and an
  unreadable or malformed config leaves only the project directory in scope.
  Each such call writes an `architect-envfile` audit line with the path and
  key names only. A subagent, a PM, a piped or wrapped form, the `Write` tool
  and every command that prints a value stay denied.
