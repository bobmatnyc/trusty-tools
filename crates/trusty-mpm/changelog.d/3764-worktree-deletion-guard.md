Added

- `SessionManager::decommission` now refuses to remove a workspace when a
  DIFFERENT `Active` managed session also claims that exact directory (by
  `workspace_path` or `cwd`) — the #1744 cwd-collision shape that preceded
  the #3715 cross-session worktree-corruption incident. The new
  `workspace_guard::foreign_active_claim` check runs unconditionally, ahead
  of every disk mutation, and refuses with the new
  `ManagedError::ForeignActiveWorkspaceClaim` error (#3764).
- The `SessionStart` hook's ambiguous-cwd skip (2+ `Active` managed sessions
  sharing one cwd) now logs at `ERROR` instead of `WARN`, so it reaches
  `tm doctor` / `list_recent_errors` instead of passing almost silently
  (#3764).
