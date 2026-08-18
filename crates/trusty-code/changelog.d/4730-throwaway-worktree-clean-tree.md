Changed

- The bundled `git-workflow` skill now shows a throwaway worktree
  (`git worktree add .claude/worktrees/baseline-$$ origin/main`) as the way to
  get a temporary clean tree, and its "Stashing Work" section saves under a ref
  and restores by that ref rather than a bare `git stash` followed by a blind
  `pop`. Kept byte-identical to trusty-mpm's copy of the same skill (#4730).
