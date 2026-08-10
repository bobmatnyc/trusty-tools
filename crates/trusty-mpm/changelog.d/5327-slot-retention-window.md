Changed

- A decommissioned or deleted session releases its `tm ls` `NUM` after 24 hours
  instead of 7 days (`TERMINAL_RECORD_RETENTION_DAYS`, 7 → 1).
- The retention guard that spared a record whose workspace directory still
  existed now spares only a record whose workspace is a session WORKTREE —
  recognised by the `.worktrees/<leaf>` shape or a `.trusty-mpm-worktree`
  ownership sentinel. A session launched on a project's main checkout records
  that checkout as its workspace, and the old guard read it as a worktree to
  protect, so such records were never evicted at any window. Records from
  `tm launch --worktree` are protected exactly as before, at any age.
