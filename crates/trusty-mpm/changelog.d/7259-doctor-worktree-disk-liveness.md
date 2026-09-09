Fixed
- `tm doctor`'s worktree-disk check no longer counts a tombstoned session's workspace as occupied. Both doctor call sites take their claims from `SessionManager::workspace_claims`, so a claim whose managed tmux name is absent from a successful `tmux list-sessions` stops hiding the orphaned disk beneath it; an unobservable tmux still leaves every claim live (#7259).
