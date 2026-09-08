Fixed

- `tm hook --pm-guard` now refuses an `EnterWorktree` call from a subagent
  already standing in a worktree, naming both the pinned tree and the target
  and pointing at the recovery. Previously the call was allowed, the harness
  reported success and moved the working directory, and its own isolation
  guard stayed pinned to the dispatch-time path — after which every command,
  a bare `pwd` included, was refused and `ExitWorktree` was refused too. That
  pin lives in the Claude Code binary and cannot be re-pointed from a hook, so
  the guard refuses the switch instead of letting a success wedge the agent.
  Re-entering the pinned tree stays allowed (it is the only recovery), the PM
  is untouched, and a target outside `.claude/worktrees/`/`.worktrees/` is
  refused rather than compared (#7172).
- New `core::project_aliases::worktree_root` resolves any path inside a
  worktree to the tree's own root, so "is this the same tree?" has one
  definition beside `is_worktree_path` and `main_checkout_root` (#7172).
