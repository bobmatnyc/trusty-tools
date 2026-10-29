Fixed
- `tm build-lease` gives a build in a linked worktree a pool slot when `CARGO_TARGET_DIR` is unset, so an agent session no longer cold-builds into the worktree's own `target/`. A main checkout keeps cargo's own directory; a slot that cannot be used falls back to it with a warning (#10215).
- New `tm doctor` row `session_target_dir` warns when this session's builds land in cargo's `./target` or inside a checkout of the repo, and lists linked worktrees that hold a local `target/`, with the fix. A failed probe reports UNKNOWN, never OK (#10215).
