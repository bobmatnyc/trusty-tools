Changed
- `tm hook --pm-guard` now lets a dispatched `version-control` agent run
  `git worktree remove`, which #5791 denied to every agent. The grant is gated
  on five re-checks the guard makes itself and never takes from the caller: the
  payload carries an `agent_id` (an `agent_type` naming `version-control` on its
  own is refused, since a top-level `--agent` session carries the same field);
  the target resolves under `.claude/worktrees/` or `.worktrees/`;
  `git status --porcelain` there is empty and no commit on HEAD is missing from
  the upstream (no upstream configured denies); `gh pr list --head <branch>
  --state merged` returns a row; and the daemon reports no other live agent or
  managed session writing in that tree. Every check fails closed — a fact that
  cannot be established denies — and the denial names which one failed. The
  permitted name is read from `dispatch_isolation`'s
  `SHARED_CHECKOUT_PERMITTED_NAMES`, the same list ADR-0056's dispatch-time
  grant keys on. Scope is `remove` only: `add`, `move`, `lock` and `prune` are
  untouched, `rm -rf` on a worktree stays denied to every caller, and
  `tm session prune-worktrees --merged-prs --force` stays the default sweep.
  Every other subagent is denied exactly as before, and the PM path is
  unchanged. (ADR-0057)
