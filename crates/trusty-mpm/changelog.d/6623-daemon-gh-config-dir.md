Fixed
- The daemon's `gh` spawns (merged-PR worktree reclaim, the `worktree_disk`
  `tm doctor` check) now resolve `GH_CONFIG_DIR`/`GH_TOKEN`/`GH_HOST` from the
  active project's `github:` binding, falling back to the global `github:`
  section, exactly as an interactive `tm` invocation already does. Under
  launchd the daemon inherits neither variable, so every lookup used to exit 4
  ("gh auth login") on a host that keeps `gh` credentials in a scoped config
  directory — 261 failed lookups in one reported sweep (#6561). A resolution
  failure (an unreadable origin remote, an `account`-only binding) falls back
  to the ambient environment rather than blocking the spawn.
- The `worktree_disk` doctor check now names the resolved `gh` identity
  alongside a failed lookup, so "used no config dir at all" reads differently
  from "used the wrong one" (#6623).
