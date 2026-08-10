Added

- `workspace_layout` — the one resolver for trusty-mpm's managed workspace root and session-worktree base name, so the four crates that hardcoded `~/trusty-mpm-projects` and `.worktrees` independently now read the same configured values (#5203, #5204). A configured worktree base is rejected back to `.worktrees` if it is not a single path component or collides with a reserved name (`worktrees`, `.git`, `.claude`, `.base`), which keeps Claude Code's `.claude/worktrees/` agent store outside trusty-mpm's ownership predicate.
