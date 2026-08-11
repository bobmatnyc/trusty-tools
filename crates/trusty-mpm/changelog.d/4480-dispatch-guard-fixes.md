Fixed

- The shared-worktree dispatch deny now offers only the remedy that always
  works — re-dispatch with `isolation: "worktree"`. It no longer suggests waiting
  for the running agent: a crashed subagent that never emits `SubagentStop` holds
  its delegation for the full six-hour staleness window, so waiting could mean
  waiting forever (#4480).
