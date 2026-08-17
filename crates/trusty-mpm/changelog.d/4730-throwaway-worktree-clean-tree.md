Changed

- `tm-workflow`'s escape hatch for "I need a clean tree to run one command" is
  now a throwaway worktree
  (`git worktree add .claude/worktrees/baseline-$$ origin/main`) rather than
  stashing the main checkout and restoring it afterwards. The
  worktree needs no main checkout, and a command that dies partway through
  leaves every other tree as it was instead of stranding uncommitted work in a
  stash entry someone has to find (#4730).
- The bundled `git-workflow` skill gained the same throwaway-worktree recipe for
  getting a temporary clean tree, and its "Stashing Work" section now saves
  under a named ref and restores by that ref rather than a bare `git stash`
  followed by a blind `pop` (#4730).
