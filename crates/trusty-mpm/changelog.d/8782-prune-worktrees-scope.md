Changed
- `tm session prune-worktrees` now acts only on the worktrees of the repository it is run from. The old daemon-global behaviour needs the new `--all-projects` flag, and outside a git repository the command refuses instead of widening (#8782).
- The prune-worktrees preview now lists every path it would remove, grouped by project, with the reason and a count. Worktrees whose pull-request state cannot be read (no `origin` remote, failed lookup) are listed as `unknown` and kept (#8782).
- `tm session prune-worktrees --force` previews first and then removes only the paths that preview listed, each pass bounded by its own list. It refuses to continue when the daemon does not confirm the requested scope, which is what a daemon older than this change does (#8782).
- Pausing a PM session (`session_context_pause`) now prunes orphaned worktrees only in the paused project, and prunes nothing when the project directory is outside a git checkout. It used to sweep every registered project (#8782).
- Under `--discard-dirty` the prune preview says which orphaned worktrees hold unsaved work that the removal will discard, instead of reporting "no unsaved work" for them (#8782).
- A scoped prune run from a checkout the daemon does not scan now says so instead of printing a bare `total: 0`, and `tm doctor` suggests `tm session prune-worktrees --all-projects` (#8782).
