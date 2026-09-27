Changed
- `tm session prune-worktrees` now acts only on the worktrees of the repository it is run from. The old daemon-global behaviour needs the new `--all-projects` flag, and outside a git repository the command refuses instead of widening (#8782).
- The prune-worktrees preview now lists every path it would remove, grouped by project, with the reason and a count. Worktrees whose pull-request state cannot be read (no `origin` remote, failed lookup) are listed as `unknown` and kept (#8782).
- `tm session prune-worktrees --force` previews first and then removes only the paths that preview listed. It refuses to continue when the daemon does not confirm the requested scope, which is what a daemon older than this change does (#8782).
