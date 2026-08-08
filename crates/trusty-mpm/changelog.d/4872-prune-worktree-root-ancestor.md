Fixed

- `tm ls` auto-prune now clears dead session records whose git worktree was fully removed (closes [#4872](https://github.com/bobmatnyc/trusty-tools/issues/4872))
  - `git worktree remove` deletes leaf and parent together, so the old immediate-parent probe read the removal as an unmounted volume and the record could never advance — it was never even marked in `auto-prune-seen.json`
  - absence is now corroborated against the nearest surviving worktree root (`.base/.worktrees`, `.claude/worktrees`); when that root is gone too, the record is still kept, so an unplugged external volume cannot mass-tombstone the sessions on it
