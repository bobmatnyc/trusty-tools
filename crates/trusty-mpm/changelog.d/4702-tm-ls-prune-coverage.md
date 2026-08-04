Fixed

- `tm ls` auto-prune no longer misses stopped sessions or non-TTY invocations, so dead records stop accumulating (closes [#4702](https://github.com/bobmatnyc/trusty-tools/issues/4702))
  - every listing surface prunes now — piped, scripted, `--json`, `tm session ls`, and bare `tm` — not just the interactive TTY picker
  - a `stopped`/`errored` record whose workspace the CLI independently verifies is gone is cleared, closing the gap where a display-reconciled zombie (persisted `active`, tmux pane gone) never got the daemon's `unresumable` probe
  - a stopped record whose workspace still exists on disk is never cleared, and `decommissioned` records are untouched
  - the prune stays record-only: it clears registry records and never removes a git worktree, a branch, or any file on disk
