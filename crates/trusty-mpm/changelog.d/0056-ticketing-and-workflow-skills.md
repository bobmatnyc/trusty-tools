Changed

- `tm-ticketing` now carries the full issue lifecycle — `status:in-progress`
  claimed at dispatch with a dated session comment, event-driven advances to
  `status:coded` / `status:merged` / `status:tested`, and a close bar that
  requires live verification evidence. `tm-workflow` and
  `tm-delegation-patterns` record the matching `version-control` ownership: the
  merge, the post-merge verification against the exact head SHA, the merged-
  worktree reclaim, and the label advance it must flag rather than make.
