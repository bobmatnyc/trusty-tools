Fixed

- `tm hook --pm-guard` now denies an unisolated `documentation`,
  `version-control`, `qa`, `web-qa`, or `api-qa` dispatch that would share a
  running file-mutating agent's working tree. The classifier recognised only
  engineer-tier agents, so those five wrote into an engineer's tree on one git
  HEAD with no deny at any step (#5650).
- `code-critic` still classifies as non-mutating. It declares the same
  `role: qa` and `extends: base-qa` as the three QA writers but only reviews, so
  the QA agents are matched by name rather than by role — a role-based widen
  would have denied a review dispatched alongside the engineer it reviews
  (#5650).
- `tm-workflow` now lists which agents count as file-mutating, replacing wording
  that said QA agents get their own worktree while the guard classified `qa` as
  non-mutating (#5650).
