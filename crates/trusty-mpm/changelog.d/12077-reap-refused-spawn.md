Fixed
- A daemon spawn refused because its `TM_CLAUDE_SANDBOX=1` opt-in could not be stored on the session record now also kills the tmux session it created and removes the worktree it reserved, so no bare shell is left for reconcile to re-adopt and resume unsandboxed. When that teardown fails, tm still kills the proven pane and tombstones the record, removing no workspace (#12077).
- A refused spawn whose pane tm cannot prove its own leaves the pane running and now says `pane left running`, with the tmux session name, in the refusal text the spawn route returns (#12077).
