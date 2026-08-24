Fixed

- `find_project_root` no longer stops at a linked git worktree. Its marker check
  asked only whether `.git` existed, which is true for the pointer FILE git
  writes in a worktree as well as for a checkout's `.git` directory, so a
  worktree answered with its own path while its main checkout answered with the
  checkout — two project roots for one project, against ADR-0012 §1. A `.git`
  file is now followed through its `gitdir:` pointer and the `commondir` file in
  the admin directory it names, and the main checkout is returned. Callers that
  read or lazily WRITE `.trusty-tools/trusty-memory.yaml` from inside a worktree
  now reach the checkout's pin instead of creating a second one on the worktree
  branch. A submodule or `--separate-git-dir` checkout carries a `.git` file of
  the same shape but no `commondir`; those directories are still their own root,
  and any unreadable or unresolvable `.git` file leaves the previous answer
  unchanged. (#5888)
