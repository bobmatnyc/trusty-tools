Fixed

- Auto-discovery excludes the configured worktree base, not just a hardcoded `.worktrees`, so session worktrees under a retargeted base are no longer indexed as duplicate content (#5204).
