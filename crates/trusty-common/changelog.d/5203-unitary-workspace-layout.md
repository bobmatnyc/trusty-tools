Added
`workspace_layout` — the one resolver for trusty-mpm's managed workspace root and session-worktree base name, so the four crates that hardcoded `~/trusty-mpm-projects` and `.worktrees` independently now read the same configured values (#5203, #5204).
