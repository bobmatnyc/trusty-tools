Added

- `tm hook --pm-guard` now denies a second concurrent file-mutating `Agent`/`Task`
  dispatch into a working directory that already has one, unless the dispatch
  declares `isolation: "worktree"` (or `"remote"`). Two subagents sharing the PM's
  directory race over one git HEAD, and git does not refuse the collision — a
  `git checkout -b` only refuses when a tracked file differs between both branches
  AND has an uncommitted change, so untracked files transfer onto the wrong branch
  silently. The guard fires only for bundled engineer-tier agents and fails open
  everywhere else: a read-only dispatch, an unknown agent name, an unresolvable
  working directory, and an unreachable daemon all allow the dispatch (#4480).
  - Delegation records now carry the `isolation` mode a dispatch declared, and the
    daemon serves `GET /api/v1/sessions/{id}/delegations/shared-tree-writers` so the
    guard can ask which agents are already writing in a directory rather than guess
    from a timer.
