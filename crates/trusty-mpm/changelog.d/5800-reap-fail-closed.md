Fixed

- The agent-worktree reap no longer removes a worktree the harness can still resume an agent into (#5800). A `SubagentStop` reports a turn boundary rather than an agent's exit, so it no longer triggers a reap; a session's end, which does prove its agents are gone, is now the sole trigger.
- A finished delegation's registered worktree is protected from another agent's reap — a terminal status records that trusty-mpm watched the delegation end, not that the harness released the directory.
- A worktree whose agent is stamped on two directories is kept rather than guessed at, so an ambiguous ownership claim reclaims neither tree.
