Fixed
- A daemon spawn refused because its `TM_CLAUDE_SANDBOX=1` opt-in could not be stored on the session record now also kills the tmux session it created and removes the worktree it reserved, so no bare shell is left for reconcile to re-adopt and resume unsandboxed (#12077).
