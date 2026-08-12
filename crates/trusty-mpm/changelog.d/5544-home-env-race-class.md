Fixed

`tm launch`, `tm connect`, daemon-managed spawns and `tm register/load/run` no
longer resolve the two user-global Claude Code files — `~/.claude.json` and
`~/.claude/settings.json` — through a path that the managed-session layout
rewrites onto the workspace. `FrameworkPaths` now carries the home tier as an
explicit `home_tier` field instead of deriving it from the agent deploy
destination, so a managed session cannot drop an untracked `.claude.json` into
the operator's repo, and the global `trusty-memory` hook cleanup targets the real
settings file rather than silently no-opping on one that does not exist.

Alongside it, the `tm` binary's own test suite stopped repointing `$HOME` and
`$CLAUDE_CONFIG_DIR` process-wide — a write that could straddle a parallel test's
agent-roster scan and report a truncated roster — and a mechanical guard now
fails the build if either comes back (#5544).
