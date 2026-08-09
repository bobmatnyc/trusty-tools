Added

- `tm launch --worktree`, and the matching `worktree` field on
  `POST /api/v1/sessions/managed`: the launch-time request that puts one
  specific session in its own protected clone plus worktree, overriding the
  main-checkout default.
