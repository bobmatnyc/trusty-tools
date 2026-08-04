Fixed

- `tm ls` auto-prune no longer misses stopped sessions or non-TTY invocations, so dead records stop accumulating (closes [#4702](https://github.com/bobmatnyc/trusty-tools/issues/4702))
  - every listing surface prunes now — piped, scripted, `--json`, `tm session ls`, and bare `tm` — not just the interactive TTY picker
  - a `stopped`/`errored` record whose workspace the CLI independently verifies is gone is cleared, closing the gap where a display-reconciled zombie (persisted `active`, tmux pane gone) never got the daemon's `unresumable` probe
  - a stopped record whose workspace still exists on disk is never cleared, and `decommissioned` records, `attached` records, and any record naming a live tmux session are untouched
  - a workspace is only "gone" when its parent directory still exists, so an unmounted volume no longer reads as every session on it being dead
  - confirmation requires 10 minutes of real elapsed age since the FIRST sighting; an intervening listing no longer resets that clock, which would otherwise leave auto-prune inert under any `tm ls` cadence tighter than the window
  - when tmux cannot be enumerated at all, nothing is pruned — without a liveness signal an `errored` record with a live pane could be tombstoned into a terminal state
  - the prune clears registry records only: never a git worktree, a branch, a file on disk, a live runtime, or a search index
